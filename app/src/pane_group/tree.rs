use crate::app_state;
use pathfinder_geometry::rect::RectF;
use pathfinder_geometry::vector::Vector2F;
use std::collections::HashSet;
use std::{fmt, iter, mem};
use mnemos_ui::elements::{
    ChildAnchor, Container, DispatchEventResult, Empty, OffsetPositioning, ParentAnchor,
    ParentOffsetBounds, PositionedElementAnchor, PositionedElementOffsetBounds, SavePosition,
    Stack,
};
use mnemos_ui::AppContext;
use mnemos_ui::{
    elements::{
        ConstrainedBox, Element, EventHandler, Flex, Hoverable, MouseStateHandle, ParentElement,
        Rect, Shrinkable,
    },
    platform::Cursor,
    EntityId, ViewContext,
};

use super::{ActivationReason, PaneGroup, PaneId};
use crate::pane_group::leaf_strip::{self, LeafStripContext};
use crate::pane_group::terminal_tab_strip::STRIP_HEIGHT;
use crate::pane_group::{get_minimum_pane_size, DraggedBorder, PaneGroupAction};
use crate::themes::theme::MnemosTheme;
use mnemos_core::features::FeatureFlag;

#[cfg(test)]
#[path = "tree_tests.rs"]
mod tests;

pub(in crate::pane_group) const DEFAULT_FLEX_VALUE: f32 = 1.0;
pub(in crate::pane_group) const DEFAULT_FLEX_SIZE: PaneFlex = PaneFlex(DEFAULT_FLEX_VALUE);

pub fn get_divider_thickness() -> f32 {
    if FeatureFlag::MinimalistUI.is_enabled() {
        1.0
    } else {
        2.0
    }
}

// Extra padding for the divider to make it easier to resize.
// This is added around each side of the divider. Only used
// when minimalist UI is enabled.
const DIVIDER_RESIZE_PADDING: f32 = 4.0;

/// Stable identifier for a terminal tab inside a [`PaneData`].
///
/// Used for click/close routing in the UI and for persistence round-trips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaneTabId(pub uuid::Uuid);

impl PaneTabId {
    pub fn new() -> Self {
        PaneTabId(uuid::Uuid::new_v4())
    }
}

impl Default for PaneTabId {
    fn default() -> Self {
        Self::new()
    }
}

/// A single browser-style terminal tab: an independent split-tree (`root`) plus a
/// stable id and an optional user-set title. The bulk of the tree logic operates on
/// the *active* tab's `root` via [`PaneData::active_root`] / [`PaneData::active_root_mut`].
pub struct PaneTab {
    pub root: PaneNode,
    /// User-set title; when `None` the UI derives a label from the active pane.
    pub title: Option<String>,
    pub id: PaneTabId,
    /// Last pane focused while this tab was active. Used to restore focus to the
    /// right pane when the user switches back to this tab. `None` falls back to the
    /// tab's first visible pane.
    last_focused: Option<PaneId>,
}

impl PaneTab {
    pub fn new(root: PaneNode) -> Self {
        Self {
            root,
            title: None,
            id: PaneTabId::new(),
            last_focused: None,
        }
    }

    /// Reconstruct a tab from persisted data (stable id + title).
    pub fn from_parts(root: PaneNode, id: PaneTabId, title: Option<String>) -> Self {
        Self {
            root,
            title,
            id,
            last_focused: None,
        }
    }
}

/// Tree for all of the split panes
///
/// The root of each project is a *tabbed container*: a list of independent
/// pane-trees ([`PaneTab`]s), with one active. External callers keep using the same
/// `PaneData` surface (`split`, `remove`, `render`, …) which routes through the
/// active tab, so single-root assumptions keep working.
///
/// `len` (total pane count) and `hidden_panes` are tracked globally because `PaneId`s
/// are unique across every tab.
///
/// Also has an option hidden pane id, if you ever want a pane
/// to remain in the tree but not be rendered, which is needed
/// for pane drag and dropping
pub struct PaneData {
    tabs: Vec<PaneTab>,
    active_tab: usize,
    len: usize,
    hidden_panes: Vec<HiddenPane>,
}

#[derive(Debug, Clone, Copy)]
pub struct HiddenPane {
    pub pane_id: PaneId,
    reason: HiddenPaneReason,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HiddenPaneReason {
    FromMove,
    FromJob,
    TemporaryReplacement(PaneId),

    // Pane was closed. We keep it around temporarily in case
    // the user wants to undo the close.
    Closed,

    // Pane is a child agent spawned by an orchestrator. It stays hidden
    // until the user explicitly reveals it from the status card.
    ChildAgent,
}

impl HiddenPane {
    pub fn from_move(pane_id: PaneId) -> Self {
        Self {
            pane_id,
            reason: HiddenPaneReason::FromMove,
        }
    }
    pub fn from_job(pane_id: PaneId) -> Self {
        Self {
            pane_id,
            reason: HiddenPaneReason::FromJob,
        }
    }
    pub fn from_temporary_replacement(pane_id: PaneId, replacement_pane_id: PaneId) -> Self {
        Self {
            pane_id,
            reason: HiddenPaneReason::TemporaryReplacement(replacement_pane_id),
        }
    }
    pub fn from_close(pane_id: PaneId) -> Self {
        Self {
            pane_id,
            reason: HiddenPaneReason::Closed,
        }
    }
    pub fn from_child_agent(pane_id: PaneId) -> Self {
        Self {
            pane_id,
            reason: HiddenPaneReason::ChildAgent,
        }
    }
}

/// A leaf in the pane tree holding one or more stacked panes (a TCB v1-style
/// "tab group"). Exactly one member — the *active* one — is rendered and
/// hit-testable; the other members stay alive (their contents remain in
/// `PaneGroup::pane_contents`) but are invisible. Inactive members are NOT
/// `hidden_panes` entries: invisibility is purely group state.
///
/// Invariants: `panes` is non-empty and `active < panes.len()`.
pub struct PaneLeafGroup {
    panes: Vec<PaneId>,
    active: usize,
}

impl PaneLeafGroup {
    /// A group with a single member (the common case — every plain leaf).
    pub fn single(pane_id: PaneId) -> Self {
        Self {
            panes: vec![pane_id],
            active: 0,
        }
    }

    /// The active (rendered/visible) member.
    pub fn active_pane(&self) -> PaneId {
        self.panes[self.active]
    }

    /// Index of the active member.
    pub fn active_index(&self) -> usize {
        self.active
    }

    /// Whether `pane_id` is a member (active or not).
    pub fn contains(&self, pane_id: PaneId) -> bool {
        self.panes.contains(&pane_id)
    }

    /// Number of members.
    pub fn len(&self) -> usize {
        self.panes.len()
    }

    /// All members, in strip order.
    pub fn members(&self) -> &[PaneId] {
        &self.panes
    }

    /// Members that are not currently hidden — i.e. the ones the strip would
    /// render. The strip (and its row height) only appears when this is `>= 2`,
    /// mirroring [`leaf_strip::wrap_leaf_group`].
    pub fn non_hidden_member_count(&self, hidden_panes: &[HiddenPane]) -> usize {
        self.panes
            .iter()
            .filter(|member| !hidden_panes.iter().any(|hidden| hidden.pane_id == **member))
            .count()
    }

    /// Append `pane_id` as a member and make it active (TCB v1 `moveTab`
    /// append+activate). Duplicate-member guard: returns `false` (no change)
    /// if the pane is already a member.
    pub fn push_member(&mut self, pane_id: PaneId) -> bool {
        if self.contains(pane_id) {
            return false;
        }
        self.panes.push(pane_id);
        self.active = self.panes.len() - 1;
        true
    }

    /// Remove the member `pane_id`, keeping the group non-empty. Returns
    /// `false` when the pane is not a member or is the *last* member — the
    /// containing branch must remove the whole leaf instead (existing
    /// `BranchRemoveResult::Collapse` path).
    ///
    /// Activation per TCB v1 `removeTab`: closing the active member activates the
    /// last remaining member; closing a background member keeps the current
    /// active member. `active` always stays `< len`.
    pub fn remove_member(&mut self, pane_id: PaneId) -> bool {
        if self.panes.len() <= 1 {
            return false;
        }
        let Some(idx) = self.panes.iter().position(|pane| *pane == pane_id) else {
            return false;
        };
        self.panes.remove(idx);
        if idx == self.active {
            // The active member was closed — activate the last remaining member.
            self.active = self.panes.len() - 1;
        } else if idx < self.active {
            // A member before the active one was closed; shift left to track it.
            self.active -= 1;
        }
        // Defensive clamp — the invariant `active < len` must always hold.
        self.active = self.active.min(self.panes.len() - 1);
        true
    }

    /// Make the member `pane_id` active. Returns `false` for a non-member.
    pub fn activate_member(&mut self, pane_id: PaneId) -> bool {
        if let Some(idx) = self.panes.iter().position(|pane| *pane == pane_id) {
            self.active = idx;
            true
        } else {
            false
        }
    }

    /// Like [`Self::activate_member`], but the boolean reports whether the
    /// active member actually *changed* — `false` when `pane_id` was already
    /// active or is not a member. Callers use this to `notify` only on a real,
    /// unrendered mutation.
    pub fn activate_member_changed(&mut self, pane_id: PaneId) -> bool {
        match self.panes.iter().position(|pane| *pane == pane_id) {
            Some(idx) if idx != self.active => {
                self.active = idx;
                true
            }
            _ => false,
        }
    }

    /// Replace the member `old` with `new` in place (activation unchanged).
    fn replace_member(&mut self, old: PaneId, new: PaneId) -> bool {
        if let Some(slot) = self.panes.iter_mut().find(|pane| **pane == old) {
            *slot = new;
            true
        } else {
            false
        }
    }
}

/// Single Node in the tree of panes
pub enum PaneNode {
    /// A collection of panes split in a specific direction
    Branch(PaneBranch),
    /// One or more stacked panes; only the group's active member is rendered
    Leaf(PaneLeafGroup),
}

#[derive(Debug)]
pub struct PaneFlex(pub f32);

impl Default for PaneFlex {
    fn default() -> Self {
        PaneFlex(DEFAULT_FLEX_VALUE)
    }
}

impl From<app_state::PaneFlex> for PaneFlex {
    fn from(pane_flex: app_state::PaneFlex) -> Self {
        PaneFlex(pane_flex.0)
    }
}

pub struct PaneBranch {
    axis: SplitDirection,
    pub nodes: Vec<(PaneFlex, PaneNode)>,
    dividers: Vec<Divider>,
}

/// The result of attempting to remove a pane from a branch
enum BranchRemoveResult {
    /// The pane was not found in this sub-tree
    NotFound,
    /// The pane was found and removed, no further action is needed
    Removed,
    /// The pane was found and removed, leaving only a single node in the branch, so it needs to
    /// be collapsed into the parent
    Collapse(PaneNode),
}

/// The result of attempting to find a pane with direction
#[derive(Debug, PartialEq)]
enum FindPaneByDirectionResult {
    /// Located the current pane in the tree.
    Located,
    /// The current pane is not found in the tree.
    NotFound,
    /// A list of possible target panes were found.
    Found(HashSet<PaneId>),
}

trait FindPaneByDirection {
    fn panes_by_direction(
        &self,
        content: PaneId,
        direction: Direction,
    ) -> FindPaneByDirectionResult;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    fn axis(&self) -> SplitDirection {
        match self {
            Direction::Left | Direction::Right => SplitDirection::Horizontal,
            Direction::Up | Direction::Down => SplitDirection::Vertical,
        }
    }
}

pub struct Divider {
    id: EntityId,
    mouse_state: MouseStateHandle,
}

impl Default for Divider {
    fn default() -> Self {
        Self::new()
    }
}

impl Divider {
    pub fn new() -> Self {
        Self {
            id: EntityId::new(),
            mouse_state: Default::default(),
        }
    }
}

impl PaneData {
    /// Create a new `PaneData` with a single tab whose root is a Leaf.
    pub fn new(pane_id: PaneId) -> Self {
        Self {
            tabs: vec![PaneTab::new(PaneNode::leaf(pane_id))],
            active_tab: 0,
            len: 1,
            hidden_panes: Vec::new(),
        }
    }

    // --- Tab layer accessors ---------------------------------------------------

    /// The active tab's root node (the split-tree currently filling the area).
    pub fn active_root(&self) -> &PaneNode {
        // Invariant: `active_tab` is always a valid index and `tabs` is never empty.
        &self.tabs[self.active_tab].root
    }

    /// Mutable access to the active tab's root node.
    pub fn active_root_mut(&mut self) -> &mut PaneNode {
        &mut self.tabs[self.active_tab].root
    }

    /// Rendered slot size of the active tab's root node. Threads `hidden_panes`
    /// so multi-member leaf groups include their strip-row height.
    pub fn root_pane_size(&self, ctx: &mut ViewContext<PaneGroup>) -> Vector2F {
        self.active_root().pane_size(&self.hidden_panes, ctx)
    }

    /// Number of terminal tabs.
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    /// Index of the currently active tab.
    pub fn active_tab_index(&self) -> usize {
        self.active_tab
    }

    /// Id of the currently active tab.
    pub fn active_tab_id(&self) -> PaneTabId {
        self.tabs[self.active_tab].id
    }

    /// Iterate over `(id, title)` for every tab, in order.
    pub fn tabs(&self) -> &[PaneTab] {
        &self.tabs
    }

    /// Find a tab's index by its stable id.
    pub fn tab_index_by_id(&self, id: PaneTabId) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == id)
    }

    /// Set the active tab by index, clamped to the valid range. Returns the id of
    /// the now-active tab so the caller can focus its pane.
    pub fn set_active_tab(&mut self, index: usize) -> PaneTabId {
        self.active_tab = index.min(self.tabs.len().saturating_sub(1));
        self.tabs[self.active_tab].id
    }

    /// Activate a tab by id. Returns `true` if the tab existed.
    pub fn activate_tab(&mut self, id: PaneTabId) -> bool {
        if let Some(idx) = self.tab_index_by_id(id) {
            self.active_tab = idx;
            true
        } else {
            false
        }
    }

    /// Set (or clear) the user title of the tab with the given id.
    pub fn set_tab_title(&mut self, id: PaneTabId, title: Option<String>) -> bool {
        if let Some(idx) = self.tab_index_by_id(id) {
            self.tabs[idx].title = title;
            true
        } else {
            false
        }
    }

    /// Push a new tab (built from a fresh leaf pane) and make it active.
    /// Returns the new tab's id.
    pub fn add_tab(&mut self, pane_id: PaneId) -> PaneTabId {
        let tab = PaneTab::new(PaneNode::leaf(pane_id));
        let id = tab.id;
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        self.len += 1;
        id
    }

    /// Append a pre-built tab (used when restoring from persistence). Does not
    /// change the active tab; `len` is recomputed by the caller via [`recompute_len`].
    pub fn push_tab(&mut self, tab: PaneTab) {
        self.tabs.push(tab);
    }

    /// Insert a brand-new pane (not yet in any tree) as a member of the leaf
    /// group owning `target`, and make it the group's active member. This is
    /// the Ctrl+T path: a "new tab" lands in the FOCUSED pane's group, not at
    /// the workspace level. `len` grows by one. Returns `false` (no change)
    /// when `target` is unknown — the caller falls back to another placement.
    pub fn add_pane_to_leaf_group(&mut self, new_pane: PaneId, target: PaneId) -> bool {
        let Some(group) = self
            .tabs
            .iter_mut()
            .find_map(|tab| tab.root.find_leaf_group_mut(target))
        else {
            return false;
        };
        if !group.push_member(new_pane) {
            return false;
        }
        self.len += 1;
        true
    }

    /// Build a single-tab `PaneData` wrapping an existing `root` node, with an
    /// optional tab title. Used when extracting a terminal tab into a standalone
    /// `PaneData` for detaching to a new window: the extracted [`PaneNode`] (and its
    /// live panes) are re-homed verbatim, not rebuilt. `len` is the node's pane
    /// count; no hidden panes carry over, and a fresh tab id is minted.
    pub fn from_single_tab(root: PaneNode, title: Option<String>) -> Self {
        let len = root.pane_ids().len();
        Self {
            tabs: vec![PaneTab::from_parts(root, PaneTabId::new(), title)],
            active_tab: 0,
            len,
            hidden_panes: Vec::new(),
        }
    }

    /// Build a `PaneData` directly from a list of restored tabs and an active index.
    /// `len` and `hidden_panes` are initialized fresh (`len` summed from the tabs,
    /// no hidden panes). Falls back to a single empty-leaf-free guarantee: if `tabs`
    /// is empty this panics in debug and otherwise is a programmer error — callers
    /// must pass at least one tab.
    pub fn from_tabs(tabs: Vec<PaneTab>, active_tab: usize) -> Self {
        debug_assert!(!tabs.is_empty(), "PaneData must always have >= 1 tab");
        let active_tab = active_tab.min(tabs.len().saturating_sub(1));
        let len = tabs.iter().map(|tab| tab.root.pane_ids().len()).sum();
        Self {
            tabs,
            active_tab,
            len,
            hidden_panes: Vec::new(),
        }
    }

    /// Recompute the total pane count across all tabs. Call after bulk tab edits
    /// (e.g. restore) where `len` wasn't maintained incrementally.
    pub fn recompute_len(&mut self) {
        self.len = self.tabs.iter().map(|tab| tab.root.pane_ids().len()).sum();
    }

    /// Remove the tab with the given id. Returns the ids of the panes that belonged
    /// to the removed tab (so the caller can tear down their contents) and the id of
    /// the tab that is active afterwards. Closing the last remaining tab is a no-op
    /// and returns `(vec![], current_active_id)`.
    pub fn remove_tab(&mut self, id: PaneTabId) -> (Vec<PaneId>, PaneTabId) {
        if self.tabs.len() <= 1 {
            // Never destroy the final tab — leaves a valid, non-empty container.
            return (Vec::new(), self.active_tab_id());
        }
        let Some(idx) = self.tab_index_by_id(id) else {
            return (Vec::new(), self.active_tab_id());
        };

        let removed = self.tabs.remove(idx);
        let removed_pane_ids = removed.root.pane_ids();
        self.len = self.len.saturating_sub(removed_pane_ids.len());

        // Keep `active_tab` pointing at a sensible neighbour.
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        } else if idx < self.active_tab {
            // A tab before the active one was removed; shift left to track it.
            self.active_tab -= 1;
        } else if idx == self.active_tab {
            // The active tab itself was removed; `active_tab` now points at what was
            // the next tab (already shifted into this slot), clamped above.
            self.active_tab = self.active_tab.min(self.tabs.len() - 1);
        }

        (removed_pane_ids, self.active_tab_id())
    }

    /// Move the tab with `id` to `new_index`, shifting the other tabs to make room.
    /// `new_index` is clamped to the valid range. The active tab follows the *moved*
    /// tab: after a successful reorder, the moved tab is the active one. Returns
    /// `true` if the tab existed and was moved (a no-op move — same slot — still
    /// returns `true`).
    pub fn reorder_tab(&mut self, id: PaneTabId, new_index: usize) -> bool {
        let Some(from) = self.tab_index_by_id(id) else {
            return false;
        };
        let to = new_index.min(self.tabs.len().saturating_sub(1));
        if from != to {
            let tab = self.tabs.remove(from);
            self.tabs.insert(to, tab);
        }
        // The moved tab is now the active one, regardless of what was active before.
        self.active_tab = to;
        true
    }

    /// Detach the tab with `id` from this container without tearing anything down:
    /// removes the tab and returns its `(root, pane_ids)`. Used to move a tab into a
    /// split (the panes are re-homed) or out to a new window (the panes are
    /// reconstructed there). Returns `None` for the last remaining tab (never leave
    /// an empty container) or an unknown id. `len` is decremented by the removed
    /// pane count and `active_tab` is kept on a valid neighbour exactly like
    /// [`PaneData::remove_tab`].
    pub fn take_tab(&mut self, id: PaneTabId) -> Option<(PaneNode, Vec<PaneId>)> {
        if self.tabs.len() <= 1 {
            return None;
        }
        let idx = self.tab_index_by_id(id)?;

        let removed = self.tabs.remove(idx);
        let removed_pane_ids = removed.root.pane_ids();
        self.len = self.len.saturating_sub(removed_pane_ids.len());

        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        } else if idx < self.active_tab {
            self.active_tab -= 1;
        } else if idx == self.active_tab {
            self.active_tab = self.active_tab.min(self.tabs.len() - 1);
        }

        Some((removed.root, removed_pane_ids))
    }

    /// Pull `pane_id` out of its owning tab's split-tree and re-home it as a new
    /// tab appended after the existing tabs (workspace-tab "park" path). Group
    /// aware: when the pane is a member of a multi-member leaf group, only the
    /// MEMBER is removed (the group's frame stays), not the whole leaf.
    /// The new tab becomes active. Returns `false` (no change) when the pane is
    /// unknown or is its tab's only pane — extracting it would leave that tab
    /// empty, and a lone pane already behaves like a tab of its own.
    ///
    /// The total pane count is unchanged: the pane leaves one tree and becomes
    /// the root of another, so `len` is not touched.
    pub fn extract_pane_to_new_tab(&mut self, pane_id: PaneId) -> bool {
        let Some(owner_idx) = self
            .tabs
            .iter()
            .position(|tab| tab.root.contains_pane(pane_id))
        else {
            return false;
        };
        if self.tabs[owner_idx].root.pane_ids().len() < 2 {
            return false;
        }
        if !self.tabs[owner_idx].root.remove(pane_id) {
            return false;
        }
        self.tabs.push(PaneTab::new(PaneNode::leaf(pane_id)));
        self.active_tab = self.tabs.len() - 1;
        true
    }

    /// Drag-pane-onto-pane center drop → leaf tab group: remove `source` from
    /// its split-tree (group-member removal when it's stacked, auto-collapse of
    /// its branch when it was a split sibling) and append it as a member of the
    /// leaf group that holds `target`, activating it (TCB v1 `moveTab`
    /// append+activate).
    ///
    /// Returns `false` (no change) when:
    /// - `source == target`, or they're already members of the same group
    ///   (duplicate-member guard),
    /// - either pane is unknown to this tree,
    /// - `source` is its tab's only pane (removing it would empty the tab).
    ///
    /// The total pane count is unchanged — the pane moves between leaves, so
    /// `len` is not touched.
    pub fn merge_pane_into_leaf_group(&mut self, source: PaneId, target: PaneId) -> bool {
        if source == target {
            return false;
        }
        // Duplicate-member guard: the target's group must exist and must not
        // already contain the source.
        let Some(target_group) = self
            .tabs
            .iter()
            .find_map(|tab| tab.root.find_leaf_group(target))
        else {
            return false;
        };
        if target_group.contains(source) {
            return false;
        }
        // The source must leave at least one pane behind in its tab.
        let Some(owner_idx) = self
            .tabs
            .iter()
            .position(|tab| tab.root.contains_pane(source))
        else {
            return false;
        };
        if self.tabs[owner_idx].root.pane_ids().len() < 2 {
            return false;
        }
        if !self.tabs[owner_idx].root.remove(source) {
            return false;
        }
        // Re-find the target group AFTER the removal — a branch collapse may
        // have moved the node that owns it.
        let Some(group) = self
            .tabs
            .iter_mut()
            .find_map(|tab| tab.root.find_leaf_group_mut(target))
        else {
            // Unreachable in practice (the target group is never the removed
            // node). Recover by re-homing the source next to itself rather than
            // leaking a live pane out of the tree.
            log::error!("merge_pane_into_leaf_group: target group vanished after removal");
            self.tabs[owner_idx].root.insert(source, Direction::Right);
            return false;
        };
        group.push_member(source);
        true
    }

    /// The active pane of the tab with `id`: its last-focused pane if still present,
    /// otherwise the first pane in that tab's tree. `None` for an unknown id.
    pub fn tab_active_pane(&self, id: PaneTabId) -> Option<PaneId> {
        let tab = self.tabs.iter().find(|tab| tab.id == id)?;
        // Focus targets must be visible: an inactive leaf-group member cannot
        // receive focus, so fall back to the first active member.
        match tab.last_focused {
            Some(pane_id) if tab.root.active_member_ids().contains(&pane_id) => Some(pane_id),
            _ => tab.root.active_member_ids().first().copied(),
        }
    }

    /// The pane ids that belong to the active tab's tree (visible + hidden).
    pub fn active_tab_pane_ids(&self) -> Vec<PaneId> {
        self.active_root().pane_ids()
    }

    /// Record that `pane_id` was focused. Stored on whichever tab owns the pane so
    /// switching back to a tab restores focus to the pane the user last used there.
    pub fn record_focused_pane(&mut self, pane_id: PaneId) {
        if let Some(tab) = self
            .tabs
            .iter_mut()
            .find(|tab| tab.root.contains_pane(pane_id))
        {
            tab.last_focused = Some(pane_id);
        }
    }

    /// The pane the active tab should focus when it becomes active: its remembered
    /// last-focused pane if that pane still exists, otherwise the first pane in the
    /// tab's tree. Returns `None` only for an (impossible) empty tree.
    pub fn active_tab_focus_target(&self) -> Option<PaneId> {
        let tab = &self.tabs[self.active_tab];
        // Same visibility rule as `tab_active_pane`: never focus an inactive
        // leaf-group member.
        match tab.last_focused {
            Some(id) if tab.root.active_member_ids().contains(&id) => Some(id),
            _ => tab.root.active_member_ids().first().copied(),
        }
    }

    /// Which tab (id) contains the given pane, if any.
    pub fn tab_id_for_pane(&self, pane_id: PaneId) -> Option<PaneTabId> {
        self.tabs
            .iter()
            .find(|tab| tab.root.contains_pane(pane_id))
            .map(|tab| tab.id)
    }

    /// Consume this `PaneData` and return the active tab's root. Used by the recursive
    /// tree-builders that construct a transient single-tab `PaneData` only to pull its
    /// root back out.
    pub fn into_single_root(mut self) -> PaneNode {
        self.tabs.swap_remove(self.active_tab).root
    }

    pub fn visible_pane_count(&self) -> usize {
        self.visible_pane_ids().len()
    }

    pub fn has_horizontal_split(&self) -> bool {
        // Reflects the visible layout, so only consider the active tab.
        self.active_root().has_horizontal_split(&self.hidden_panes)
    }

    pub fn num_hidden_panes(&self) -> usize {
        self.hidden_panes.len()
    }

    pub fn remove_hidden_pane(&mut self, pane_id: PaneId) {
        self.hidden_panes.retain(|pane| pane.pane_id != pane_id);
    }

    /// Create a new `PaneData` with a Branch as the root
    ///
    /// Note: If there is only a single top-level Node (e.g. from a manually edited launch
    /// configuration), then this will collapse that Node into the root of this `PaneData`
    pub fn new_branch(axis: SplitDirection, nodes: Vec<(PaneFlex, PaneNode)>, len: usize) -> Self {
        let root = if nodes.len() == 1 {
            let mut mutable_nodes = nodes;
            // Safety: We know there is exactly one node in the list
            mutable_nodes.pop().unwrap().1
        } else {
            let dividers = iter::repeat_with(Divider::new)
                .take(nodes.len() - 1)
                .collect();
            PaneNode::Branch(PaneBranch {
                axis,
                nodes,
                dividers,
            })
        };

        Self {
            tabs: vec![PaneTab::new(root)],
            active_tab: 0,
            len,
            hidden_panes: Vec::new(),
        }
    }

    pub fn move_pane(&mut self, id: PaneId, target_pane_id: PaneId, direction: Direction) -> bool {
        if id == target_pane_id {
            return false;
        }

        // If the given move would not result in the pane tree being mutated, just
        // return early. Only valid when `id` is its leaf's sole pane: a member of
        // a multi-member group POPS OUT of its group on a move, which mutates the
        // tree even when the destination slot is right next to the group — and
        // since dragging a strip tab activates it, the dragged member IS the
        // neighbouring leaf's pane identity, so this guard used to eat every
        // "drop a tab onto the adjacent pane's edge" gesture.
        let is_solo_leaf = self
            .leaf_group_members(id)
            .map_or(true, |members| members.len() < 2);
        if is_solo_leaf && self.sibling_by_direction(target_pane_id, direction) == Some(id) {
            return false;
        }

        // Remove the pane from the tree
        if !self.remove(id) {
            log::error!("Pane not found");
            return false;
        }

        // Call a new split to move the pane to the new location
        self.split(target_pane_id, id, direction)
    }

    pub fn hide_pane_for_move(&mut self, id: PaneId) {
        self.hidden_panes.push(HiddenPane::from_move(id));
    }

    pub fn clear_hidden_panes_from_move(&mut self) {
        self.hidden_panes
            .retain(|pane| pane.reason != HiddenPaneReason::FromMove);
    }

    pub fn hide_pane_for_job(&mut self, id: PaneId) {
        self.hidden_panes.push(HiddenPane::from_job(id));
        // Same handoff as `hide_closed_pane`: if the hidden pane was a
        // multi-member group's ACTIVE member, hand activation to a surviving
        // member so the group frame keeps rendering.
        self.activate_surviving_member(id);
    }

    pub fn show_pane_for_job(&mut self, id: PaneId) {
        if let Some(pos) = self
            .hidden_panes
            .iter()
            .position(|pane| pane.pane_id == id && pane.reason == HiddenPaneReason::FromJob)
        {
            self.hidden_panes.remove(pos);
            // Mirror `unhide_closed_pane`: a revealed member becomes its leaf
            // group's active (visible) member again.
            self.activate_leaf_group_member(id);
        } else {
            log::error!("Attempted to show pane for the job but couldn't find it.")
        }
    }

    pub fn hide_pane_for_child_agent(&mut self, id: PaneId) {
        if !self.is_pane_hidden(&id) {
            self.hidden_panes.push(HiddenPane::from_child_agent(id));
            // See `hide_pane_for_job`: keep a multi-member group's frame alive
            // by handing activation to a surviving member.
            self.activate_surviving_member(id);
        }
    }

    pub fn show_pane_for_child_agent(&mut self, id: PaneId) {
        if let Some(pos) = self
            .hidden_panes
            .iter()
            .position(|pane| pane.pane_id == id && pane.reason == HiddenPaneReason::ChildAgent)
        {
            self.hidden_panes.remove(pos);
            // Mirror `unhide_closed_pane`: reveal makes the member active again.
            self.activate_leaf_group_member(id);
        } else {
            log::error!("Attempted to show child agent pane but couldn't find it.")
        }
    }

    pub fn toggle_pane_visibility_for_job(&mut self, id: PaneId) -> bool {
        if pane_hidden_for_job(&self.hidden_panes, &id) {
            self.show_pane_for_job(id);
            true
        } else {
            self.hide_pane_for_job(id);
            false
        }
    }

    pub fn hide_closed_pane(&mut self, id: PaneId) {
        self.hidden_panes.push(HiddenPane::from_close(id));
        // If the pane was a multi-member leaf group's ACTIVE member, hand
        // activation to the last remaining non-hidden member so the group's
        // frame keeps rendering. The closed pane stays a (now inactive, now
        // hidden) member until the undo grace period expires and `remove`
        // strips the membership for good.
        self.activate_surviving_member(id);
    }

    /// After hiding `hidden_id`, make sure its leaf group (if any) has a
    /// visible active member: activation moves to the *last* remaining
    /// non-hidden member, mirroring [`PaneLeafGroup::remove_member`]'s
    /// active-close rule.
    fn activate_surviving_member(&mut self, hidden_id: PaneId) {
        let hidden_ids: Vec<PaneId> = self.hidden_panes.iter().map(|h| h.pane_id).collect();
        for tab in &mut self.tabs {
            if let Some(group) = tab.root.find_leaf_group_mut(hidden_id) {
                if group.len() > 1 && group.active_pane() == hidden_id {
                    let survivor = group
                        .members()
                        .iter()
                        .rev()
                        .copied()
                        .find(|member| *member != hidden_id && !hidden_ids.contains(member));
                    if let Some(survivor) = survivor {
                        group.activate_member(survivor);
                    }
                }
                return;
            }
        }
    }

    pub fn unhide_closed_pane(&mut self, id: PaneId) -> bool {
        if let Some(pos) = self.hidden_panes.iter().position(|pane| pane.pane_id == id) {
            self.hidden_panes.remove(pos);
            // A restored leaf-group member becomes the active member again —
            // an undone close must be visible immediately.
            self.activate_leaf_group_member(id);
            true
        } else {
            log::warn!(
                "Attempted to show pane {id} for undo close but couldn't find it in hidden panes"
            );
            false
        }
    }

    /// Make `pane_id` its leaf group's active (visible) member. Returns `true`
    /// when the pane is a member of some leaf group (including when it already
    /// was the active member) — i.e. it is now the visible member; `false` for
    /// a pane unknown to this tree.
    pub fn activate_leaf_group_member(&mut self, pane_id: PaneId) -> bool {
        for tab in &mut self.tabs {
            if let Some(group) = tab.root.find_leaf_group_mut(pane_id) {
                return group.activate_member(pane_id);
            }
        }
        false
    }

    /// Like [`Self::activate_leaf_group_member`], but returns `true` only when
    /// the active member actually changed (a real tree mutation the caller
    /// must render). `false` for an already-active member or a pane unknown to
    /// the tree.
    pub fn activate_leaf_group_member_changed(&mut self, pane_id: PaneId) -> bool {
        for tab in &mut self.tabs {
            if let Some(group) = tab.root.find_leaf_group_mut(pane_id) {
                return group.activate_member_changed(pane_id);
            }
        }
        false
    }

    /// The members (in strip order) of the leaf group `pane_id` belongs to,
    /// if any.
    pub fn leaf_group_members(&self, pane_id: PaneId) -> Option<&[PaneId]> {
        self.tabs
            .iter()
            .find_map(|tab| tab.root.find_leaf_group(pane_id))
            .map(|group| group.members())
    }

    /// The active (visible) member of the leaf group `pane_id` belongs to, if
    /// any. Used by the leaf-strip drag-out retarget to prefer the visible
    /// member when a member is dropped on its own group.
    pub fn leaf_group_active_pane(&self, pane_id: PaneId) -> Option<PaneId> {
        self.tabs
            .iter()
            .find_map(|tab| tab.root.find_leaf_group(pane_id))
            .map(|group| group.active_pane())
    }

    /// Members of every multi-member leaf group across all tabs, in tab/tree/
    /// member order. Used for strip mouse-state reconciliation and label
    /// lookup. Includes hidden members (state handles are cheap; the strip
    /// itself filters hidden members at render time).
    pub fn leaf_group_member_ids(&self) -> Vec<PaneId> {
        let mut ids = Vec::new();
        for tab in &self.tabs {
            tab.root.for_each_leaf_group(&mut |group| {
                if group.len() > 1 {
                    ids.extend(group.members().iter().copied());
                }
            });
        }
        ids
    }

    /// Panes that belong to a leaf group with more than one *non-hidden*
    /// member — panes whose header × must stay clickable even outside a split,
    /// because closing them reveals a stacked sibling instead of emptying the
    /// layout.
    pub fn panes_in_multi_member_groups(&self) -> HashSet<PaneId> {
        let mut set = HashSet::new();
        for tab in &self.tabs {
            tab.root.for_each_leaf_group(&mut |group| {
                let non_hidden: Vec<PaneId> = group
                    .members()
                    .iter()
                    .copied()
                    .filter(|member| !self.is_pane_hidden(member))
                    .collect();
                if non_hidden.len() > 1 {
                    set.extend(non_hidden);
                }
            });
        }
        set
    }

    pub fn get_closed_pane_ids(&self) -> Vec<PaneId> {
        self.hidden_panes
            .iter()
            .filter(|hidden| matches!(hidden.reason, HiddenPaneReason::Closed))
            .map(|hidden| hidden.pane_id)
            .collect()
    }

    pub fn clear_hidden_closed_panes(&mut self) {
        self.hidden_panes
            .retain(|pane| pane.reason != HiddenPaneReason::Closed);
    }

    pub fn is_temporary_replacement(&self, replacement_pane_id: PaneId) -> bool {
        self.original_pane_for_replacement(replacement_pane_id)
            .is_some()
    }

    pub fn original_pane_for_replacement(&self, replacement_pane_id: PaneId) -> Option<PaneId> {
        self.hidden_panes.iter().find_map(|hidden_pane| {
            matches!(hidden_pane.reason, HiddenPaneReason::TemporaryReplacement(id) if id == replacement_pane_id)
                .then_some(hidden_pane.pane_id)
        })
    }

    pub fn is_hidden_closed_pane(&self, pane_id: &PaneId) -> bool {
        self.hidden_panes
            .iter()
            .any(|hidden_pane| hidden_pane.pane_id == *pane_id)
    }

    /// Mutable reference to the root of whichever tab contains `pane_id`, falling
    /// back to the active tab when the pane isn't found (so splits anchored on the
    /// focused pane still work).
    fn root_for_pane_mut(&mut self, pane_id: PaneId) -> &mut PaneNode {
        let idx = self
            .tabs
            .iter()
            .position(|tab| tab.root.contains_pane(pane_id))
            .unwrap_or(self.active_tab);
        &mut self.tabs[idx].root
    }

    /// Whether any tab's tree contains `pane_id`.
    pub fn contains_pane(&self, pane_id: PaneId) -> bool {
        self.tabs.iter().any(|tab| tab.root.contains_pane(pane_id))
    }

    pub fn replace_pane(
        &mut self,
        original_pane_id: PaneId,
        replacement_pane_id: PaneId,
        is_temporary: bool,
    ) -> bool {
        // First, check if the original pane exists in any tab's tree
        if !self.contains_pane(original_pane_id) {
            return false;
        }

        // Hide the original pane for temporary replacement
        if is_temporary {
            self.hidden_panes
                .push(HiddenPane::from_temporary_replacement(
                    original_pane_id,
                    replacement_pane_id,
                ));
        }

        // Replace the original pane with the replacement pane in its tab's tree
        let success = self
            .root_for_pane_mut(original_pane_id)
            .replace_pane(original_pane_id, replacement_pane_id);

        if success {
            return true;
        } else if is_temporary {
            // If our pane replacement failed, remove the newly added pane from the hidden panes list
            self.hidden_panes.pop();
        }
        false
    }

    pub fn revert_temporary_replacement(&mut self, replacement_pane_id: PaneId) -> Option<PaneId> {
        // Find and remove the hidden pane that corresponds to this replacement
        if let Some(position) = self.hidden_panes.iter().position(|hidden_pane| {
            matches!(hidden_pane.reason, HiddenPaneReason::TemporaryReplacement(id) if id == replacement_pane_id)
        }) {
            let hidden_pane = self.hidden_panes.remove(position);
            let original_pane_id = hidden_pane.pane_id;

            // Replace the replacement pane with the original pane in its tab's tree
            if self
                .root_for_pane_mut(replacement_pane_id)
                .replace_pane(replacement_pane_id, original_pane_id)
            {
                Some(original_pane_id)
            } else {
                // If replacement failed, re-add the hidden pane entry
                self.hidden_panes.insert(position, hidden_pane);
                None
            }
        } else {
            None
        }
    }

    pub fn split(&mut self, old_id: PaneId, new_id: PaneId, direction: Direction) -> bool {
        let successful_split = self.root_for_pane_mut(old_id).split(old_id, new_id, direction);

        if successful_split {
            self.len += 1;
        }

        successful_split
    }

    /// Split the root of the active tab's pane tree, inserting `new_id` according to
    /// the given direction.
    pub fn split_root(&mut self, new_id: PaneId, direction: Direction) {
        self.active_root_mut().insert(new_id, direction);
        self.len += 1;
    }

    pub fn remove(&mut self, content: PaneId) -> bool {
        let successful_remove = self.root_for_pane_mut(content).remove(content);

        if successful_remove {
            self.len = self.len.saturating_sub(1);
        }

        successful_remove
    }

    /// Get the child panes across all tabs in an array sorted from left to right,
    /// up to down (active-tab order first is not guaranteed; tabs are concatenated
    /// in tab order). All terminal tabs' panes are included so external bookkeeping
    /// (content retention, enumeration) keeps every terminal alive.
    pub fn pane_ids(&self) -> Vec<PaneId> {
        self.tabs
            .iter()
            .flat_map(|tab| tab.root.pane_ids())
            .collect()
    }

    /// Get only the visible child panes across all tabs.
    /// This filters out inactive leaf-group members (only a group's active
    /// member is visible) and panes that are hidden for any reason
    /// (move, job, close, etc.).
    pub fn visible_pane_ids(&self) -> Vec<PaneId> {
        self.tabs
            .iter()
            .flat_map(|tab| tab.root.active_member_ids())
            .filter(|pane_id| !self.is_pane_hidden(pane_id))
            .collect()
    }

    /// Visible pane count for the *active tab only* — used to decide whether the
    /// currently displayed layout is split (vs. tabs/leaf groups, which don't
    /// count as a split).
    pub fn active_tab_visible_count(&self) -> usize {
        self.active_root()
            .active_member_ids()
            .into_iter()
            .filter(|pane_id| !self.is_pane_hidden(pane_id))
            .count()
    }

    /// Returns true if the given pane is hidden for any reason.
    pub fn is_pane_hidden(&self, pane_id: &PaneId) -> bool {
        self.hidden_panes
            .iter()
            .any(|hidden_pane| hidden_pane.pane_id == *pane_id)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Render the active tab's tree. (The workspace-level tab strip is rendered by
    /// `PaneGroup`, not here; per-pane leaf-group strips render in the leaf arms
    /// using the passed [`LeafStripContext`].)
    pub fn render(
        &self,
        theme: &MnemosTheme,
        strip: &LeafStripContext<'_>,
        app: &AppContext,
    ) -> Box<dyn Element> {
        match self.active_root() {
            PaneNode::Leaf(group) => {
                let content = group.active_pane().render(app);
                leaf_strip::wrap_leaf_group(group, &self.hidden_panes, strip, content, app)
            }
            PaneNode::Branch(node) => node.render(theme, &self.hidden_panes, strip, app),
        }
    }

    pub fn adjust_pane_size(
        &mut self,
        border_id: EntityId,
        delta: f32,
        ctx: &mut ViewContext<PaneGroup>,
    ) {
        // Border drags happen within the visible (active) tab's layout.
        // Split the borrow so pane-size measurement can see `hidden_panes`
        // while the active root is mutated.
        let Self {
            tabs,
            active_tab,
            hidden_panes,
            ..
        } = self;
        tabs[*active_tab]
            .root
            .adjust_pane_size(hidden_panes, border_id, delta, ctx);
    }

    pub fn adjust_pane_size_by_id(
        &mut self,
        pane_id: PaneId,
        direction: SplitDirection,
        delta: f32,
        ctx: &mut ViewContext<PaneGroup>,
    ) {
        // Clone the (usually empty) hidden set so pane-size measurement can read
        // it while the active root is mutated.
        let hidden_panes = self.hidden_panes.clone();
        self.root_for_pane_mut(pane_id)
            .adjust_pane_size_by_id(&hidden_panes, pane_id, direction, delta, ctx);
    }

    pub fn panes_by_direction(
        &self,
        pane_id: PaneId,
        direction: Direction,
        ctx: &ViewContext<PaneGroup>,
    ) -> Vec<PaneId> {
        // Find the panes from the current pane in the given direction.
        // Due to uneven splits, we may have multiple panes in the same direction.
        // Detect which ones are touching the current pane by checking the boundaries from the view context.

        let root_for_pane = self
            .tabs
            .iter()
            .find(|tab| tab.root.contains_pane(pane_id))
            .map(|tab| &tab.root)
            .unwrap_or_else(|| self.active_root());

        if let FindPaneByDirectionResult::Found(ids) =
            root_for_pane.panes_by_direction(pane_id, direction)
        {
            if let Some(current_rect) = ctx.element_position_by_id(pane_id.position_id()) {
                ids.into_iter()
                    .filter(|id| {
                        match ctx.element_position_by_id(id.position_id()) {
                            Some(candidate_rect) => PaneData::are_rects_overlapping(
                                &current_rect,
                                &candidate_rect,
                                direction.axis(),
                            ),
                            None => true, // If we can't find the position, we assume it's overlapping
                        }
                    })
                    .collect()
            } else {
                Vec::from_iter(ids)
            }
        } else {
            // We didn't find any panes in that direction, return an empty list
            Vec::new()
        }
    }

    fn are_rects_overlapping(rect1: &RectF, rect2: &RectF, axis: SplitDirection) -> bool {
        // Returns true if the two rectangles overlap in the given axis.
        //
        //                 ---------
        //  -----------    | rect2 |
        // | rect1     |   ---------
        // |           |
        //  -----------
        //
        // In this case, the function would return true for SplitDirection::Horizontal.
        // It would return false for SplitDirection::Vertical.
        match axis {
            SplitDirection::Horizontal => {
                !(rect1.max_y() <= rect2.min_y() || rect1.min_y() >= rect2.max_y())
            }
            SplitDirection::Vertical => {
                !(rect1.max_x() <= rect2.min_x() || rect1.min_x() >= rect2.max_x())
            }
        }
    }

    // Find a pane from the given pane in the given direction, but only if it is a direct sibling
    //  of the given pane.  This means they are direct children of the same branch.
    fn sibling_by_direction(&self, pane_id: PaneId, direction: Direction) -> Option<PaneId> {
        let root_for_pane = self
            .tabs
            .iter()
            .find(|tab| tab.root.contains_pane(pane_id))
            .map(|tab| &tab.root)
            .unwrap_or_else(|| self.active_root());
        match root_for_pane {
            PaneNode::Branch(b) => b.sibling_by_direction(pane_id, direction),
            _ => None,
        }
    }
}

impl PaneNode {
    /// Convenience constructor for the (overwhelmingly common) single-member leaf.
    pub fn leaf(pane_id: PaneId) -> Self {
        PaneNode::Leaf(PaneLeafGroup::single(pane_id))
    }

    /// The pane ids that can currently be visible: the *active member* of every
    /// leaf group in this sub-tree. Inactive members are excluded — they are
    /// neither rendered nor `hidden_panes` entries.
    pub fn active_member_ids(&self) -> Vec<PaneId> {
        match self {
            PaneNode::Leaf(group) => vec![group.active_pane()],
            PaneNode::Branch(branch) => branch
                .nodes
                .iter()
                .flat_map(|(_, node)| node.active_member_ids())
                .collect(),
        }
    }

    /// The leaf group that has `pane_id` as a member (active or not), if any.
    fn find_leaf_group(&self, pane_id: PaneId) -> Option<&PaneLeafGroup> {
        match self {
            PaneNode::Leaf(group) => group.contains(pane_id).then_some(group),
            PaneNode::Branch(branch) => branch
                .nodes
                .iter()
                .find_map(|(_, node)| node.find_leaf_group(pane_id)),
        }
    }

    /// Visit every leaf group in this sub-tree, in tree order.
    fn for_each_leaf_group<'a, F: FnMut(&'a PaneLeafGroup)>(&'a self, f: &mut F) {
        match self {
            PaneNode::Leaf(group) => f(group),
            PaneNode::Branch(branch) => {
                for (_, node) in &branch.nodes {
                    node.for_each_leaf_group(f);
                }
            }
        }
    }

    /// Mutable access to the leaf group that has `pane_id` as a member.
    fn find_leaf_group_mut(&mut self, pane_id: PaneId) -> Option<&mut PaneLeafGroup> {
        match self {
            PaneNode::Leaf(group) => {
                if group.contains(pane_id) {
                    Some(group)
                } else {
                    None
                }
            }
            PaneNode::Branch(branch) => branch
                .nodes
                .iter_mut()
                .find_map(|(_, node)| node.find_leaf_group_mut(pane_id)),
        }
    }

    fn has_visible_children(&self, hidden_panes: &[HiddenPane]) -> bool {
        match self {
            // Only a leaf group's active member counts — inactive members are
            // invisible by group state, independent of `hidden_panes`.
            PaneNode::Leaf(group) => {
                let pane_id = &group.active_pane();
                !pane_hidden_for_job(hidden_panes, pane_id)
                    && !pane_hidden_for_undo(hidden_panes, pane_id)
                    && !pane_hidden_for_move(hidden_panes, pane_id)
                    && !pane_hidden_for_child_agent(hidden_panes, pane_id)
            }
            PaneNode::Branch(branch) => branch.has_visible_children(hidden_panes),
        }
    }

    fn has_children_hidden_for_move(&self, hidden_panes: &[HiddenPane]) -> bool {
        match self {
            PaneNode::Leaf(group) => pane_hidden_for_move(hidden_panes, &group.active_pane()),
            PaneNode::Branch(branch) => branch.has_children_hidden_for_move(hidden_panes),
        }
    }

    pub fn has_horizontal_split(&self, hidden_panes: &[HiddenPane]) -> bool {
        match self {
            PaneNode::Leaf(_) => false,
            PaneNode::Branch(branch) => {
                let mut visible_or_move_children = 0usize;
                let mut any_child_split = false;

                for (_, child) in &branch.nodes {
                    if !child.has_visible_children(hidden_panes)
                        && !child.has_children_hidden_for_move(hidden_panes)
                    {
                        continue;
                    }

                    visible_or_move_children += 1;

                    if child.has_horizontal_split(hidden_panes) {
                        any_child_split = true;
                    }
                }

                let self_has_split =
                    branch.axis == SplitDirection::Horizontal && visible_or_move_children > 1;

                self_has_split || any_child_split
            }
        }
    }

    fn split(&mut self, old_pane_id: PaneId, new_pane_id: PaneId, direction: Direction) -> bool {
        match self {
            // Splits match on group *membership*: splitting any member moves the
            // whole group into the new branch (the group is one visual frame).
            PaneNode::Leaf(group) => {
                if group.contains(old_pane_id) {
                    let old_node = mem::replace(self, PaneNode::leaf(new_pane_id));
                    *self = PaneNode::Branch(PaneBranch::new(
                        old_node,
                        PaneNode::leaf(new_pane_id),
                        direction,
                    ));
                    true
                } else {
                    false
                }
            }
            PaneNode::Branch(branch) => branch.split(old_pane_id, new_pane_id, direction),
        }
    }

    /// Number of splits at the node in the given axis. For leaf nodes, this is always one.
    pub fn num_splits_in_direction(&self, axis: SplitDirection) -> usize {
        match self {
            PaneNode::Branch(branch) if branch.axis == axis => branch.nodes.len(),
            _ => 1,
        }
    }

    fn remove(&mut self, pane_id: PaneId) -> bool {
        match self {
            // A multi-member group removes the member in place (TCB v1 `removeTab`);
            // a single-member leaf can only be removed by the containing branch.
            PaneNode::Leaf(group) => group.remove_member(pane_id),
            PaneNode::Branch(branch) => match branch.remove(pane_id) {
                BranchRemoveResult::NotFound => false,
                BranchRemoveResult::Removed => true,
                BranchRemoveResult::Collapse(last_node) => {
                    *self = last_node;
                    true
                }
            },
        }
    }

    fn insert(&mut self, new_pane_id: PaneId, direction: Direction) {
        match self {
            PaneNode::Leaf(_) => {
                // The whole group moves into the new branch unchanged.
                let old_node = mem::replace(self, PaneNode::leaf(new_pane_id));
                *self = PaneNode::Branch(PaneBranch::new(
                    old_node,
                    PaneNode::leaf(new_pane_id),
                    direction,
                ));
            }
            PaneNode::Branch(branch) => branch.insert(new_pane_id, direction),
        }
    }

    /// Every pane in this sub-tree, *including* inactive leaf-group members —
    /// this is the "alive" set used for bookkeeping/teardown, not visibility.
    pub fn pane_ids(&self) -> Vec<PaneId> {
        match self {
            PaneNode::Leaf(group) => group.members().to_vec(),
            PaneNode::Branch(branch) => branch.get_children(),
        }
    }

    fn render(
        &self,
        theme: &MnemosTheme,
        hidden_panes: &Vec<HiddenPane>,
        strip: &LeafStripContext<'_>,
        app: &AppContext,
    ) -> Box<dyn Element> {
        match self {
            PaneNode::Leaf(group) => {
                let view = group.active_pane();
                let content = EventHandler::new(view.render(app))
                    .on_left_mouse_down(move |ctx, _, _| {
                        ctx.dispatch_typed_action(PaneGroupAction::Activate(
                            view,
                            ActivationReason::Click,
                        ));
                        DispatchEventResult::StopPropagation
                    })
                    .finish();
                // Multi-member groups get the per-pane tab strip above the
                // active member; single leaves render unchanged.
                leaf_strip::wrap_leaf_group(group, hidden_panes, strip, content, app)
            }
            PaneNode::Branch(branch) => branch.render(theme, hidden_panes, strip, app),
        }
    }

    pub fn pane_size(
        &self,
        hidden_panes: &[HiddenPane],
        ctx: &mut ViewContext<PaneGroup>,
    ) -> Vector2F {
        match self {
            // Only the active member is laid out, so its rect is the leaf's rect.
            PaneNode::Leaf(group) => {
                let size = ctx
                    .element_position_by_id(group.active_pane().position_id())
                    .map_or(Vector2F::zero(), |rect| rect.size());
                // A multi-member group renders a strip row above the active
                // member (see `leaf_strip::wrap_leaf_group`), so the active
                // member's rect is short by `STRIP_HEIGHT`; add it back so the
                // leaf's slot extent is measured consistently with branches.
                if group.non_hidden_member_count(hidden_panes) >= 2 {
                    Vector2F::new(size.x(), size.y() + STRIP_HEIGHT)
                } else {
                    size
                }
            }
            PaneNode::Branch(branch) => branch.size(hidden_panes, ctx),
        }
    }

    pub fn adjust_pane_size(
        &mut self,
        hidden_panes: &[HiddenPane],
        border_id: EntityId,
        delta: f32,
        ctx: &mut ViewContext<PaneGroup>,
    ) -> bool {
        match self {
            PaneNode::Leaf(_) => false,
            PaneNode::Branch(branch) => {
                branch.adjust_pane_size(hidden_panes, border_id, delta, ctx)
            }
        }
    }

    /// The boolean value returned here indicates whether a resizing needs to
    /// be handled at a parent branch. For a leaf node, if the pane's id does not match,
    /// we returns false as its parent branch does not need to handle the resize.
    /// If it does match, we returns true so its parent branch will handle it.
    /// For a branch node, if the direction we are resizing does not match the branch
    /// axis, it will return true so a parent branch that does match will handle the
    /// resize.
    pub fn adjust_pane_size_by_id(
        &mut self,
        hidden_panes: &[HiddenPane],
        pane_id: PaneId,
        direction: SplitDirection,
        delta: f32,
        ctx: &mut ViewContext<PaneGroup>,
    ) -> bool {
        match self {
            // Resizes act on the visible layout — match the active member only.
            PaneNode::Leaf(group) => group.active_pane() == pane_id,
            PaneNode::Branch(branch) => {
                branch.adjust_pane_size_by_id(hidden_panes, pane_id, direction, delta, ctx)
            }
        }
    }

    /// Find the first panes in the given direction inside of this pane.
    fn first_panes_in_direction(&self, direction: Direction) -> HashSet<PaneId> {
        match self {
            // If this is a leaf, then this is the first pane from any direction.
            // Directional nav only ever targets the visible (active) member.
            PaneNode::Leaf(group) => HashSet::from([group.active_pane()]),
            PaneNode::Branch(branch) => {
                // If the direction matches the split axis, then we only search the first sub-tree in the given direction.
                //  --------------------     The first panes from the left are 1 and 3.
                //  |   1     |    2   |     The first panes from the right are 2 and 3.
                //  --------------------     For these cases we must search both sub-trees.
                //  |        3         |
                //  --------------------     The first pane from down is 3.  We only need to search the first sub-tree.
                if branch.axis() == direction.axis() {
                    match direction {
                        Direction::Left | Direction::Up => branch
                            .nodes
                            .last()
                            .expect("PaneGroup has no nodes when moving focus.")
                            .1
                            .first_panes_in_direction(direction),
                        Direction::Right | Direction::Down => branch
                            .nodes
                            .first()
                            .expect("PaneBranch has no nodes when moving focus.")
                            .1
                            .first_panes_in_direction(direction),
                    }
                } else {
                    branch
                        .nodes
                        .iter()
                        .flat_map(|(_, node)| node.first_panes_in_direction(direction))
                        .collect()
                }
            }
        }
    }

    #[cfg(test)]
    fn as_branch(&self) -> Option<&PaneBranch> {
        match self {
            PaneNode::Branch(branch) => Some(branch),
            PaneNode::Leaf(_) => None,
        }
    }

    /// Test helper: the *active member* of a leaf node (`None` for a branch).
    #[cfg(test)]
    fn as_leaf(&self) -> Option<PaneId> {
        match self {
            PaneNode::Leaf(group) => Some(group.active_pane()),
            PaneNode::Branch(_) => None,
        }
    }

    /// Test helper: the full leaf group (`None` for a branch).
    #[cfg(test)]
    fn as_leaf_group(&self) -> Option<&PaneLeafGroup> {
        match self {
            PaneNode::Leaf(group) => Some(group),
            PaneNode::Branch(_) => None,
        }
    }

    /// Sum this [`PaneNode`]s [`PaneFlex`] values along the given `axis`. Return the
    /// [`DEFAULT_FLEX_SIZE`] if this [`PaneNode`] isn't a [`PaneNode::Branch`] in the given
    /// [`SplitDirection`] (or it is a [`PaneNode::Leaf`]).
    pub(in crate::pane_group) fn pane_flex_sum_along_axis(&self, axis: SplitDirection) -> f32 {
        match self {
            PaneNode::Branch(pane_branch) if pane_branch.axis == axis => pane_branch
                .nodes
                .iter()
                .fold(0., |sum, (pane_flex, _)| sum + pane_flex.0),
            _ => DEFAULT_FLEX_VALUE,
        }
    }

    /// Whether `pane_id` exists anywhere in this sub-tree, including as an
    /// *inactive* leaf-group member (membership, not visibility).
    fn contains_pane(&self, pane_id: PaneId) -> bool {
        match self {
            PaneNode::Leaf(group) => group.contains(pane_id),
            PaneNode::Branch(branch) => branch.contains_pane(pane_id),
        }
    }

    fn replace_pane(&mut self, old_pane_id: PaneId, new_pane_id: PaneId) -> bool {
        match self {
            PaneNode::Leaf(group) => group.replace_member(old_pane_id, new_pane_id),
            PaneNode::Branch(branch) => branch.replace_pane(old_pane_id, new_pane_id),
        }
    }
}

impl FindPaneByDirection for PaneNode {
    fn panes_by_direction(
        &self,
        pane_id: PaneId,
        direction: Direction,
    ) -> FindPaneByDirectionResult {
        match self {
            // Directional nav operates on the visible layout: only the active
            // member locates a leaf; inactive members are not reachable targets.
            PaneNode::Leaf(group) => {
                if group.active_pane() == pane_id {
                    FindPaneByDirectionResult::Located
                } else {
                    FindPaneByDirectionResult::NotFound
                }
            }
            PaneNode::Branch(branch) => branch.panes_by_direction(pane_id, direction),
        }
    }
}

impl PaneBranch {
    fn new(old_pane: PaneNode, new_pane: PaneNode, direction: Direction) -> Self {
        let axis = direction.axis();
        PaneBranch {
            axis,
            nodes: match direction {
                Direction::Left | Direction::Up => {
                    vec![(DEFAULT_FLEX_SIZE, new_pane), (DEFAULT_FLEX_SIZE, old_pane)]
                }
                Direction::Right | Direction::Down => {
                    vec![(DEFAULT_FLEX_SIZE, old_pane), (DEFAULT_FLEX_SIZE, new_pane)]
                }
            },
            dividers: vec![Divider::new()],
        }
    }

    fn split(&mut self, old_pane: PaneId, new_pane: PaneId, direction: Direction) -> bool {
        // First pass: recurse into branch children, or locate the leaf group that
        // has `old_pane` as a member. (Two-phase so the group node can be moved
        // wholesale into a perpendicular branch below.)
        let mut leaf_idx = None;
        for (idx, (_, node)) in self.nodes.iter_mut().enumerate() {
            match node {
                PaneNode::Branch(branch) => {
                    if branch.split(old_pane, new_pane, direction) {
                        return true;
                    }
                }
                PaneNode::Leaf(group) => {
                    if group.contains(old_pane) {
                        leaf_idx = Some(idx);
                        break;
                    }
                }
            }
        }
        let Some(idx) = leaf_idx else {
            return false;
        };

        // If the split comes in the same direction as the previous splits
        // on this sub-tree, we can insert the new pane into the nodes directly
        if direction.axis() == self.axis {
            self.nodes.insert(
                match direction {
                    Direction::Left | Direction::Up => idx,
                    Direction::Right | Direction::Down => idx + 1,
                },
                (DEFAULT_FLEX_SIZE, PaneNode::leaf(new_pane)),
            );
            self.dividers.insert(idx, Divider::new());
        } else {
            // Otherwise, split the current leaf into a perpendicular branch. The
            // whole leaf group moves — its members stay stacked in one frame.
            let node = &mut self.nodes[idx].1;
            let old_node = mem::replace(node, PaneNode::leaf(new_pane));
            *node = PaneNode::Branch(PaneBranch::new(
                old_node,
                PaneNode::leaf(new_pane),
                direction,
            ));
        }
        true
    }

    /// Inserts `new_pane_id` into this branch at either the start or the end, according to the
    /// [`Direction`]. If the direction axis does not match that of this branch, the branch is
    /// re-split in place.
    fn insert(&mut self, new_pane_id: PaneId, direction: Direction) {
        if direction.axis() == self.axis {
            match direction {
                Direction::Left | Direction::Up => {
                    self.nodes
                        .insert(0, (DEFAULT_FLEX_SIZE, PaneNode::leaf(new_pane_id)));
                    self.dividers.insert(0, Divider::new());
                }
                Direction::Right | Direction::Down => {
                    self.nodes
                        .push((DEFAULT_FLEX_SIZE, PaneNode::leaf(new_pane_id)));
                    self.dividers.push(Divider::new());
                }
            }
        } else {
            // If the axes don't match, split this branch in place.
            let nodes = mem::take(&mut self.nodes);
            let dividers = mem::take(&mut self.dividers);
            let axis = self.axis;
            *self = PaneBranch::new(
                PaneNode::Branch(PaneBranch {
                    nodes,
                    dividers,
                    axis,
                }),
                PaneNode::leaf(new_pane_id),
                direction,
            );
        }
    }

    fn remove(&mut self, pane_id_to_remove: PaneId) -> BranchRemoveResult {
        // First pass: recurse into branches / remove a member from a multi-member
        // group in place. A single-member leaf is only located here; the node
        // removal (and possible collapse) happens below, outside the iteration.
        let mut leaf_idx = None;
        for (idx, (_, node)) in self.nodes.iter_mut().enumerate() {
            match node {
                PaneNode::Branch(_) => {
                    if node.remove(pane_id_to_remove) {
                        return BranchRemoveResult::Removed;
                    }
                }
                PaneNode::Leaf(group) => {
                    if group.contains(pane_id_to_remove) {
                        if group.len() > 1 {
                            // TCB v1 `removeTab`: drop the member; the group's
                            // remaining member(s) keep the frame. Activation is
                            // handled by `remove_member`.
                            group.remove_member(pane_id_to_remove);
                            return BranchRemoveResult::Removed;
                        }
                        // Last member — remove the whole leaf node (auto-collapse
                        // path below, exactly as before).
                        leaf_idx = Some(idx);
                        break;
                    }
                }
            }
        }

        let Some(idx) = leaf_idx else {
            return BranchRemoveResult::NotFound;
        };

        self.nodes.remove(idx);
        if self.dividers.is_empty() {
            log::error!("Attempted to remove a pane when there are no dividers!");
        } else {
            self.dividers.remove(idx.min(self.dividers.len() - 1));
        }
        if self.nodes.len() == 1 {
            // Safety: We know that there is an element in `self.nodes`
            BranchRemoveResult::Collapse(self.nodes.pop().unwrap().1)
        } else {
            BranchRemoveResult::Removed
        }
    }

    fn get_children(&self) -> Vec<PaneId> {
        let mut res = vec![];
        for (_, member) in &self.nodes {
            match member {
                PaneNode::Branch(branch) => res.extend(branch.get_children()),
                PaneNode::Leaf(group) => res.extend(group.members().iter().copied()),
            }
        }
        res
    }

    /// Returns the (active members of) leaf panes that are direct children of this branch.
    #[cfg(test)]
    fn direct_children(&self) -> Vec<PaneId> {
        self.nodes
            .iter()
            .filter_map(|(_, node)| match node {
                PaneNode::Leaf(group) => Some(group.active_pane()),
                PaneNode::Branch(_) => None,
            })
            .collect()
    }

    /// Returns a reference to the child node at `index`, panicking if it's out of bounds.
    #[cfg(test)]
    fn node(&self, index: usize) -> &PaneNode {
        let (_, node) = &self.nodes[index];
        node
    }

    fn render(
        &self,
        theme: &MnemosTheme,
        hidden_panes: &Vec<HiddenPane>,
        strip: &LeafStripContext<'_>,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let mut parent = match self.axis {
            SplitDirection::Horizontal => Flex::row(),
            SplitDirection::Vertical => Flex::column(),
        };

        // Iterate through all the panes, skipping nodes that have no visible children
        // except when children are hidden for move operations (we need empty drop targets)
        let mut dividers = self.dividers.iter();

        // Collect divider positions to render them as positioned elements later.
        let mut divider_positions = Vec::new();

        for (flex, node) in self.nodes.iter() {
            // Skip nodes that have no visible children, but preserve nodes with children
            // hidden for move operations as they serve as drop targets
            if !node.has_visible_children(hidden_panes)
                && !node.has_children_hidden_for_move(hidden_panes)
            {
                continue;
            }
            let mut flex_value = flex.0;
            if let PaneNode::Leaf(group) = node {
                // If the pane is hidden for a move, render a divider, but set the
                // child element's flex value to 0 to skip rendering the pane's contents.
                if pane_hidden_for_move(hidden_panes, &group.active_pane()) {
                    flex_value = 0.;
                }
            }

            parent.add_child(
                Shrinkable::new(flex_value, node.render(theme, hidden_panes, strip, app)).finish(),
            );
            if let Some(divider) = dividers.next() {
                if matches!(node, PaneNode::Leaf(group) if pane_hidden_for_move(hidden_panes, &group.active_pane()))
                {
                    continue;
                }
                // Store a position index to render the actual divider at after we've rendered all pane content.
                // The reason we don't render the actual divider here is that, if we have rich content
                // (or anything that listens for a mouse click) to the right/bottom of the divider,
                // that content is rendered after the divider. Because of that, the clickbox for
                // that content takes precedence over the divider's clickbox, meaning the divider
                // is not clickable when this content is in the blocklist. To fix this, we wait to
                // render the actual divider until after we've rendered all pane content. We
                // cannot use an overlay because content like right click menus that overflow over
                // the divider should still take precedence over the divider's clickbox.
                let position_id = format!("divider_placeholder_{}", divider.id);
                divider_positions.push((divider, position_id.clone()));
                parent.add_child(create_divider_placeholder(self.axis, &position_id));
            }
        }
        let mut stack = Stack::new().with_constrain_absolute_children();
        stack.add_child(parent.finish());

        // Add actual dividers as positioned children anchored to their placeholders
        // (the reason we have to do it this way is explained in the large comment above)
        for (divider, position_id) in divider_positions {
            let divider_element = if FeatureFlag::MinimalistUI.is_enabled() {
                create_minimalist_divider(self.axis, divider, theme)
            } else {
                create_divider(self.axis, divider, theme)
            };

            stack.add_positioned_child(
                divider_element,
                OffsetPositioning::offset_from_save_position_element(
                    position_id,
                    Vector2F::new(0., 0.),
                    PositionedElementOffsetBounds::Unbounded,
                    PositionedElementAnchor::TopLeft,
                    ChildAnchor::TopLeft,
                ),
            );
        }
        stack.finish()
    }

    pub fn adjust_pane_size(
        &mut self,
        hidden_panes: &[HiddenPane],
        border_id: EntityId,
        delta: f32,
        ctx: &mut ViewContext<PaneGroup>,
    ) -> bool {
        if let Some(idx) = self
            .dividers
            .iter()
            .position(|divider| divider.id == border_id)
        {
            let pane_size_1 = self.nodes[idx].1.pane_size(hidden_panes, ctx);
            let pane_size_2 = self.nodes[idx + 1].1.pane_size(hidden_panes, ctx);

            let flex_1 = self.nodes[idx].0 .0;
            let flex_2 = self.nodes[idx + 1].0 .0;

            let total_flex = flex_1 + flex_2;

            let (size_1, size_2) = match self.axis {
                SplitDirection::Horizontal => (pane_size_1.x(), pane_size_2.x()),
                SplitDirection::Vertical => (pane_size_1.y(), pane_size_2.y()),
            };

            // Omit noise in dragging.
            let minimum_pane_size = get_minimum_pane_size(ctx);
            if size_1 + delta < minimum_pane_size
                || size_2 - delta < minimum_pane_size
                || delta.abs() < f32::EPSILON
            {
                return true;
            }

            // Re-distribute the flex factors.
            let new_flex = ((size_1 + delta) / (size_1 + size_2) * total_flex)
                .max(0.)
                .min(total_flex);

            self.nodes[idx].0 = PaneFlex(new_flex);
            self.nodes[idx + 1].0 = PaneFlex(total_flex - new_flex);

            return true;
        }

        for (_, node) in &mut self.nodes {
            if node.adjust_pane_size(hidden_panes, border_id, delta, ctx) {
                return true;
            }
        }

        false
    }

    // Get the size of a branch by recursively adding the size of its children.
    pub fn size(&self, hidden_panes: &[HiddenPane], ctx: &mut ViewContext<PaneGroup>) -> Vector2F {
        match self.axis {
            SplitDirection::Horizontal => Vector2F::new(
                self.nodes
                    .iter()
                    .fold(0., |x, (_, node)| x + node.pane_size(hidden_panes, ctx).x()),
                self.nodes[0].1.pane_size(hidden_panes, ctx).y(),
            ),
            SplitDirection::Vertical => Vector2F::new(
                self.nodes[0].1.pane_size(hidden_panes, ctx).x(),
                self.nodes
                    .iter()
                    .fold(0., |y, (_, node)| y + node.pane_size(hidden_panes, ctx).y()),
            ),
        }
    }

    pub fn adjust_pane_size_by_id(
        &mut self,
        hidden_panes: &[HiddenPane],
        pane_id: PaneId,
        direction: SplitDirection,
        delta: f32,
        ctx: &mut ViewContext<PaneGroup>,
    ) -> bool {
        for (idx, (_, node)) in self.nodes.iter_mut().enumerate() {
            if node.adjust_pane_size_by_id(hidden_panes, pane_id, direction, delta, ctx) {
                // If the resizing direction is different from the splitting direction
                // of the branch, we return for the parents to handle.
                if direction != self.axis {
                    return true;
                }

                let divider_id = self.dividers[idx.min(self.dividers.len() - 1)].id;
                self.adjust_pane_size(hidden_panes, divider_id, delta, ctx);
                break;
            }
        }
        false
    }

    pub fn axis(&self) -> SplitDirection {
        self.axis
    }

    // Find the sibling of the given pane in the given direction.
    //  They must be direct children of the same branch.
    fn sibling_by_direction(&self, pane_id: PaneId, direction: Direction) -> Option<PaneId> {
        for (idx, (_, node)) in self.nodes.iter().enumerate() {
            match node {
                PaneNode::Branch(branch) => {
                    if let Some(id) = branch.sibling_by_direction(pane_id, direction) {
                        return Some(id);
                    }
                }
                // Siblings are resolved on the visible layout: locate by the leaf's
                // active member and resolve to the neighbour's active member.
                PaneNode::Leaf(group) => {
                    if direction.axis() == self.axis() && group.active_pane() == pane_id {
                        return match direction {
                            Direction::Left | Direction::Up => {
                                if idx == 0 {
                                    None
                                } else {
                                    match &self.nodes[idx - 1].1 {
                                        PaneNode::Leaf(group) => Some(group.active_pane()),
                                        _ => None,
                                    }
                                }
                            }
                            Direction::Right | Direction::Down => {
                                if idx == self.nodes.len() - 1 {
                                    None
                                } else {
                                    match &self.nodes[idx + 1].1 {
                                        PaneNode::Leaf(group) => Some(group.active_pane()),
                                        _ => None,
                                    }
                                }
                            }
                        };
                    }
                }
            }
        }
        None
    }

    fn contains_pane(&self, pane_id: PaneId) -> bool {
        self.nodes
            .iter()
            .any(|(_, node)| node.contains_pane(pane_id))
    }

    fn replace_pane(&mut self, old_pane_id: PaneId, new_pane_id: PaneId) -> bool {
        for (_, node) in &mut self.nodes {
            if node.replace_pane(old_pane_id, new_pane_id) {
                return true;
            }
        }
        false
    }

    fn has_visible_children(&self, hidden_panes: &[HiddenPane]) -> bool {
        self.nodes
            .iter()
            .any(|(_, node)| node.has_visible_children(hidden_panes))
    }

    fn has_children_hidden_for_move(&self, hidden_panes: &[HiddenPane]) -> bool {
        self.nodes
            .iter()
            .any(|(_, node)| node.has_children_hidden_for_move(hidden_panes))
    }
}

fn pane_hidden_for_job(hidden_panes: &[HiddenPane], id: &PaneId) -> bool {
    hidden_panes
        .iter()
        .any(|pane| pane.reason == HiddenPaneReason::FromJob && pane.pane_id == *id)
}

fn pane_hidden_for_move(hidden_panes: &[HiddenPane], id: &PaneId) -> bool {
    hidden_panes
        .iter()
        .any(|pane| pane.reason == HiddenPaneReason::FromMove && pane.pane_id == *id)
}

fn pane_hidden_for_undo(hidden_panes: &[HiddenPane], id: &PaneId) -> bool {
    hidden_panes
        .iter()
        .any(|pane| pane.reason == HiddenPaneReason::Closed && pane.pane_id == *id)
}

fn pane_hidden_for_child_agent(hidden_panes: &[HiddenPane], id: &PaneId) -> bool {
    hidden_panes
        .iter()
        .any(|pane| pane.reason == HiddenPaneReason::ChildAgent && pane.pane_id == *id)
}

impl FindPaneByDirection for PaneBranch {
    fn panes_by_direction(
        &self,
        pane_id: PaneId,
        direction: Direction,
    ) -> FindPaneByDirectionResult {
        for (idx, (_, node)) in self.nodes.iter().enumerate() {
            let res = node.panes_by_direction(pane_id, direction);

            match res {
                FindPaneByDirectionResult::Found(_) => return res,
                FindPaneByDirectionResult::Located => {
                    // If the axis is different, we left for the parent branch to handle.
                    if direction.axis() != self.axis {
                        return res;
                    }

                    let target_panes = match direction {
                        Direction::Left | Direction::Up => {
                            if idx == 0 {
                                return res;
                            }
                            self.nodes[idx - 1].1.first_panes_in_direction(direction)
                        }
                        Direction::Right | Direction::Down => {
                            if idx == self.nodes.len() - 1 {
                                return res;
                            }
                            self.nodes[idx + 1].1.first_panes_in_direction(direction)
                        }
                    };

                    return FindPaneByDirectionResult::Found(target_panes);
                }
                FindPaneByDirectionResult::NotFound => (),
            }
        }
        FindPaneByDirectionResult::NotFound
    }
}

/// Create an invisible placeholder element that occupies the same space as the divider
/// and saves its position so the actual divider can be anchored to it.
fn create_divider_placeholder(direction: SplitDirection, position_id: &str) -> Box<dyn Element> {
    let divider_thickness = get_divider_thickness() - 1.0;

    let placeholder = match direction {
        SplitDirection::Horizontal => ConstrainedBox::new(Empty::new().finish())
            .with_width(divider_thickness)
            .finish(),
        SplitDirection::Vertical => ConstrainedBox::new(Empty::new().finish())
            .with_height(divider_thickness)
            .finish(),
    };

    SavePosition::new(placeholder, position_id).finish()
}

fn create_divider(
    direction: SplitDirection,
    item: &Divider,
    theme: &MnemosTheme,
) -> Box<dyn Element> {
    let divider = ConstrainedBox::new(
        Rect::new()
            .with_background(theme.split_pane_border_color())
            .finish(),
    );

    let cursor_shape = match direction {
        SplitDirection::Horizontal => Cursor::ResizeLeftRight,
        SplitDirection::Vertical => Cursor::ResizeUpDown,
    };

    let border_id = item.id;

    Hoverable::new(item.mouse_state.clone(), |_| {
        EventHandler::new(match direction {
            SplitDirection::Horizontal => divider.with_width(get_divider_thickness()).finish(),
            SplitDirection::Vertical => divider.with_height(get_divider_thickness()).finish(),
        })
        .on_left_mouse_down(move |ctx, _, position| {
            ctx.dispatch_typed_action(PaneGroupAction::StartResizing(DraggedBorder {
                border_id,
                direction,
                previous_mouse_location: position,
            }));
            DispatchEventResult::StopPropagation
        })
        .finish()
    })
    .with_cursor(cursor_shape)
    .with_propagate_drag()
    .finish()
}

fn create_minimalist_divider(
    direction: SplitDirection,
    item: &Divider,
    theme: &MnemosTheme,
) -> Box<dyn Element> {
    let divider = ConstrainedBox::new(
        Rect::new()
            .with_background(theme.split_pane_border_color())
            .finish(),
    );

    let cursor_shape = match direction {
        SplitDirection::Horizontal => Cursor::ResizeLeftRight,
        SplitDirection::Vertical => Cursor::ResizeUpDown,
    };

    let border_id = item.id;
    let hoverable = Hoverable::new(item.mouse_state.clone(), |_| {
        let container = match direction {
            SplitDirection::Horizontal => {
                Container::new(divider.with_width(get_divider_thickness()).finish())
                    .with_padding_left(DIVIDER_RESIZE_PADDING)
                    .with_padding_right(DIVIDER_RESIZE_PADDING)
                    .finish()
            }
            SplitDirection::Vertical => {
                Container::new(divider.with_height(get_divider_thickness()).finish())
                    .with_padding_top(DIVIDER_RESIZE_PADDING)
                    .with_padding_bottom(DIVIDER_RESIZE_PADDING)
                    .finish()
            }
        };
        EventHandler::new(container)
            .on_left_mouse_down(move |ctx, _, position| {
                ctx.dispatch_typed_action(PaneGroupAction::StartResizing(DraggedBorder {
                    border_id,
                    direction,
                    previous_mouse_location: position,
                }));
                DispatchEventResult::StopPropagation
            })
            .finish()
    })
    .with_cursor(cursor_shape)
    .with_propagate_drag();

    let mut stack = Stack::new().with_constrain_absolute_children();
    match direction {
        SplitDirection::Horizontal => stack.add_positioned_child(
            hoverable.finish(),
            OffsetPositioning::offset_from_parent(
                Vector2F::new(0., 0.),
                ParentOffsetBounds::Unbounded,
                ParentAnchor::TopMiddle,
                ChildAnchor::TopMiddle,
            ),
        ),
        SplitDirection::Vertical => stack.add_positioned_child(
            hoverable.finish(),
            OffsetPositioning::offset_from_parent(
                Vector2F::new(0., -DIVIDER_RESIZE_PADDING),
                ParentOffsetBounds::Unbounded,
                ParentAnchor::TopLeft,
                ChildAnchor::TopLeft,
            ),
        ),
    };
    stack.finish()
}

impl fmt::Debug for PaneData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.tabs.len() == 1 {
            // Preserve the historical single-root representation when untabbed.
            write!(f, "Root({:?})", self.tabs[0].root)
        } else {
            write!(f, "Tabs[active={}]", self.active_tab)?;
            f.debug_list()
                .entries(self.tabs.iter().map(|tab| &tab.root))
                .finish()
        }
    }
}

impl fmt::Debug for PaneNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Preserve the historical representation for single-member leaves.
            PaneNode::Leaf(group) if group.len() == 1 => {
                write!(f, "Leaf({:?})", group.active_pane())
            }
            PaneNode::Leaf(group) => write!(
                f,
                "LeafGroup[active={}]({:?})",
                group.active_index(),
                group.members()
            ),
            PaneNode::Branch(branch) => write!(f, "Branch {branch:?}"),
        }
    }
}

impl fmt::Debug for PaneBranch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.axis {
            SplitDirection::Horizontal => write!(f, "Horizontal({:?})", self.nodes),
            SplitDirection::Vertical => write!(f, "Vertical({:?})", self.nodes),
        }
    }
}

// When pane group is split horizontally, new panes are added from left to right.
// When pane group is split vertically, new panes are added from top to bottom.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

impl From<app_state::SplitDirection> for SplitDirection {
    fn from(direction: app_state::SplitDirection) -> Self {
        match direction {
            app_state::SplitDirection::Horizontal => SplitDirection::Horizontal,
            app_state::SplitDirection::Vertical => SplitDirection::Vertical,
        }
    }
}

impl From<SplitDirection> for app_state::SplitDirection {
    fn from(direction: SplitDirection) -> Self {
        match direction {
            SplitDirection::Horizontal => app_state::SplitDirection::Horizontal,
            SplitDirection::Vertical => app_state::SplitDirection::Vertical,
        }
    }
}

impl From<crate::launch_configs::launch_config::SplitDirection> for SplitDirection {
    fn from(direction: crate::launch_configs::launch_config::SplitDirection) -> Self {
        match direction {
            crate::launch_configs::launch_config::SplitDirection::Horizontal => {
                SplitDirection::Horizontal
            }
            crate::launch_configs::launch_config::SplitDirection::Vertical => {
                SplitDirection::Vertical
            }
        }
    }
}

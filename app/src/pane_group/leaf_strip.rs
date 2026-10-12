//! Per-pane tab strip for multi-member leaf groups (TCB v1-style "tab groups").
//!
//! Rendered by the TREE LEAF ARM (`tree.rs`), *not* inside `PaneHeader<P>` —
//! the header is generic over one `BackingView`, which would block
//! heterogeneous (terminal + code + …) groups. The strip sits above the
//! group's active member: `Flex::column` = strip row + `Shrinkable(1., active
//! member render)`. Hidden when the group has fewer than two (non-hidden)
//! members, mirroring the workspace-level `terminal_tab_strip`.
//!
//! Visual language is shared with [`super::terminal_tab_strip`] (same
//! `TabStyle` theme fills, same row metrics). v1 is click-to-activate only —
//! no dragging from the strip (Phase 2).
//!
//! Clicks dispatch [`PaneGroupAction::ActivateLeafGroupMember`]; the per-tab ×
//! dispatches [`PaneGroupAction::CloseLeafGroupMember`]. As in the workspace
//! strip, the × is its own nested [`Hoverable`] with its own mouse-down
//! handler so the close click fires instead of the body's activate.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use mnemos_ui::elements::{
    Align, Border, Clipped, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Draggable,
    DraggableState, Element, Fill, Flex, Hoverable, MainAxisSize, MouseState, MouseStateHandle,
    ParentElement, Radius, Shrinkable, Stack, Text,
};
use mnemos_ui::fonts::FamilyId;
use mnemos_ui::platform::Cursor;
use mnemos_ui::AppContext;

use crate::pane_group::pane::PaneId;
use crate::pane_group::terminal_tab_strip::{
    hit_test_pane, TabStyle, CLOSE_WIDTH, STRIP_HEIGHT, STRIP_HORIZONTAL_PADDING,
    TAB_CORNER_RADIUS, TAB_HORIZONTAL_PADDING, TAB_MAX_WIDTH, TAB_MIN_WIDTH, TAB_SPACING,
    TAB_VERTICAL_PADDING,
};
use crate::pane_group::tree::{HiddenPane, PaneLeafGroup};
use crate::pane_group::PaneGroupAction;

/// One member entry to render in a leaf group's strip.
struct LeafStripItem {
    pane_id: PaneId,
    label: String,
    is_active: bool,
}

/// Stable per-member `MouseState` handles. `Hoverable` consumes a handle on
/// every render, so these must persist across frames; they live on `PaneGroup`
/// (`leaf_strip_states`) and are reconciled against the live group membership
/// each render via [`LeafStripStates::sync`] — the same pattern as
/// `terminal_tab_strip_states`.
#[derive(Default)]
pub struct LeafStripStates {
    body: HashMap<PaneId, MouseStateHandle>,
    close: HashMap<PaneId, MouseStateHandle>,
    /// Per-member drag state. Like the mouse-state handles, these must persist
    /// across renders so an in-flight drag isn't reset every frame.
    drag: HashMap<PaneId, DraggableState>,
}

impl LeafStripStates {
    fn new_state() -> MouseStateHandle {
        Arc::new(Mutex::new(MouseState::default()))
    }

    /// Ensure a body + close + drag handle exists for every current
    /// multi-member group member, dropping handles for panes that left. Keeps
    /// existing handles by identity so hover/drag state survives re-renders.
    pub fn sync(&mut self, member_ids: &[PaneId]) {
        let mut body = HashMap::with_capacity(member_ids.len());
        let mut close = HashMap::with_capacity(member_ids.len());
        let mut drag = HashMap::with_capacity(member_ids.len());
        for id in member_ids {
            let b = self.body.remove(id).unwrap_or_else(Self::new_state);
            let c = self.close.remove(id).unwrap_or_else(Self::new_state);
            let d = self.drag.remove(id).unwrap_or_default();
            body.insert(*id, b);
            close.insert(*id, c);
            drag.insert(*id, d);
        }
        self.body = body;
        self.close = close;
        self.drag = drag;
    }

    fn body_state(&self, id: PaneId) -> MouseStateHandle {
        self.body.get(&id).cloned().unwrap_or_else(Self::new_state)
    }

    fn close_state(&self, id: PaneId) -> MouseStateHandle {
        self.close.get(&id).cloned().unwrap_or_else(Self::new_state)
    }

    fn drag_state(&self, id: PaneId) -> DraggableState {
        self.drag.get(&id).cloned().unwrap_or_default()
    }
}

/// Per-render context handed down the tree render path. `PaneGroup::render`
/// computes the labels (it owns `pane_contents`, where pane titles live) and
/// reconciles the mouse states; the tree's leaf arm consumes both.
pub struct LeafStripContext<'a> {
    /// Pane title per multi-member group member. Members without a usable
    /// title fall back to a positional label at render time.
    pub labels: &'a HashMap<PaneId, String>,
    pub states: &'a LeafStripStates,
    /// Panes currently visible on screen (the active member of every leaf).
    /// A dragged strip tab's drop handler hit-tests the drop point against
    /// these panes' cached rects — geometry, not drop-target resolution — the
    /// same position-first approach as the workspace `terminal_tab_strip`.
    pub visible_pane_ids: &'a [PaneId],
}

/// Wrap a leaf group's rendered active member with the member strip.
///
/// Returns `content` unchanged when the group has fewer than two non-hidden
/// members (single leaves look exactly like before). Hidden members (e.g.
/// hidden-for-close during the undo grace period) get no strip tab.
pub fn wrap_leaf_group(
    group: &PaneLeafGroup,
    hidden_panes: &[HiddenPane],
    strip: &LeafStripContext<'_>,
    content: Box<dyn Element>,
    app: &AppContext,
) -> Box<dyn Element> {
    let members: Vec<PaneId> = group
        .members()
        .iter()
        .copied()
        .filter(|member| !hidden_panes.iter().any(|hidden| hidden.pane_id == *member))
        .collect();
    if members.len() < 2 {
        return content;
    }

    let active_pane = group.active_pane();
    let items: Vec<LeafStripItem> = members
        .iter()
        .map(|pane_id| LeafStripItem {
            pane_id: *pane_id,
            label: strip.labels.get(pane_id).cloned().unwrap_or_else(|| {
                // Number by position in the FULL member list, not the
                // hidden-filtered one, so labels stay stable while a member is
                // hidden during an undo grace period.
                let n = group
                    .members()
                    .iter()
                    .position(|member| member == pane_id)
                    .map_or(0, |idx| idx + 1);
                format!("Tab {n}")
            }),
            is_active: *pane_id == active_pane,
        })
        .collect();

    let strip_row = render_leaf_strip(&items, &members, strip.states, strip.visible_pane_ids, app);

    let mut column = Flex::column()
        .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
        .with_main_axis_size(MainAxisSize::Max);
    column.add_child(strip_row);
    column.add_child(Shrinkable::new(1., content).finish());
    column.finish()
}

/// The strip row itself: one clickable tab per member (active member visually
/// distinct, × per tab). Same metrics/fills as the workspace terminal-tab
/// strip, minus the (+) button and all drag affordances.
fn render_leaf_strip(
    items: &[LeafStripItem],
    group_members: &[PaneId],
    states: &LeafStripStates,
    visible_pane_ids: &[PaneId],
    app: &AppContext,
) -> Box<dyn Element> {
    let (style, font) = TabStyle::from_app(app);

    let children: Vec<Box<dyn Element>> = items
        .iter()
        .map(|item| render_member_tab(item, group_members, states, visible_pane_ids, style, font))
        .collect();

    let row = Flex::row()
        .with_spacing(TAB_SPACING)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_children(children)
        .finish();

    let strip = Container::new(row)
        .with_horizontal_padding(STRIP_HORIZONTAL_PADDING)
        .with_background(style.fg_overlay_1)
        .finish();

    ConstrainedBox::new(strip).with_height(STRIP_HEIGHT).finish()
}

fn render_member_tab(
    item: &LeafStripItem,
    group_members: &[PaneId],
    states: &LeafStripStates,
    visible_pane_ids: &[PaneId],
    style: TabStyle,
    font: FamilyId,
) -> Box<dyn Element> {
    let is_active = item.is_active;
    let body_state = states.body_state(item.pane_id);
    let close_state = states.close_state(item.pane_id);
    let drag_state = states.drag_state(item.pane_id);

    let pane_id = item.pane_id;
    let label = item.label.clone();

    let body = Hoverable::new(body_state, move |body_state| {
        let body_hovered = body_state.is_hovered();
        let (background, border_fill) = style.body_state_fills(is_active, body_hovered);
        let label_color = style.label_color(is_active, body_hovered);

        let label_el: Box<dyn Element> = Text::new_inline(label.clone(), font, style.font_size)
            .with_color(label_color)
            .finish();
        let label_clipped = Clipped::new(label_el).finish();

        // Close (×) — only visible when active or hovered. A *separate* nested
        // Hoverable with its own mouse-down handler, so the close click fires
        // instead of the body's activate (the v1 swallowed-click lesson).
        let close_slot: Box<dyn Element> = if is_active || body_hovered {
            let close_state = close_state.clone();
            Hoverable::new(close_state, move |close_state| {
                let close_hovered = close_state.is_hovered();
                let bg = if close_hovered {
                    style.fg_overlay_3
                } else {
                    Fill::None
                };
                let glyph_color = if close_hovered {
                    style.fg_full
                } else {
                    style.fg_60
                };
                let glyph = Text::new_inline("×".to_string(), font, style.font_size)
                    .with_color(glyph_color)
                    .finish();
                Container::new(Align::new(glyph).finish())
                    .with_background(bg)
                    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(TAB_CORNER_RADIUS)))
                    .finish()
            })
            .with_cursor(Cursor::PointingHand)
            .on_mouse_down(move |ev_ctx, _app, _pos| {
                ev_ctx.dispatch_typed_action(PaneGroupAction::CloseLeafGroupMember { pane_id });
            })
            .finish()
        } else {
            // Reserve the column width so tabs don't jiggle on hover.
            ConstrainedBox::new(Stack::new().finish())
                .with_width(0.0)
                .finish()
        };

        let close_column = ConstrainedBox::new(close_slot)
            .with_width(CLOSE_WIDTH)
            .with_height(STRIP_HEIGHT)
            .finish();

        let content = Flex::row()
            .with_spacing(4.0)
            .with_child(label_clipped)
            .with_child(close_column)
            .finish();

        let tab_inner = Container::new(content)
            .with_horizontal_padding(TAB_HORIZONTAL_PADDING)
            .with_vertical_padding(TAB_VERTICAL_PADDING)
            .with_background(background)
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(TAB_CORNER_RADIUS)))
            .with_border(Border::all(1.).with_border_fill(border_fill))
            .finish();

        ConstrainedBox::new(tab_inner)
            .with_min_width(TAB_MIN_WIDTH)
            .with_max_width(TAB_MAX_WIDTH)
            .with_height(STRIP_HEIGHT - 2.0 * TAB_VERTICAL_PADDING)
            .finish()
    })
    .with_cursor(Cursor::PointingHand)
    .on_mouse_down(move |ev_ctx, _app, _pos| {
        ev_ctx.dispatch_typed_action(PaneGroupAction::ActivateLeafGroupMember { pane_id });
    })
    .finish();

    // Each strip tab is draggable: dragging a member out lets it become a pane
    // again (edge drop) or a tab in another pane's group (center drop). The
    // decision is POSITION-FIRST — we hit-test the drop point against real pane
    // geometry rather than branching on the resolved drop-target `data`. As in
    // `terminal_tab_strip`, a strip-level Draggable never reliably resolves to
    // the pane drop targets living in a different render subtree, so consulting
    // `data` would misfire (the "v3 bug"); geometry is the source of truth.
    let drag_visible_pane_ids: Vec<PaneId> = visible_pane_ids.to_vec();
    let drop_visible_pane_ids: Vec<PaneId> = visible_pane_ids.to_vec();
    let drag_group_members: Vec<PaneId> = group_members.to_vec();

    Draggable::new(drag_state, body)
        .on_drag(move |ev_ctx, _app, drag_rect, _data| {
            // Live drop-zone preview: highlight the pane (edge/center) under the
            // drop point. The hovered pane is usually the dragged member's OWN
            // pane (pressing a strip tab activates it), and edge drops there are
            // real actions — the member pops out and splits at that edge (the
            // handler retargets to a sibling) — so edge zones must preview.
            // Only the center zone over the member's own group is suppressed:
            // that drop is a no-op (merging back into the group it's already in).
            let target =
                hit_test_pane(ev_ctx, &drag_visible_pane_ids, drag_rect).filter(
                    |(hit_id, direction)| {
                        !(direction.is_none() && drag_group_members.contains(hit_id))
                    },
                );
            ev_ctx.dispatch_typed_action(PaneGroupAction::PreviewPaneDropZone { target });
        })
        .on_drop(move |ev_ctx, _app, drag_rect, _data| {
            match hit_test_pane(ev_ctx, &drop_visible_pane_ids, drag_rect) {
                // Over a pane → move the member there (edge split / center tab).
                // Self- and own-group targets are resolved in the handler.
                Some((target_pane_id, direction)) => {
                    ev_ctx.dispatch_typed_action(PaneGroupAction::MoveLeafGroupMember {
                        member: pane_id,
                        target_pane_id,
                        direction,
                    });
                }
                // Over no pane → cancel (spec: no tear-off for leaf-tab drags).
                // Clear any stale preview overlay the drag left behind.
                None => {
                    ev_ctx.dispatch_typed_action(PaneGroupAction::PreviewPaneDropZone {
                        target: None,
                    });
                }
            }
        })
        .finish()
}

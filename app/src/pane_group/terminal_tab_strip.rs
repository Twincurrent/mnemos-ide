//! Browser-style terminal-tab strip rendered above the active tab's pane tree.
//!
//! This is the *root-level* terminal tab strip (one row of tabs per project),
//! distinct from the top-of-window project tab strip (`mnemos-ide-projects`). It mirrors
//! that strip's visual/interaction pattern: a ~28px row of rounded tabs, each with
//! a label and a close (×) button that appears on hover/active, plus a trailing (+)
//! "new terminal tab" button.
//!
//! Clicks dispatch [`PaneGroupAction`] variants, routed back to
//! [`crate::pane_group::PaneGroup::handle_action`].
//!
//! ## Close-button correctness
//! The v1 attempt's × button silently swallowed clicks because of how nested
//! interactive elements routed events. Here the close button is its own
//! [`Hoverable`] with its own `on_mouse_down` handler nested inside the tab body's
//! `Hoverable`. The innermost element under the cursor receives the press, so a
//! click on the × fires `CloseTerminalTab` (not the body's `ActivateTerminalTab`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use mnemos_core::ui::theme::color::internal_colors;
use mnemos_ui::elements::{
    AcceptedByDropTarget, Align, Border, Clipped, ConstrainedBox, Container, CornerRadius,
    Draggable, DraggableState, DropTarget, DropTargetData, Element, Fill, Flex, Hoverable,
    MouseState, MouseStateHandle, ParentElement, Radius, Stack, Text,
};
use mnemos_ui::fonts::FamilyId;
use mnemos_ui::platform::Cursor;
use pathfinder_geometry::rect::RectF;

use crate::appearance::Appearance;
use crate::features::FeatureFlag;
use crate::pane_group::pane::PaneId;
use crate::pane_group::pane::view::TabToSplitData;
use crate::pane_group::pane::view::header::calculate_pane_move_direction;
use crate::pane_group::tree::{Direction, PaneTabId};
use crate::pane_group::PaneGroupAction;
use mnemos_ui::{AppContext, SingletonEntity};

/// Drop-target marker placed on every tab in the strip. A tab drag dropped on one
/// of these triggers a reorder to `index`. Distinct from the pane drop-target types
/// so pane-header drags can't trigger tab reorders and vice-versa.
#[derive(Debug, Clone, Copy)]
struct ReorderTabData {
    /// The destination slot for the dragged tab.
    index: usize,
}

impl DropTargetData for ReorderTabData {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// Shared with the per-pane leaf-group strip (`leaf_strip.rs`) so both strips
// speak the same visual language.
pub(super) const STRIP_HEIGHT: f32 = 28.0;
pub(super) const TAB_SPACING: f32 = 4.0;
pub(super) const TAB_MIN_WIDTH: f32 = 80.0;
pub(super) const TAB_MAX_WIDTH: f32 = 200.0;
pub(super) const TAB_HORIZONTAL_PADDING: f32 = 8.0;
pub(super) const TAB_VERTICAL_PADDING: f32 = 2.0;
pub(super) const TAB_CORNER_RADIUS: f32 = 4.0;
pub(super) const CLOSE_WIDTH: f32 = 16.0;
const ADD_WIDTH: f32 = 28.0;
const ADD_BUTTON_LEADING_GAP: f32 = 6.0;
pub(super) const STRIP_HORIZONTAL_PADDING: f32 = 6.0;

/// One entry to render in the strip. Computed by `PaneGroup::render` (which has
/// access to per-pane titles) and handed to [`render_terminal_tab_strip`].
pub struct TabStripItem {
    pub id: PaneTabId,
    pub label: String,
    pub is_active: bool,
    /// This tab's current position in the strip — the drop slot a tab dragged onto
    /// it should move to.
    pub index: usize,
}

/// Stable per-tab `MouseState` handles. `Hoverable` consumes a handle on every
/// render, so these must persist across frames; they live on `PaneGroup` and are
/// reconciled against the live tab list each render via [`TabStripStates::sync`].
#[derive(Default)]
pub struct TabStripStates {
    body: HashMap<PaneTabId, MouseStateHandle>,
    close: HashMap<PaneTabId, MouseStateHandle>,
    /// Per-tab drag state. Like the mouse-state handles, these must persist across
    /// renders so an in-flight drag isn't reset every frame.
    drag: HashMap<PaneTabId, DraggableState>,
    add: Option<MouseStateHandle>,
}

impl TabStripStates {
    fn new_state() -> MouseStateHandle {
        Arc::new(Mutex::new(MouseState::default()))
    }

    /// Ensure a body + close + drag handle exists for every current tab id, dropping
    /// handles for ids that no longer exist. Keeps existing handles by identity so
    /// hover/drag state survives re-renders.
    pub fn sync(&mut self, tab_ids: &[PaneTabId]) {
        let mut body = HashMap::with_capacity(tab_ids.len());
        let mut close = HashMap::with_capacity(tab_ids.len());
        let mut drag = HashMap::with_capacity(tab_ids.len());
        for id in tab_ids {
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
        if self.add.is_none() {
            self.add = Some(Self::new_state());
        }
    }

    fn body_state(&self, id: PaneTabId) -> MouseStateHandle {
        self.body
            .get(&id)
            .cloned()
            .unwrap_or_else(Self::new_state)
    }

    fn close_state(&self, id: PaneTabId) -> MouseStateHandle {
        self.close
            .get(&id)
            .cloned()
            .unwrap_or_else(Self::new_state)
    }

    fn drag_state(&self, id: PaneTabId) -> DraggableState {
        self.drag.get(&id).cloned().unwrap_or_default()
    }

    fn add_state(&self) -> MouseStateHandle {
        self.add.clone().unwrap_or_else(Self::new_state)
    }
}

/// Snapshot of theme-resolved fills used to paint the strip. `Fill`/`ColorU` are
/// `Copy`, so this is captured once and copied into each Hoverable closure.
/// Shared with the per-pane leaf-group strip (`leaf_strip.rs`).
#[derive(Clone, Copy)]
pub(super) struct TabStyle {
    pub(super) fg_overlay_1: Fill,
    pub(super) fg_overlay_2: Fill,
    pub(super) fg_overlay_3: Fill,
    pub(super) fg_60: mnemos_ui::color::ColorU,
    pub(super) fg_90: mnemos_ui::color::ColorU,
    pub(super) fg_full: mnemos_ui::color::ColorU,
    pub(super) font_size: f32,
}

impl TabStyle {
    pub(super) fn from_app(app: &AppContext) -> (Self, FamilyId) {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let style = Self {
            fg_overlay_1: internal_colors::fg_overlay_1(theme).into(),
            fg_overlay_2: internal_colors::fg_overlay_2(theme).into(),
            fg_overlay_3: internal_colors::fg_overlay_3(theme).into(),
            fg_60: theme.foreground().with_opacity(60).into_solid(),
            fg_90: theme.foreground().with_opacity(90).into_solid(),
            fg_full: theme.foreground().into_solid(),
            font_size: appearance.ui_font_size(),
        };
        (style, appearance.ui_font_family())
    }

    pub(super) fn body_state_fills(&self, is_active: bool, hovered: bool) -> (Fill, Fill) {
        if is_active {
            (self.fg_overlay_2, self.fg_overlay_2)
        } else if hovered {
            (self.fg_overlay_1, self.fg_overlay_2)
        } else {
            (Fill::None, self.fg_overlay_1)
        }
    }

    pub(super) fn label_color(&self, is_active: bool, hovered: bool) -> mnemos_ui::color::ColorU {
        if is_active {
            self.fg_full
        } else if hovered {
            self.fg_90
        } else {
            self.fg_60
        }
    }
}

/// Render the whole terminal-tab strip. Returns `None` when the strip should be
/// hidden (fewer than two tabs — single-tab projects look like a plain terminal).
pub fn render_terminal_tab_strip(
    items: &[TabStripItem],
    states: &TabStripStates,
    visible_pane_ids: &[PaneId],
    app: &AppContext,
) -> Option<Box<dyn Element>> {
    if items.len() < 2 {
        return None;
    }

    let (style, font) = TabStyle::from_app(app);

    let mut children: Vec<Box<dyn Element>> = items
        .iter()
        .map(|item| render_tab(item, states, visible_pane_ids, style, font))
        .collect();

    // Separator gap before the (+) button.
    children.push(
        ConstrainedBox::new(Stack::new().finish())
            .with_width(ADD_BUTTON_LEADING_GAP - TAB_SPACING)
            .finish(),
    );
    children.push(render_add_button(states, style, font));

    let row = Flex::row()
        .with_spacing(TAB_SPACING)
        .with_cross_axis_alignment(mnemos_ui::elements::CrossAxisAlignment::Center)
        .with_children(children)
        .finish();

    let strip = Container::new(row)
        .with_horizontal_padding(STRIP_HORIZONTAL_PADDING)
        .with_background(style.fg_overlay_1)
        .finish();

    Some(
        ConstrainedBox::new(strip)
            .with_height(STRIP_HEIGHT)
            .finish(),
    )
}

fn render_tab(
    item: &TabStripItem,
    states: &TabStripStates,
    visible_pane_ids: &[PaneId],
    style: TabStyle,
    font: FamilyId,
) -> Box<dyn Element> {
    let is_active = item.is_active;
    let body_state = states.body_state(item.id);
    let close_state = states.close_state(item.id);
    let drag_state = states.drag_state(item.id);

    let tab_id = item.id;
    let tab_index = item.index;
    let label = item.label.clone();
    // Snapshot of the panes currently visible on screen. The drop handler hit-tests
    // the drop point against each pane's cached rect (geometry), rather than relying
    // on the drop resolving to a pane's `TabToSplitData` drop target — those targets
    // live in a different render subtree than this strip-level Draggable, so a tab
    // dropped over a pane never resolved to them (the v2 split bug).
    let visible_pane_ids: Vec<PaneId> = visible_pane_ids.to_vec();

    let body = Hoverable::new(body_state, move |body_state| {
        let body_hovered = body_state.is_hovered();
        let (background, border_fill) = style.body_state_fills(is_active, body_hovered);
        let label_color = style.label_color(is_active, body_hovered);

        let label_el: Box<dyn Element> = Text::new_inline(label.clone(), font, style.font_size)
            .with_color(label_color)
            .finish();
        let label_clipped = Clipped::new(label_el).finish();

        // Close (×) — only visible when active or hovered, so inactive tabs stay
        // calm. It's a *separate* nested Hoverable with its own mouse-down handler;
        // that's what makes the close click fire instead of the body's activate.
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
                ev_ctx.dispatch_typed_action(PaneGroupAction::CloseTerminalTab(tab_id));
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
        ev_ctx.dispatch_typed_action(PaneGroupAction::ActivateTerminalTab(tab_id));
    })
    .finish();

    // Each tab is itself a reorder drop target: dropping a dragged tab here moves the
    // dragged tab to this tab's slot.
    let drop_target = DropTarget::new(body, ReorderTabData { index: tab_index }).finish();

    // The tab is draggable. Three landing zones, all keyed off the data type the drop
    // resolved to (so a tab drag is never confused with a pane-header drag):
    //   * `ReorderTabData`  → reorder within the strip.
    //   * `TabToSplitData`  → split the tab into that pane (edge direction).
    //   * no accepted target → detach into a new window at the drop point.
    let drag_visible_pane_ids = visible_pane_ids.clone();
    Draggable::new(drag_state, drop_target)
        .with_accepted_by_drop_target_fn(move |data, _| {
            let accepted = data.as_any().is::<ReorderTabData>()
                || data.as_any().is::<TabToSplitData>();
            if accepted {
                AcceptedByDropTarget::Yes
            } else {
                AcceptedByDropTarget::No
            }
        })
        .on_drag(move |ev_ctx, _app, drag_rect, _data| {
            // Live drop-zone preview, reusing the header-drag overlay plumbing:
            // highlight the pane (edge/center) under the drag. Every pane is a
            // real target here — including the dragged tab's OWN pane (pressing
            // a strip tab activates it), whose drop the handler redirects to the
            // neighbour tab — so no target is suppressed. The center zone is
            // remapped by `resolve_center_zone` so the preview always shows
            // exactly what the drop will commit.
            let target = hit_test_pane(ev_ctx, &drag_visible_pane_ids, drag_rect)
                .map(|(hit_id, direction)| (hit_id, resolve_center_zone(direction)));
            ev_ctx.dispatch_typed_action(PaneGroupAction::PreviewPaneDropZone { target });
        })
        .on_drop(move |ev_ctx, _app, drag_rect, data| {
            // The decision is POSITION-FIRST. We deliberately do NOT branch on `data`
            // for the split-vs-reorder choice: when the cursor is over no other
            // accepted target, the Draggable falls back to resolving `data` to the
            // dragged tab's OWN `ReorderTabData` target — so `data` is `ReorderTabData`
            // on essentially every drop, including drops deep inside a pane. Using it
            // to gate the pane check made every drop reorder (the v3 bug). Instead we
            // hit-test the drop point against real geometry and only consult the
            // `ReorderTabData` index once we've already decided the drop is a reorder.

            // 1) PANE GEOMETRY FIRST — the same hit-test the drag preview uses, so
            //    the drop commits exactly the zone the overlay showed.
            if let Some((target_pane_id, direction)) =
                hit_test_pane(ev_ctx, &visible_pane_ids, drag_rect)
            {
                // Edge zone → side-by-side split toward that edge; center zone →
                // merge as a tab into the target's leaf group (or a right-hand
                // split when leaf groups are off — see `resolve_center_zone`).
                let direction = resolve_center_zone(direction);
                ev_ctx.dispatch_typed_action(PaneGroupAction::SplitTerminalTabIntoPane {
                    tab_id,
                    target_pane_id,
                    direction,
                });
                return;
            }

            // Over no pane → whatever preview the drag left behind is stale.
            ev_ctx.dispatch_typed_action(PaneGroupAction::PreviewPaneDropZone { target: None });

            // 2) Over NO pane. Decide reorder vs detach by the strip's vertical band:
            //    the strip sits ABOVE the pane area, so a drop whose point is above the
            //    topmost pane's top edge is in the strip → reorder. Use the resolved
            //    `ReorderTabData` index for the destination slot (the dragged tab's own
            //    reorder target reliably resolves); this is the one place `data` is a
            //    legitimate input, now that geometry has already ruled out a split.
            let drop_point = drag_rect.center();
            let topmost_pane_top = visible_pane_ids
                .iter()
                .filter_map(|pane_id| ev_ctx.element_position_by_id(pane_id.position_id()))
                .map(|pane_rect| pane_rect.origin_y())
                .reduce(f32::min);
            let in_strip_band = topmost_pane_top
                .map(|top| drop_point.y() < top)
                .unwrap_or(false);
            if in_strip_band {
                let new_index = data
                    .and_then(|d| d.as_any().downcast_ref::<ReorderTabData>())
                    .map(|r| r.index)
                    .unwrap_or(tab_index);
                ev_ctx.dispatch_typed_action(PaneGroupAction::ReorderTerminalTab {
                    id: tab_id,
                    new_index,
                });
                return;
            }

            // 3) Over neither a pane nor the strip → truly outside → detach to a new
            //    window at the dragged tab's top-left corner.
            ev_ctx.dispatch_typed_action(PaneGroupAction::DetachTerminalTabToWindow {
                tab_id,
                screen_position: drag_rect.origin(),
            });
        })
        .finish()
}

/// Map the center zone to the direction the drop will actually commit: with
/// [`FeatureFlag::PaneLeafGroups`] enabled (the default) a center drop merges
/// the tab into the target's leaf group (`None`); with the flag off there is
/// nothing to merge into, so the center commits a right-hand split — and the
/// drag preview must show that edge, not a center zone (what you see is what
/// you get).
fn resolve_center_zone(direction: Option<Direction>) -> Option<Direction> {
    if direction.is_none() && !FeatureFlag::PaneLeafGroups.is_enabled() {
        Some(Direction::Right)
    } else {
        direction
    }
}

/// Hit-test `drag_rect`'s CENTER against every visible pane's cached rect.
/// Returns the first pane containing the point, along with the drop zone
/// (`Some(edge)` near an edge, `None` for a center drop) from
/// [`calculate_pane_move_direction`].
///
/// The center is deliberately the same reference point that
/// `calculate_pane_move_direction` keys the zone off (matching the pane-header
/// drag, which also decides its 5-zone preview by the ghost rect's center), so
/// "which pane" and "which zone" can never disagree — an origin-based
/// containment test could land in one pane while the zone was computed for a
/// center sitting in its neighbour.
///
/// Shared by the workspace strip and the per-pane leaf-group strip
/// (`leaf_strip.rs`): both drag geometrically rather than through resolved
/// drop-target data (the v2/v3 bugs), so both need the same hit-test.
pub(super) fn hit_test_pane(
    ev_ctx: &mnemos_ui::EventContext<'_>,
    visible_pane_ids: &[PaneId],
    drag_rect: RectF,
) -> Option<(PaneId, Option<Direction>)> {
    let drop_point = drag_rect.center();
    for pane_id in visible_pane_ids {
        if let Some(pane_rect) = ev_ctx.element_position_by_id(pane_id.position_id()) {
            if pane_rect.contains_point(drop_point) {
                let direction = calculate_pane_move_direction(pane_rect, drag_rect);
                return Some((*pane_id, direction));
            }
        }
    }
    None
}

fn render_add_button(
    states: &TabStripStates,
    style: TabStyle,
    font: FamilyId,
) -> Box<dyn Element> {
    Hoverable::new(states.add_state(), move |state| {
        let hovered = state.is_hovered();
        let bg = if hovered { style.fg_overlay_1 } else { Fill::None };
        let glyph_color = if hovered { style.fg_full } else { style.fg_60 };
        let glyph = Text::new_inline("+".to_string(), font, style.font_size + 2.0)
            .with_color(glyph_color)
            .finish();
        let inner = Container::new(Align::new(glyph).finish())
            .with_background(bg)
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(TAB_CORNER_RADIUS)))
            .finish();
        ConstrainedBox::new(inner)
            .with_width(ADD_WIDTH)
            .with_height(STRIP_HEIGHT - 2.0 * TAB_VERTICAL_PADDING)
            .finish()
    })
    .with_cursor(Cursor::PointingHand)
    .on_mouse_down(|ev_ctx, _app, _pos| {
        ev_ctx.dispatch_typed_action(PaneGroupAction::AddTerminalTab);
    })
    .finish()
}

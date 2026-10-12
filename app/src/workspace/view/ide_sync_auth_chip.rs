//! Mnemos IDE: the header chip that says mnemos-ide-sync has lost its admin login.
//!
//! Sync used to stop without a trace in the UI when the server ended the
//! session: the log filled with failed pushes while the app looked healthy.
//! This chip sits with the status icons at the right end of the tab bar, is
//! there only while the login needs attention (see
//! `crate::auth::ide_sync_auth::notice`), and opens the admin sign-in on
//! click (or checks for an update, when the sync server needs a newer
//! Mnemos). The sign-in's redirect restarts sync, so no app restart is needed.

use mnemos_core::ui::theme::Fill;
use mnemos_ui::elements::{
    Align, Border, ChildAnchor, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment,
    Element, Flex, Hoverable, OffsetPositioning, ParentAnchor, ParentElement, ParentOffsetBounds,
    Radius, Stack, Text,
};
use mnemos_ui::geometry::vector::vec2f;
use mnemos_ui::platform::Cursor;
use mnemos_ui::ui_components::components::UiComponent;
use mnemos_ui::{AppContext, SingletonEntity};

use super::{Workspace, PILL_FONT_SIZE, TAB_BAR_ICON_PADDING};
use crate::appearance::Appearance;
use crate::auth::auth_manager::AuthManager;
use crate::auth::ide_sync_auth::{self, SyncAuthNoticeAction};
use crate::ui_components::icons;
use crate::workspace::WorkspaceAction;

/// Glyph size inside the chip; the chip itself stays shorter than the 24 px
/// icon buttons beside it.
const CHIP_ICON_SIZE: f32 = 12.;

impl Workspace {
    /// The "Sync signed out" / "Sync login not saved" chip, or `None` while
    /// mnemos-ide-sync is signed in.
    pub(super) fn render_ide_sync_auth_chip(
        &self,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Option<Box<dyn Element>> {
        let notice = ide_sync_auth::notice(&AuthManager::as_ref(app).ide_sync_auth())?;
        let theme = appearance.theme();
        // The theme's warning token, the same one LSP warnings and the AI
        // page's policy notice use.
        let warn = theme.ui_warning_color();
        let hover_background = theme.surface_2();
        let font = appearance.ui_font_family();
        let ui_builder = appearance.ui_builder().clone();
        let label = notice.label;
        let detail = notice.detail;
        let hint = notice.action_hint.to_owned();
        let action = notice.action;

        let chip = Hoverable::new(self.mouse_states.ide_sync_auth_chip.clone(), move |state| {
            let content = Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(
                    ConstrainedBox::new(
                        icons::Icon::AlertTriangle
                            .to_mnemosui_icon(Fill::Solid(warn))
                            .finish(),
                    )
                    .with_width(CHIP_ICON_SIZE)
                    .with_height(CHIP_ICON_SIZE)
                    .finish(),
                )
                .with_child(
                    Container::new(
                        Text::new_inline(label, font, PILL_FONT_SIZE)
                            .with_color(warn)
                            .finish(),
                    )
                    .with_margin_left(4.)
                    .finish(),
                )
                .finish();
            let mut pill = Container::new(content)
                .with_border(Border::all(1.).with_border_color(warn))
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
                .with_horizontal_padding(6.)
                .with_vertical_padding(2.);
            if state.is_hovered() {
                pill = pill.with_background(hover_background);
            }
            let mut stack = Stack::new().with_child(pill.finish());
            if state.is_hovered() {
                // Right-aligned under the chip: it sits at the window's right
                // edge, and the reason can be a long line.
                stack.add_positioned_overlay_child(
                    ui_builder
                        .tool_tip_with_sublabel(detail.clone(), hint.clone())
                        .build()
                        .finish(),
                    OffsetPositioning::offset_from_parent(
                        vec2f(0., 4.),
                        ParentOffsetBounds::WindowByPosition,
                        ParentAnchor::BottomRight,
                        ChildAnchor::TopRight,
                    ),
                );
            }
            stack.finish()
        })
        .on_click(move |ctx, _, _| match action {
            SyncAuthNoticeAction::SignIn => {
                ctx.dispatch_typed_action(WorkspaceAction::SignInToIdeSync)
            }
            // The sync server refused this build's protocol: only an update helps.
            SyncAuthNoticeAction::CheckForUpdate => {
                ctx.dispatch_typed_action(WorkspaceAction::CheckForUpdate)
            }
        })
        .with_cursor(Cursor::PointingHand)
        .finish();

        Some(
            Container::new(Align::new(chip).finish())
                .with_margin_left(TAB_BAR_ICON_PADDING)
                .finish(),
        )
    }
}

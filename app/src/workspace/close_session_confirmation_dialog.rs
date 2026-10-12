use pathfinder_geometry::vector::vec2f;
use mnemos_core::ui::theme::Fill;
use mnemos_ui::{
    elements::{
        Align, ChildAnchor, Container, MouseStateHandle, OffsetPositioning, ParentAnchor,
        ParentOffsetBounds, Stack,
    },
    fonts::Weight,
    platform::Cursor,
    ui_components::{
        button::ButtonVariant,
        components::{Coords, UiComponent, UiComponentStyles},
        text::Span,
    },
    AppContext, Element, Entity, EntityId, SingletonEntity, TypedActionView, View, ViewContext,
};

use crate::{
    appearance::Appearance,
    pane_group::PaneId,
    ui_components::dialog::{dialog_styles, Dialog},
    workspace::TabMovement,
};

#[allow(clippy::enum_variant_names)]
#[derive(Clone)]
/// Describes the action which opened the close session confirmation dialog
pub enum OpenDialogSource {
    /// Close a specific pane
    ClosePane {
        pane_group_id: EntityId,
        pane_id: PaneId,
    },
    /// Close a specific tab
    CloseTab { tab_index: usize },
    /// Close all tabs other than the tab_index
    CloseOtherTabs { tab_index: usize },
    /// Close all tabs to the right/left of tab_index
    CloseTabsDirection {
        tab_index: usize,
        direction: TabMovement,
    },
    // Mnemos IDE project tabs have no variant of their own: closing one only closes
    // the project (it stays in the registry), so it goes through `CloseTab`
    // like any tab.
    /// End a CLI agent session from the Agents panel ✕ button. The
    /// `terminal_view_id_bits` is the EntityId of the terminal hosting
    /// the agent; `agent_label` and `project_label` are display strings
    /// for the dialog copy ("End claude session in Mnemos?").
    CancelCliAgent {
        terminal_view_id_bits: u64,
        agent_label: String,
        project_label: String,
    },
}

pub struct CloseSessionConfirmationDialog {
    cancel_mouse_state: MouseStateHandle,
    confirm_mouse_state: MouseStateHandle,
    dont_show_again_mouse_state: MouseStateHandle,
    dont_show_again: bool,
    // Source will be None if dialog was never opened, since there is no reasonable default
    open_confirmation_source: Option<OpenDialogSource>,
}

#[allow(dead_code)]
impl CloseSessionConfirmationDialog {
    pub fn new() -> Self {
        Self {
            cancel_mouse_state: Default::default(),
            confirm_mouse_state: Default::default(),
            dont_show_again_mouse_state: Default::default(),
            open_confirmation_source: None,
            dont_show_again: false,
        }
    }
    pub fn set_open_confirmation_source(&mut self, source: OpenDialogSource) {
        self.open_confirmation_source = Some(source);
    }

    pub fn get_open_confirmation_source(&self) -> Option<OpenDialogSource> {
        self.open_confirmation_source.clone()
    }
}

impl Entity for CloseSessionConfirmationDialog {
    type Event = CloseSessionConfirmationEvent;
}

impl View for CloseSessionConfirmationDialog {
    fn ui_name() -> &'static str {
        "CloseSessionConfirmation"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let button_style = UiComponentStyles {
            font_size: Some(14.),
            font_weight: Some(Weight::Bold),
            width: Some(202.),
            height: Some(40.),
            ..Default::default()
        };

        let dont_show_again_checkbox = appearance
            .ui_builder()
            .checkbox(self.dont_show_again_mouse_state.clone(), Some(14.))
            .with_label(Span::new("Don't show again.", Default::default()))
            .check(self.dont_show_again)
            .build()
            .with_cursor(Cursor::PointingHand)
            .on_click(|ctx, _, _| {
                ctx.dispatch_typed_action(CloseSessionConfirmationAction::ToggleDontShowAgain)
            })
            .finish();

        let dont_show_again_value = self.dont_show_again;
        // Pick a confirm-button label that matches the source. Agent cancels
        // get "End session"; everything else falls back to "Close session"
        // since the existing copy was tuned for shared-session warnings.
        let confirm_label = match &self.open_confirmation_source {
            Some(OpenDialogSource::CancelCliAgent { .. }) => "End session".to_string(),
            _ => "Close session".to_string(),
        };
        let close_session_button = appearance
            .ui_builder()
            .button(ButtonVariant::Accent, self.confirm_mouse_state.clone())
            .with_centered_text_label(confirm_label.into())
            .with_style(button_style)
            .build()
            .with_cursor(Cursor::PointingHand)
            .on_click(move |ctx, _, _| {
                ctx.dispatch_typed_action(CloseSessionConfirmationAction::CloseSession {
                    dont_show_again: dont_show_again_value,
                })
            })
            .finish();

        let cancel_button = appearance
            .ui_builder()
            .button(ButtonVariant::Basic, self.cancel_mouse_state.clone())
            .with_centered_text_label("Cancel".into())
            .with_style(button_style)
            .build()
            .with_cursor(Cursor::PointingHand)
            .on_click(move |ctx, _, _| {
                ctx.dispatch_typed_action(CloseSessionConfirmationAction::Cancel)
            })
            .finish();

        let (dialog_title, dialog_subtitle) = match &self.open_confirmation_source {
            Some(OpenDialogSource::CancelCliAgent { agent_label, project_label, .. }) => {
                let where_text = if project_label.is_empty() {
                    String::new()
                } else {
                    format!(" in \u{201C}{project_label}\u{201D}")
                };
                (
                    format!("End {agent_label} session{where_text}?"),
                    format!("Sends Ctrl-C twice to {agent_label} so the agent exits cleanly. Any in-flight tool call will be interrupted."),
                )
            }
            _ => (
                "Close session?".to_string(),
                "You are about to close a session that is currently being shared. Closing it will end sharing for everyone.".to_string(),
            ),
        };
        let dialog = Container::new(
            Dialog::new(
                dialog_title.into(),
                Some(dialog_subtitle.into()),
                UiComponentStyles {
                    width: Some(460.),
                    padding: Some(Coords::uniform(24.)),
                    ..dialog_styles(appearance)
                },
            )
            .with_child(dont_show_again_checkbox)
            .with_bottom_row_child(cancel_button)
            .with_bottom_row_child(close_session_button)
            .build()
            .finish()
        )
        .with_margin_top(35.)
        .finish();

        // Stack needed so that dialog can get bounds information,
        // specifically to ensure no overlap with the window's traffic lights
        let mut stack = Stack::new();
        stack.add_positioned_child(
            dialog,
            OffsetPositioning::offset_from_parent(
                vec2f(0., 0.),
                ParentOffsetBounds::WindowByPosition,
                ParentAnchor::Center,
                ChildAnchor::Center,
            ),
        );

        // This blurs the background and makes it uninteractable
        Container::new(Align::new(stack.finish()).finish())
            .with_background_color(Fill::blur().into())
            .with_corner_radius(app.windows().window_corner_radius())
            .finish()
    }
}

pub enum CloseSessionConfirmationEvent {
    CloseSession {
        dont_show_again: bool,
        open_confirmation_source: OpenDialogSource,
    },
    Cancel,
}

#[derive(Debug)]
pub enum CloseSessionConfirmationAction {
    CloseSession { dont_show_again: bool },
    Cancel,
    ToggleDontShowAgain,
}

impl TypedActionView for CloseSessionConfirmationDialog {
    type Action = CloseSessionConfirmationAction;

    fn handle_action(
        &mut self,
        action: &CloseSessionConfirmationAction,
        ctx: &mut ViewContext<Self>,
    ) {
        match action {
            CloseSessionConfirmationAction::CloseSession { dont_show_again } => {
                let Some(open_confirmation_source) = self.open_confirmation_source.clone() else {
                    // Should not be possible.
                    log::error!(
                        "Close session button pressed with no open confirmation dialog source"
                    );
                    return;
                };
                ctx.emit(CloseSessionConfirmationEvent::CloseSession {
                    dont_show_again: *dont_show_again,
                    open_confirmation_source,
                });
            }
            CloseSessionConfirmationAction::Cancel => {
                ctx.emit(CloseSessionConfirmationEvent::Cancel);
                self.dont_show_again = false;
            }
            CloseSessionConfirmationAction::ToggleDontShowAgain => {
                self.dont_show_again = !self.dont_show_again;
                ctx.notify();
            }
        }
    }
}

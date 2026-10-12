use crate::appearance::Appearance;
use mnemos_ui::elements::{Container, Flex, MainAxisSize, MouseStateHandle, ParentElement};
use mnemos_ui::ui_components::button::ButtonVariant;
use mnemos_ui::ui_components::components::UiComponent;
use mnemos_ui::{
    platform::Cursor, AppContext, Element, Entity, SingletonEntity, TypedActionView, View,
    ViewContext,
};

use super::style::{self, MODAL_PADDING};

const SESSION_LIMIT_SUBHEADER: &str = "You have reached the limit of shared sessions for your account.\n\nEnd a shared session to start a new one, or check your team settings.";
const OPEN_TEAM_SETTINGS_TEXT: &str = "Open team settings";

pub struct DeniedBody {
    button_mouse_state: MouseStateHandle,
}

#[derive(Debug, Clone, Copy)]
pub enum DeniedBodyAction {
    Upgrade,
}

pub enum DeniedBodyEvent {
    Upgrade,
}

impl DeniedBody {
    pub fn new(_ctx: &mut ViewContext<Self>) -> Self {
        Self {
            button_mouse_state: Default::default(),
        }
    }
}

impl Entity for DeniedBody {
    type Event = DeniedBodyEvent;
}

impl View for DeniedBody {
    fn ui_name() -> &'static str {
        "ShareModalDeniedBody"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);

        let mut col = Flex::column();
        let subheader = SESSION_LIMIT_SUBHEADER;

        let text = appearance
            .ui_builder()
            .wrappable_text(subheader, true)
            .with_style(style::subheader_styles(appearance))
            .build()
            .finish();

        let button = appearance
            .ui_builder()
            .button(ButtonVariant::Accent, self.button_mouse_state.clone())
            .with_centered_text_label(OPEN_TEAM_SETTINGS_TEXT.to_owned())
            .with_style(style::button_styles())
            .build()
            .with_cursor(Cursor::PointingHand)
            .on_click(|ctx, _, _| ctx.dispatch_typed_action(DeniedBodyAction::Upgrade))
            .finish();

        col.add_child(text);
        col.add_child(
            Container::new(button)
                .with_margin_top(MODAL_PADDING)
                .finish(),
        );
        col.with_main_axis_size(MainAxisSize::Min).finish()
    }
}

impl TypedActionView for DeniedBody {
    type Action = DeniedBodyAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            DeniedBodyAction::Upgrade => ctx.emit(DeniedBodyEvent::Upgrade),
        }
    }
}

//! Explains why a cloud agent run was queued or stopped.
//!
//! Mnemos IDE: upstream's version was an upsell (plan pricing, "Upgrade plan"
//! and "Open billing" buttons). Mnemos IDE has no plans to sell, so the modal
//! only explains what happened. Its header image (upstream art) is gone too.

use crate::ui_components::blended_colors;
use pathfinder_color::ColorU;
use pathfinder_geometry::vector::vec2f;
use mnemos_core::ui::appearance::Appearance;
use mnemos_core::ui::theme::Fill;
use mnemos_ui::elements::{
    Align, ChildAnchor, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, DropShadow,
    Expanded, Flex, FormattedTextElement, MainAxisSize, MouseStateHandle, OffsetPositioning,
    ParentAnchor, ParentElement, ParentOffsetBounds, Radius, Stack,
};
use mnemos_ui::fonts::Weight;
use mnemos_ui::keymap::FixedBinding;
use mnemos_ui::ui_components::components::UiComponent as _;
use mnemos_ui::{AppContext, Element, Entity, SingletonEntity, TypedActionView, View, ViewContext};

use crate::send_telemetry_from_ctx;
use crate::TelemetryEvent;

const MODAL_WIDTH: f32 = 360.;
const MODAL_HEIGHT: f32 = 360.;
const BUTTON_DIAMETER: f32 = 20.;

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum CloudAgentCapacityModalVariant {
    #[default]
    ConcurrentLimit,
    UsageLimitReached,
}

pub fn init(app: &mut AppContext) {
    use mnemos_ui::keymap::macros::*;

    app.register_fixed_bindings([FixedBinding::new(
        "escape",
        CloudAgentCapacityModalAction::Close,
        id!("CloudAgentCapacityModal"),
    )]);
}

#[derive(Default)]
struct StateHandles {
    close_button: MouseStateHandle,
}

pub struct CloudAgentCapacityModal {
    state_handles: StateHandles,
    variant: CloudAgentCapacityModalVariant,
}

impl CloudAgentCapacityModal {
    pub fn new() -> Self {
        CloudAgentCapacityModal {
            state_handles: Default::default(),
            variant: CloudAgentCapacityModalVariant::default(),
        }
    }

    pub fn set_variant(&mut self, variant: CloudAgentCapacityModalVariant) {
        self.variant = variant;
    }

    fn render_content(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::handle(app).as_ref(app);
        let theme = appearance.theme();
        let neutral_bg = blended_colors::neutral_1(theme);
        let (title_text, explanation_text) = match self.variant {
            CloudAgentCapacityModalVariant::ConcurrentLimit => (
                "Concurrent cloud agent limit reached",
                "This cloud run is queued because your team has reached the maximum number of concurrent cloud agents. It will start automatically when another cloud run finishes.",
            ),
            CloudAgentCapacityModalVariant::UsageLimitReached => (
                "AI request limit reached",
                "This cloud run stopped because your team has used all available AI requests for the current period.",
            ),
        };

        let title = FormattedTextElement::from_str(title_text, appearance.ui_font_family(), 24.)
            .with_color(blended_colors::text_main(theme, neutral_bg))
            .with_weight(Weight::Bold)
            .finish();

        let subtitle =
            FormattedTextElement::from_str(explanation_text, appearance.ui_font_family(), 14.)
                .with_color(blended_colors::text_sub(theme, neutral_bg))
                .finish();

        let content = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_child(Container::new(title).with_margin_bottom(12.).finish())
            .with_child(Container::new(subtitle).with_margin_bottom(16.).finish())
            .finish();
        Container::new(content).with_uniform_padding(32.).finish()
    }
}

impl Entity for CloudAgentCapacityModal {
    type Event = CloudAgentCapacityModalEvent;
}

impl View for CloudAgentCapacityModal {
    fn ui_name() -> &'static str {
        "CloudAgentCapacityModal"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::handle(app).as_ref(app);
        let theme = appearance.theme();

        let close_button = appearance
            .ui_builder()
            .close_button(BUTTON_DIAMETER, self.state_handles.close_button.clone())
            .build()
            .on_click(|ctx, _, _| ctx.dispatch_typed_action(CloudAgentCapacityModalAction::Close))
            .finish();

        let mut modal = Stack::new();
        modal.add_child(
            Container::new(
                ConstrainedBox::new(
                    Flex::column()
                        .with_main_axis_size(MainAxisSize::Max)
                        .with_child(Expanded::new(1., self.render_content(app)).finish())
                        .finish(),
                )
                .with_width(MODAL_WIDTH)
                .with_height(MODAL_HEIGHT)
                .finish(),
            )
            .with_background_color(blended_colors::neutral_1(theme))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(10.)))
            .with_drop_shadow(DropShadow::default())
            .finish(),
        );
        modal.add_positioned_child(
            close_button,
            OffsetPositioning::offset_from_parent(
                vec2f(-8., 8.),
                ParentOffsetBounds::ParentByPosition,
                ParentAnchor::TopRight,
                ChildAnchor::TopRight,
            ),
        );

        let mut stack = Stack::new();
        stack.add_positioned_child(
            modal.finish(),
            OffsetPositioning::offset_from_parent(
                vec2f(0., 0.),
                ParentOffsetBounds::WindowByPosition,
                ParentAnchor::Center,
                ChildAnchor::Center,
            ),
        );

        // Semi-transparent backdrop overlay
        Container::new(Align::new(stack.finish()).finish())
            .with_background(Fill::Solid(ColorU::new(97, 97, 97, 255)).with_opacity(50))
            .finish()
    }
}

impl TypedActionView for CloudAgentCapacityModal {
    type Action = CloudAgentCapacityModalAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            CloudAgentCapacityModalAction::Close => {
                send_telemetry_from_ctx!(TelemetryEvent::CloudAgentCapacityModalDismissed, ctx);
                ctx.emit(CloudAgentCapacityModalEvent::Close);
            }
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub enum CloudAgentCapacityModalEvent {
    Close,
}

#[derive(Clone, Debug)]
pub enum CloudAgentCapacityModalAction {
    Close,
}

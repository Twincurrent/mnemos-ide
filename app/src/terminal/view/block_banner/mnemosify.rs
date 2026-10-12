use pathfinder_color::ColorU;
use mnemos_ui::{
    elements::{
        Align, ConstrainedBox, Container, CrossAxisAlignment, Flex, HighlightedHyperlink,
        MouseStateHandle, ParentElement, Shrinkable,
    },
    fonts::Weight,
    keymap::Keystroke,
    ui_components::{
        button::ButtonVariant,
        components::{Coords, UiComponent, UiComponentStyles},
    },
    AppContext, Element,
};

use crate::{
    appearance::Appearance,
    terminal::{
        ssh::mnemosify::mnemosify_description,
        view::{RememberForMnemosification, TerminalAction},
    },
    themes::theme::Fill,
    ui_components::blended_colors,
};

use super::{render_block_banner, BLOCK_BANNER_DESCRIPTION_MAX_HEIGHT};

const CLOSE_BUTTON_DIAMETER: f32 = 20.0;
const STANDARD_PADDING: f32 = 8.0;

#[derive(Clone)]
pub enum MnemosificationMode {
    Ssh {
        command: String,
        host: Option<String>,
        hyperlink_index: HighlightedHyperlink,
    },
    Subshell {
        command: String,
    },
}

impl MnemosificationMode {
    pub fn ssh(command: String, host: Option<String>) -> Self {
        Self::Ssh {
            command,
            host,
            hyperlink_index: Default::default(),
        }
    }

    pub fn has_host(&self) -> bool {
        matches!(self, Self::Ssh { host: Some(_), .. })
    }

    pub fn subshell(command: String) -> Self {
        Self::Subshell { command }
    }
}

impl MnemosificationMode {
    pub fn is_ssh(&self) -> bool {
        matches!(self, Self::Ssh { .. })
    }
}

pub struct MnemosifyBannerState {
    pub mode: MnemosificationMode,
    pub height: f32,
    pub accept_button_mouse_state: MouseStateHandle,
    pub dont_ask_button_mouse_state: MouseStateHandle,
    pub dismiss_button_mouse_state: MouseStateHandle,

    /// This keybinding gets rendered in the Mnemosification banner, but we can't look it up
    /// during render as a &mut AppContext is not available then. This needs to get
    /// looked up during action handling and cached here.
    pub initialize_mnemosify_keybinding: Option<Keystroke>,
    pub hover_state: MouseStateHandle,
}

impl MnemosifyBannerState {
    pub fn new(mode: MnemosificationMode, initialize_mnemosify_keybinding: Option<Keystroke>) -> Self {
        Self {
            mode,
            height: 0.0,
            initialize_mnemosify_keybinding,
            accept_button_mouse_state: Default::default(),
            dont_ask_button_mouse_state: Default::default(),
            dismiss_button_mouse_state: Default::default(),
            hover_state: Default::default(),
        }
    }

    pub fn is_ssh(&self) -> bool {
        self.mode.is_ssh()
    }

    pub fn title(&self) -> &str {
        match &self.mode {
            MnemosificationMode::Ssh { .. } => "Mnemosify SSH session",
            MnemosificationMode::Subshell { .. } => "Mnemosify subshell",
        }
    }

    pub fn action(&self) -> TerminalAction {
        match &self.mode {
            MnemosificationMode::Ssh { .. } => TerminalAction::MnemosifySSHSession,
            MnemosificationMode::Subshell { .. } => TerminalAction::TriggerSubshellBootstrap,
        }
    }

    fn remember_for_mnemosification(&self, should_remember: bool) -> RememberForMnemosification {
        match &self.mode {
            MnemosificationMode::Ssh { command, host, .. } => {
                let Some(host) = host else {
                    if should_remember {
                        return RememberForMnemosification::RememberSubshellCommand(
                            command.to_owned(),
                        );
                    }
                    return RememberForMnemosification::DoNotRememberSSHHost;
                };
                if should_remember {
                    RememberForMnemosification::RememberSSHHost(host.to_owned())
                } else {
                    RememberForMnemosification::DoNotRememberSSHHost
                }
            }
            MnemosificationMode::Subshell { command } => {
                if should_remember {
                    RememberForMnemosification::RememberSubshellCommand(command.to_owned())
                } else {
                    RememberForMnemosification::DoNotRememberSubshellCommand
                }
            }
        }
    }
}

/// This banner is shown when the user runs a command which is recognized as a subshell-compatible
/// command. It asks if they want to boostrap a subshell and, if so, whether we should ask again
/// next time they run the same command.
pub fn render_mnemosification_banner(
    state: &MnemosifyBannerState,
    appearance: &Appearance,
    app: &AppContext,
) -> Box<dyn Element> {
    let yes_button = render_yes_button(
        state,
        &state.initialize_mnemosify_keybinding,
        &state.accept_button_mouse_state,
        appearance,
    );

    let remember = state.remember_for_mnemosification(true);
    let dont_ask_button = Container::new(
        appearance
            .ui_builder()
            .button(
                ButtonVariant::Text,
                state.dont_ask_button_mouse_state.clone(),
            )
            .with_text_label("Do not show again".to_owned())
            .build()
            .on_click(move |ctx, _, _| {
                ctx.dispatch_typed_action(TerminalAction::DismissMnemosifyBanner(
                    remember.to_owned(),
                ));
            })
            .finish(),
    )
    .with_margin_right(16.)
    .finish();

    let do_not_remember = state.remember_for_mnemosification(false);
    let close_button = appearance
        .ui_builder()
        .close_button(
            CLOSE_BUTTON_DIAMETER,
            state.dismiss_button_mouse_state.clone(),
        )
        .build()
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(TerminalAction::DismissMnemosifyBanner(
                do_not_remember.to_owned(),
            ));
        })
        .finish();

    let mut col = Flex::column()
        .with_child(
            Flex::row()
                .with_child(Align::new(yes_button).finish())
                .with_child(
                    Shrinkable::new(1., Align::new(dont_ask_button).right().finish()).finish(),
                )
                .with_child(Align::new(close_button).finish())
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .finish(),
        )
        .with_cross_axis_alignment(CrossAxisAlignment::Start);

    render_block_banner(
        |hover_state| {
            if let MnemosificationMode::Ssh {
                hyperlink_index, ..
            } = &state.mode
            {
                let description = Container::new(mnemosify_description(app, hyperlink_index))
                    .with_uniform_margin(STANDARD_PADDING)
                    .with_margin_top(4.)
                    .finish();
                let description = if hover_state.is_hovered() {
                    description
                } else {
                    ConstrainedBox::new(description)
                        .with_max_height(2. * BLOCK_BANNER_DESCRIPTION_MAX_HEIGHT)
                        .finish()
                };
                col.add_child(description);
            }
            col.finish()
        },
        state.hover_state.clone(),
        appearance.theme(),
    )
}

fn render_yes_button(
    state: &MnemosifyBannerState,
    initialize_mnemosification_keybinding: &Option<Keystroke>,
    mouse_state: &MouseStateHandle,
    appearance: &Appearance,
) -> Box<dyn Element> {
    let yes_button = match initialize_mnemosification_keybinding {
        Some(keystroke) => appearance
            .ui_builder()
            .keyboard_shortcut_button(state.title().to_owned(), keystroke, mouse_state.clone())
            .with_style(UiComponentStyles {
                height: Some(36.),
                padding: Some(Coords {
                    top: 0.,
                    bottom: 0.,
                    left: STANDARD_PADDING,
                    right: STANDARD_PADDING,
                }),
                ..Default::default()
            }),
        None => appearance
            .ui_builder()
            .button(ButtonVariant::Basic, mouse_state.clone())
            .with_text_label(state.title().to_owned())
            .with_style(UiComponentStyles {
                background: Some(Fill::Solid(ColorU::transparent_black()).into()),
                height: Some(36.),
                font_size: Some(appearance.ui_font_size() + 2.),
                font_weight: Some(Weight::Bold),
                font_color: Some(blended_colors::text_main(
                    appearance.theme(),
                    appearance.theme().background(),
                )),
                border_color: Some(appearance.theme().surface_3().into()),
                border_width: Some(1.),
                padding: Some(Coords::uniform(STANDARD_PADDING)),
                ..Default::default()
            })
            .with_hovered_styles(UiComponentStyles {
                background: Some(appearance.theme().surface_3().into()),
                border_color: Some(blended_colors::accent(appearance.theme()).into()),
                ..Default::default()
            }),
    };
    let action = state.action();
    yes_button
        .build()
        .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.to_owned()))
        .finish()
}

use crate::appearance::Appearance;
use crate::terminal::model::ansi::MnemosificationUnavailableReason;
use crate::terminal::mnemosify;
use crate::terminal::mnemosify::render::apply_spacing_styles;
use crate::terminal::mnemosify::render::build_description_row;
use crate::terminal::mnemosify::settings::MnemosifySettings;
use crate::ui_components::icons::Icon as UiIcon;
use markdown_parser::FormattedText;
use markdown_parser::FormattedTextFragment;
use markdown_parser::FormattedTextLine;
use mnemos_core::channel::ChannelState;
use mnemos_core::ui::theme::MnemosTheme;
use mnemos_ui::elements::HighlightedHyperlink;
use mnemos_ui::elements::Hoverable;
use mnemos_ui::elements::Icon;
use mnemos_ui::elements::MainAxisAlignment;
use mnemos_ui::elements::MainAxisSize;
use mnemos_ui::elements::MouseStateHandle;
use mnemos_ui::keymap::FixedBinding;
use mnemos_ui::platform::Cursor;
use mnemos_ui::ui_components::button::ButtonVariant;
use mnemos_ui::ui_components::components::UiComponent;
use mnemos_ui::ui_components::components::UiComponentStyles;
use mnemos_ui::AppContext;
use mnemos_ui::BlurContext;
use mnemos_ui::FocusContext;
use mnemos_ui::{
    elements::{Border, Container, CrossAxisAlignment, Flex, ParentElement},
    Element, Entity, SingletonEntity, TypedActionView, View, ViewContext,
};

const TMUX_NOT_INSTALLED_ERROR: &str =
    "tmux is not installed on the remote machine. Please install tmux and try again.";
const UNSUPPORTED_TMUX_VERSION_ERROR: &str =
    "The tmux version available on the remote machine is below 3.0. Please install tmux 3.0 or greater using a different method and try again.";
const TMUX_FAILED_ERROR: &str =
    "tmux failed to execute on the remote machine. Please re-install tmux and try again.";
const MNEMOSIFY_TIMEOUT_ERROR: &str = "Mnemosifying the session hit a timeout.";
const UNSUPPORTED_SHELL_ERROR: &str =
    "Unsupported shell. Please set bash, zsh, or fish as your default shell and try again.";
const TMUX_INSTALL_FAILED_ERROR: &str =
    "The tmux install hit an unexpected error. Please install tmux manually and try again.";

const SSH_GITHUB_ISSUE_URL: &str = crate::util::links::NEW_ISSUE_URL;

fn get_ssh_github_issue_url(title: &str) -> String {
    let url = if let Some(version) = ChannelState::app_version() {
        format!("{SSH_GITHUB_ISSUE_URL}&mnemos-version={version}")
    } else {
        SSH_GITHUB_ISSUE_URL.to_string()
    };
    // prepend the title with "SSH tmux bug report: " and uri encode it
    let title = format!("SSH tmux bug report: {title:?}");
    let title = urlencoding::encode(&title);
    format!("{url}&title={title}")
}

impl MnemosificationUnavailableReason {
    fn error_message(&self) -> &'static str {
        match self {
            MnemosificationUnavailableReason::TmuxNotInstalled { .. } => TMUX_NOT_INSTALLED_ERROR,
            MnemosificationUnavailableReason::UnsupportedTmuxVersion { .. } => {
                UNSUPPORTED_TMUX_VERSION_ERROR
            }
            MnemosificationUnavailableReason::TmuxFailed => TMUX_FAILED_ERROR,
            MnemosificationUnavailableReason::Timeout { .. } => MNEMOSIFY_TIMEOUT_ERROR,
            MnemosificationUnavailableReason::UnsupportedShell { .. } => UNSUPPORTED_SHELL_ERROR,
            MnemosificationUnavailableReason::TmuxInstallFailed { .. } => TMUX_INSTALL_FAILED_ERROR,
        }
    }

    fn error_title(&self) -> &'static str {
        match self {
            MnemosificationUnavailableReason::TmuxNotInstalled { .. } => "tmux Not Installed",
            MnemosificationUnavailableReason::UnsupportedTmuxVersion { .. } => {
                "Unsupported Tmux Version"
            }
            MnemosificationUnavailableReason::TmuxFailed => "tmux Failed",
            MnemosificationUnavailableReason::Timeout {
                is_tmux_install, ..
            } => {
                if *is_tmux_install {
                    "tmux Install Timeout"
                } else {
                    "SSH Mnemosify Timeout"
                }
            }
            MnemosificationUnavailableReason::UnsupportedShell { .. } => "Unsupported Shell",
            MnemosificationUnavailableReason::TmuxInstallFailed { .. } => "tmux Install Failed",
        }
    }
}

#[derive(Debug, Clone)]
pub enum SshErrorBlockEvent {
    ContinueWithoutMnemosification,
    MnemosifyWithoutTmux,
}

#[derive(Debug, Clone)]
pub enum SshErrorBlockAction {
    ContinueWithoutMnemosification,
    MnemosifyWithoutTmux,
    OpenUrl(String),
    AddSshHostToDenylist(String),
    Focus,
}

pub struct SshErrorBlock {
    error_reason: MnemosificationUnavailableReason,
    ssh_host: Option<String>,
    mnemosify_without_tmux_button_mouse_state: MouseStateHandle,
    continue_button_mouse_state: MouseStateHandle,
    report_link_highlight_index: HighlightedHyperlink,
    never_mnemosify_mouse_state_handle: MouseStateHandle,
    block_mouse_state: MouseStateHandle,
    is_focused: bool,
}

pub fn init(app: &mut AppContext) {
    use mnemos_ui::keymap::macros::*;

    app.register_fixed_bindings([
        FixedBinding::new(
            "enter",
            SshErrorBlockAction::MnemosifyWithoutTmux,
            id!(SshErrorBlock::ui_name()),
        ),
        FixedBinding::new(
            "escape",
            SshErrorBlockAction::ContinueWithoutMnemosification,
            id!(SshErrorBlock::ui_name()),
        ),
        FixedBinding::new(
            "ctrl-c",
            SshErrorBlockAction::ContinueWithoutMnemosification,
            id!(SshErrorBlock::ui_name()),
        ),
    ]);
}

impl SshErrorBlock {
    #[allow(clippy::new_without_default)]
    pub fn new(error_reason: MnemosificationUnavailableReason, ssh_host: Option<String>) -> Self {
        Self {
            error_reason,
            ssh_host,
            mnemosify_without_tmux_button_mouse_state: Default::default(),
            continue_button_mouse_state: Default::default(),
            report_link_highlight_index: Default::default(),
            never_mnemosify_mouse_state_handle: Default::default(),
            block_mouse_state: Default::default(),
            is_focused: false,
        }
    }

    pub fn focus(&self, ctx: &mut ViewContext<Self>) {
        ctx.focus_self();
        ctx.notify();
    }

    fn should_show_report_to_mnemos_button(&self) -> bool {
        matches!(
            self.error_reason,
            MnemosificationUnavailableReason::Timeout { .. }
                | MnemosificationUnavailableReason::TmuxInstallFailed { .. }
        )
    }

    fn render_title_ui(
        &self,
        app: &AppContext,
        theme: &MnemosTheme,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let header_contents = mnemosify::render::build_header_row(
            "Error Mnemosifying session",
            Icon::new(UiIcon::AlertTriangle.into(), theme.ui_error_color()),
            theme,
            appearance,
        )
        .with_margin_right(8.)
        .finish();

        let right_hand_size = mnemosify::render::render_never_mnemosify_ssh_link(
            &self.ssh_host,
            app,
            appearance,
            self.never_mnemosify_mouse_state_handle.clone(),
            move |ctx, ssh_host| {
                ctx.dispatch_typed_action(SshErrorBlockAction::AddSshHostToDenylist(
                    ssh_host.to_owned(),
                ));
            },
        );

        let mut row = Flex::row()
            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
            .with_cross_axis_alignment(CrossAxisAlignment::End)
            .with_main_axis_size(MainAxisSize::Max)
            .with_child(header_contents);

        if let Some(right_hand_size) = right_hand_size {
            row.add_child(right_hand_size);
        }

        mnemosify::render::apply_spacing_styles(Container::new(row.finish())).finish()
    }
}

impl Entity for SshErrorBlock {
    type Event = SshErrorBlockEvent;
}

pub const SSH_ERROR_BLOCK_VISIBLE_KEY: &str = "SshErrorBlockVisible";

impl View for SshErrorBlock {
    fn ui_name() -> &'static str {
        "SshErrorBlock"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();

        let mut content = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);

        content.add_child(self.render_title_ui(app, theme, appearance));

        content.add_child(mnemosify::render::description_row(
            self.error_reason.error_message(),
            theme,
            appearance,
        ));

        let ui_builder = appearance.ui_builder();

        if self.should_show_report_to_mnemos_button() {
            let report_issue_text = build_description_row(FormattedText::new([FormattedTextLine::Line(vec![
                    FormattedTextFragment::plain_text("We are actively working on improving the stability of SSH in Mnemos. Please consider "),
                    FormattedTextFragment::hyperlink("filing an issue", get_ssh_github_issue_url(self.error_reason.error_title())),
                    FormattedTextFragment::plain_text(" on GitHub so we can better identify the problem."),
                ])]),
                theme, appearance, self.report_link_highlight_index.clone())
                .with_hyperlink_font_color(theme.accent().into())
                .register_default_click_handlers(|link, ctx, _| {
                    ctx.dispatch_typed_action(SshErrorBlockAction::OpenUrl(link.url));
                }).finish();
            content.add_child(apply_spacing_styles(Container::new(report_issue_text)).finish());
        }

        let buttons = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_child(
                Container::new(
                    ui_builder
                        .button(
                            ButtonVariant::Accent,
                            self.mnemosify_without_tmux_button_mouse_state.clone(),
                        )
                        .with_centered_text_label("Mnemosify without TMUX".into())
                        .with_style(UiComponentStyles {
                            font_size: Some(appearance.monospace_font_size()),
                            ..Default::default()
                        })
                        .build()
                        .with_cursor(Cursor::PointingHand)
                        .on_click(move |ctx, _, _| {
                            ctx.dispatch_typed_action(SshErrorBlockAction::MnemosifyWithoutTmux)
                        })
                        .finish(),
                )
                .with_margin_right(8.)
                .finish(),
            )
            .with_child(
                ui_builder
                    .button(
                        ButtonVariant::Secondary,
                        self.continue_button_mouse_state.clone(),
                    )
                    .with_centered_text_label("Continue without Mnemosification".into())
                    .with_style(UiComponentStyles {
                        font_size: Some(appearance.monospace_font_size()),
                        ..Default::default()
                    })
                    .build()
                    .with_cursor(Cursor::PointingHand)
                    .on_click(move |ctx, _, _| {
                        ctx.dispatch_typed_action(SshErrorBlockAction::ContinueWithoutMnemosification)
                    })
                    .finish(),
            );

        content.add_child(
            Container::new(buttons.finish())
                .with_uniform_margin(20.)
                .finish(),
        );

        Hoverable::new(self.block_mouse_state.clone(), |_| {
            Container::new(content.finish())
                .with_padding_top(10.)
                .with_background(theme.foreground().with_opacity(10))
                .with_border(Border::top(1.).with_border_fill(theme.outline()))
                .finish()
        })
        .on_click(|ctx, _, _| {
            ctx.dispatch_typed_action(SshErrorBlockAction::Focus);
        })
        .finish()
    }

    fn on_focus(&mut self, focus_ctx: &FocusContext, ctx: &mut ViewContext<Self>) {
        if focus_ctx.is_self_focused() {
            self.is_focused = true;
            ctx.notify();
        }
    }

    fn on_blur(&mut self, blur_ctx: &BlurContext, ctx: &mut ViewContext<Self>) {
        if blur_ctx.is_self_blurred() {
            self.is_focused = false;
            ctx.notify();
        }
    }
}

impl TypedActionView for SshErrorBlock {
    type Action = SshErrorBlockAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            SshErrorBlockAction::MnemosifyWithoutTmux => {
                ctx.emit(SshErrorBlockEvent::MnemosifyWithoutTmux)
            }
            SshErrorBlockAction::ContinueWithoutMnemosification => {
                ctx.emit(SshErrorBlockEvent::ContinueWithoutMnemosification)
            }
            SshErrorBlockAction::OpenUrl(url) => {
                ctx.open_url(url);
            }
            SshErrorBlockAction::AddSshHostToDenylist(ssh_host) => {
                let settings = MnemosifySettings::handle(ctx);
                settings.update(ctx, |mnemosify, ctx| {
                    mnemosify.denylist_ssh_host(ssh_host, ctx);
                });
                ctx.emit(SshErrorBlockEvent::ContinueWithoutMnemosification);
                ctx.notify()
            }
            SshErrorBlockAction::Focus => {
                self.focus(ctx);
            }
        }
    }
}

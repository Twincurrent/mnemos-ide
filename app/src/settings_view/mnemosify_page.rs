use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Display;

use markdown_parser::{FormattedText, FormattedTextFragment, FormattedTextLine};
use regex::Regex;
use settings::{Setting, ToggleableSetting};
use strum::IntoEnumIterator;
use mnemos_core::features::FeatureFlag;
use mnemos_ui::elements::{FormattedTextElement, HighlightedHyperlink};
use mnemos_ui::keymap::ContextPredicate;
use mnemos_ui::{
    elements::{Container, Flex, MouseStateHandle, ParentElement},
    presenter::ChildView,
    ui_components::{
        components::{Coords, UiComponent, UiComponentStyles},
        switch::SwitchStateHandle,
    },
    Action, AppContext, Element, Entity, ModelHandle, SingletonEntity, TypedActionView, View,
    ViewContext, ViewHandle,
};

use crate::terminal::mnemosify::settings::{
    EnableSshMnemosification, SshExtensionInstallMode, UseSshTmuxWrapper, MnemosifySettingsChangedEvent,
};
use crate::ui_components::blended_colors;
use crate::{
    appearance::Appearance,
    report_if_error, send_telemetry_from_ctx,
    server::telemetry::TelemetryEvent,
    terminal::mnemosify::settings::MnemosifySettings,
    view_components::{SubmittableTextInput, SubmittableTextInputEvent},
};

use super::settings_page::{
    render_body_item, render_dropdown_item, render_page_title, AdditionalInfo, Category,
    LocalOnlyIconState, MatchData, PageType, SettingsPageEvent, SettingsWidget, ToggleState,
    HEADER_FONT_SIZE, HEADER_PADDING,
};
use super::SettingsSection;
use super::{
    flags,
    settings_page::{
        add_setting, render_alternating_color_list, SettingsPageMeta, SettingsPageViewHandle,
    },
    SettingsAction, ToggleSettingActionPair,
};
use crate::view_components::dropdown::{Dropdown, DropdownItem};

pub fn init_actions_from_parent_view<T: Action + Clone>(
    app: &mut AppContext,
    context: &ContextPredicate,
    builder: fn(SettingsAction) -> T,
) {
    // Add all of the toggle settings from the Mnemosify Page that you want to show up on the Command Palette here.
    let mut toggle_binding_pairs = vec![];

    if FeatureFlag::SSHTmuxWrapper.is_enabled() {
        toggle_binding_pairs.push(ToggleSettingActionPair::new(
            "SSH session detection for Mnemosification",
            builder(SettingsAction::MnemosifyPageToggle(
                MnemosifyPageAction::ToggleTmuxMnemosification,
            )),
            context,
            flags::SSH_TMUX_WRAPPER_CONTEXT_FLAG,
        ));
    }

    ToggleSettingActionPair::add_toggle_setting_action_pairs_as_bindings(toggle_binding_pairs, app);
}

const CONTENT_FONT_SIZE: f32 = 12.;
const ITEM_VERTICAL_SPACING: f32 = 24.;
/// There's a built-in 10px margin below the text input.
const BUILT_IN_TEXT_INPUT_MARGIN: f32 = 10.;
const SPACE_AFTER_TEXT_INPUT: f32 = ITEM_VERTICAL_SPACING - BUILT_IN_TEXT_INPUT_MARGIN;

const SSH_TMUX_MNEMOSIFICATION_DESCRIPTION: &str = "The tmux ssh wrapper works in many situations where the default one does not, but may require you to hit a button to mnemosify. Takes effect in new tabs.";

const SSH_EXTENSION_INSTALL_MODE_DESCRIPTION: &str =
    "Controls the installation behavior for Mnemos's SSH extension when a remote host doesn't have it installed.";

/// This page lets users configure when they get asked to mnemosify a session. Some shell commands
/// are recognized by default. Users can add new shell commands, or prevent the default ones from
/// asking. Users can also enable the SSH wrapper, and add hosts to a denylist.
/// This page is essentially the View for the SubshellSettings model, as well as the SshSettings
/// related to mnemosification.
pub struct MnemosifyPageView {
    page: PageType<Self>,
    /// This needs to mirror the length of SubshellSettings::added_remove_button_states.
    remove_added_command_button_states: Vec<MouseStateHandle>,
    add_added_commands_editor: ViewHandle<SubmittableTextInput>,
    /// This needs to mirror the length of SubshellSettings::denylisted_remove_button_states.
    remove_denylisted_command_button_states: Vec<MouseStateHandle>,
    add_denylisted_commands_editor: ViewHandle<SubmittableTextInput>,

    remove_denylisted_ssh_button_states: Vec<MouseStateHandle>,
    add_denylisted_ssh_editor: ViewHandle<SubmittableTextInput>,

    ssh_extension_install_mode_dropdown: ViewHandle<Dropdown<MnemosifyPageAction>>,
}

impl MnemosifyPageView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let mnemosify_settings_handle = MnemosifySettings::handle(ctx);

        ctx.observe(&mnemosify_settings_handle, Self::update_button_states);
        ctx.subscribe_to_model(&mnemosify_settings_handle, move |me, model, event, ctx| {
            me.update_button_states(model, ctx);
            if matches!(
                event,
                MnemosifySettingsChangedEvent::SshExtensionInstallMode { .. }
            ) {
                me.update_dropdown(ctx);
            }
            ctx.notify();
        });

        // Added commands can be specified by regex, while denied commands are strictly exact
        // match.
        let add_added_commands_editor = ctx.add_typed_action_view(|ctx| {
            let mut input =
                SubmittableTextInput::new(ctx).validate_on_edit(|regex| Regex::new(regex).is_ok());
            input.set_placeholder_text("command (supports regex)", ctx);
            input
        });

        ctx.subscribe_to_view(
            &add_added_commands_editor,
            Self::handle_added_command_editor_event,
        );

        let add_denylisted_commands_editor = ctx.add_typed_action_view(|ctx| {
            let mut input = SubmittableTextInput::new(ctx);
            input.set_placeholder_text("command (supports regex)", ctx);
            input
        });

        ctx.subscribe_to_view(
            &add_denylisted_commands_editor,
            Self::handle_denylisted_command_editor_event,
        );

        let add_denylisted_ssh_editor = ctx.add_typed_action_view(|ctx| {
            let mut input = SubmittableTextInput::new(ctx);
            input.set_placeholder_text("host (supports regex)", ctx);
            input
        });

        ctx.subscribe_to_view(
            &add_denylisted_ssh_editor,
            Self::handle_denylisted_ssh_editor_event,
        );

        let ssh_extension_install_mode_dropdown =
            Self::create_ssh_extension_install_mode_dropdown(ctx);

        let mut instance = Self {
            page: Self::build_page(ctx),
            remove_added_command_button_states: Default::default(),
            add_added_commands_editor,
            remove_denylisted_command_button_states: Default::default(),
            add_denylisted_commands_editor,
            remove_denylisted_ssh_button_states: Default::default(),
            add_denylisted_ssh_editor,
            ssh_extension_install_mode_dropdown,
        };

        instance.update_button_states(mnemosify_settings_handle, ctx);
        instance
    }

    fn build_page(ctx: &mut ViewContext<Self>) -> PageType<Self> {
        let mut categories = vec![
            Category::new("", vec![Box::new(TitleWidget::default())]),
            Category::new("Subshells", vec![Box::new(SubshellsWidget::default())])
                .with_subtitle("Subshells supported: bash, zsh, and fish."),
        ];

        let mnemosify_settings = MnemosifySettings::as_ref(ctx);
        if FeatureFlag::SSHTmuxWrapper.is_enabled()
            && mnemosify_settings
                .enable_ssh_mnemosification
                .is_supported_on_current_platform()
        {
            categories.push(
                Category::new("SSH", vec![Box::new(SSHWidget::default())])
                    .with_subtitle("Mnemosify your interactive SSH sessions."),
            );
        }
        PageType::new_categorized(categories, None)
    }

    /// This method ensures each command in the SubshellSettings has a matching button state for
    /// its delete button in the View.
    fn update_button_states(
        &mut self,
        mnemosify_settings_handle: ModelHandle<MnemosifySettings>,
        ctx: &mut ViewContext<Self>,
    ) {
        let mnemosify_settings = mnemosify_settings_handle.as_ref(ctx);
        self.remove_denylisted_command_button_states = mnemosify_settings
            .subshell_command_denylist
            .iter()
            .map(|_| Default::default())
            .collect();
        self.remove_added_command_button_states = mnemosify_settings
            .added_subshell_commands
            .iter()
            .map(|_| Default::default())
            .collect();
        self.remove_denylisted_ssh_button_states = mnemosify_settings
            .ssh_hosts_denylist
            .iter()
            .map(|_| Default::default())
            .collect();
        ctx.notify();
    }

    /// Syncs the install-mode dropdown selection with the current
    /// `MnemosifySettings::ssh_extension_install_mode` value (e.g. after it
    /// was changed from the SSH remote server choice view).
    fn update_dropdown(&mut self, ctx: &mut ViewContext<Self>) {
        let current_mode = *MnemosifySettings::as_ref(ctx)
            .ssh_extension_install_mode
            .value();
        self.ssh_extension_install_mode_dropdown
            .update(ctx, |dropdown, ctx| {
                dropdown.set_selected_by_action(
                    MnemosifyPageAction::SetSshExtensionInstallMode(current_mode),
                    ctx,
                );
            });
    }

    fn handle_added_command_editor_event(
        &mut self,
        _handle: ViewHandle<SubmittableTextInput>,
        event: &SubmittableTextInputEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        match event {
            SubmittableTextInputEvent::Submit(new_command) => {
                MnemosifySettings::handle(ctx).update(ctx, |mnemosify_settings, ctx| {
                    mnemosify_settings.add_subshell_command(new_command, ctx);
                });

                send_telemetry_from_ctx!(TelemetryEvent::AddAddedSubshellCommand, ctx);
            }
            SubmittableTextInputEvent::Escape => ctx.emit(SettingsPageEvent::FocusModal),
        }
    }

    fn handle_denylisted_command_editor_event(
        &mut self,
        _handle: ViewHandle<SubmittableTextInput>,
        event: &SubmittableTextInputEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        match event {
            SubmittableTextInputEvent::Submit(new_command) => {
                MnemosifySettings::handle(ctx).update(ctx, |mnemosify_settings, ctx| {
                    mnemosify_settings.denylist_subshell_command(new_command, ctx);
                });

                send_telemetry_from_ctx!(TelemetryEvent::AddDenylistedSubshellCommand, ctx);
            }
            SubmittableTextInputEvent::Escape => ctx.emit(SettingsPageEvent::FocusModal),
        }
    }

    fn handle_denylisted_ssh_editor_event(
        &mut self,
        _handle: ViewHandle<SubmittableTextInput>,
        event: &SubmittableTextInputEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        match event {
            SubmittableTextInputEvent::Submit(new_command) => {
                MnemosifySettings::handle(ctx).update(ctx, |mnemosify_settings, ctx| {
                    mnemosify_settings.denylist_ssh_host(new_command, ctx);
                });

                send_telemetry_from_ctx!(TelemetryEvent::AddDenylistedSshTmuxWrapperHost, ctx);
            }
            SubmittableTextInputEvent::Escape => ctx.emit(SettingsPageEvent::FocusModal),
        }
    }

    fn remove_denylisted_command(&self, index: usize, ctx: &mut ViewContext<Self>) {
        send_telemetry_from_ctx!(TelemetryEvent::RemoveDenylistedSubshellCommand, ctx);
        MnemosifySettings::handle(ctx).update(ctx, |mnemosify, ctx| {
            mnemosify.remove_denylisted_subshell_command(index, ctx)
        });
    }

    fn remove_added_command(&self, index: usize, ctx: &mut ViewContext<Self>) {
        send_telemetry_from_ctx!(TelemetryEvent::RemoveAddedSubshellCommand, ctx);
        MnemosifySettings::handle(ctx).update(ctx, |mnemosify, ctx| {
            mnemosify.remove_added_subshell_command(index, ctx)
        });
    }

    fn remove_denylisted_ssh_host(&self, index: usize, ctx: &mut ViewContext<Self>) {
        send_telemetry_from_ctx!(TelemetryEvent::RemoveDenylistedSshTmuxWrapperHost, ctx);
        MnemosifySettings::handle(ctx).update(ctx, |mnemosify, ctx| {
            mnemosify.remove_denylisted_ssh_host(index, ctx)
        });
    }
}

impl Entity for MnemosifyPageView {
    type Event = SettingsPageEvent;
}

fn build_sub_sub_title(title: &str, appearance: &Appearance) -> Container {
    appearance
        .ui_builder()
        .span(title.to_string())
        .with_style(UiComponentStyles {
            font_size: Some(CONTENT_FONT_SIZE),
            ..Default::default()
        })
        .build()
}

const SSH_EXTENSION_DROPDOWN_WIDTH: f32 = 250.;

impl MnemosifyPageView {
    fn create_ssh_extension_install_mode_dropdown(
        ctx: &mut ViewContext<Self>,
    ) -> ViewHandle<Dropdown<MnemosifyPageAction>> {
        let items: Vec<DropdownItem<MnemosifyPageAction>> = SshExtensionInstallMode::iter()
            .map(|mode| {
                DropdownItem::new(
                    mode.display_name(),
                    MnemosifyPageAction::SetSshExtensionInstallMode(mode),
                )
            })
            .collect();

        let current_mode = *MnemosifySettings::as_ref(ctx)
            .ssh_extension_install_mode
            .value();
        let enable_ssh_mnemosification = *MnemosifySettings::as_ref(ctx)
            .enable_ssh_mnemosification
            .value();

        ctx.add_typed_action_view(move |ctx| {
            let mut dropdown = Dropdown::new(ctx);
            dropdown.set_top_bar_max_width(SSH_EXTENSION_DROPDOWN_WIDTH);
            dropdown.set_menu_width(SSH_EXTENSION_DROPDOWN_WIDTH, ctx);
            dropdown.add_items(items, ctx);
            dropdown.set_selected_by_action(
                MnemosifyPageAction::SetSshExtensionInstallMode(current_mode),
                ctx,
            );
            if !enable_ssh_mnemosification {
                dropdown.set_disabled(ctx);
            }
            dropdown
        })
    }

    /// Renders a title, a list of items that can be removed, and an input field to add new items.
    fn build_input_list<
        ListItem: Display,
        SettingsPageAction: Action + Clone,
        F: Fn(usize) -> SettingsPageAction,
        T: View,
    >(
        &self,
        title: &str,
        patterns: &[ListItem],
        mouse_states: &[MouseStateHandle],
        create_action: F,
        handle: &ViewHandle<T>,
        appearance: &Appearance,
    ) -> Container {
        let mut column = Flex::column();
        let mut title = build_sub_sub_title(title, appearance);

        if !patterns.is_empty() {
            title = title.with_padding_bottom(BUILT_IN_TEXT_INPUT_MARGIN);
        }

        column.add_child(title.finish());

        render_alternating_color_list(
            &mut column,
            patterns,
            mouse_states,
            create_action,
            appearance,
        );

        Container::new(
            column
                .with_child(
                    Container::new(ChildView::new(handle).finish())
                        .with_margin_bottom(SPACE_AFTER_TEXT_INPUT)
                        .finish(),
                )
                .finish(),
        )
    }
}

impl View for MnemosifyPageView {
    fn ui_name() -> &'static str {
        "MnemosifyPageView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum MnemosifyPageAction {
    RemoveAddedCommand(usize),
    RemoveDenylistedCommand(usize),
    RemoveDenylistedSshHost(usize),
    /// If disabled, auto-Mnemosification and the SSH Mnemosification prompt will be disabled.
    ToggleTmuxMnemosification,
    ToggleSshMnemosification,
    /// Set the SSH extension installation mode (always ask / always install / always skip).
    SetSshExtensionInstallMode(SshExtensionInstallMode),
    OpenUrl(String),
}

impl TypedActionView for MnemosifyPageView {
    type Action = MnemosifyPageAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        use MnemosifyPageAction::*;
        match action {
            RemoveDenylistedCommand(index) => self.remove_denylisted_command(*index, ctx),
            RemoveAddedCommand(index) => self.remove_added_command(*index, ctx),
            ToggleSshMnemosification => {
                MnemosifySettings::handle(ctx).update(ctx, |ssh_settings, ctx| {
                    report_if_error!(ssh_settings
                        .enable_ssh_mnemosification
                        .toggle_and_save_value(ctx));
                    send_telemetry_from_ctx!(
                        TelemetryEvent::ToggleSshMnemosification {
                            enabled: *ssh_settings.enable_ssh_mnemosification.value(),
                        },
                        ctx
                    );
                });
                let enabled = *MnemosifySettings::as_ref(ctx)
                    .enable_ssh_mnemosification
                    .value();
                self.ssh_extension_install_mode_dropdown
                    .update(ctx, |dropdown, ctx| {
                        if enabled {
                            dropdown.set_enabled(ctx);
                        } else {
                            dropdown.set_disabled(ctx);
                        }
                    });
            }
            ToggleTmuxMnemosification => {
                MnemosifySettings::handle(ctx).update(ctx, |ssh_settings, ctx| {
                    report_if_error!(ssh_settings.use_ssh_tmux_wrapper.toggle_and_save_value(ctx));
                    send_telemetry_from_ctx!(
                        TelemetryEvent::ToggleSshTmuxWrapper {
                            enabled: *ssh_settings.use_ssh_tmux_wrapper.value(),
                        },
                        ctx
                    );
                });
            }
            SetSshExtensionInstallMode(mode) => {
                MnemosifySettings::handle(ctx).update(ctx, |mnemosify_settings, ctx| {
                    report_if_error!(mnemosify_settings
                        .ssh_extension_install_mode
                        .set_value(*mode, ctx));
                    send_telemetry_from_ctx!(
                        TelemetryEvent::SetSshExtensionInstallMode {
                            mode: mode.display_name(),
                        },
                        ctx
                    );
                });
            }
            MnemosifyPageAction::RemoveDenylistedSshHost(index) => {
                self.remove_denylisted_ssh_host(*index, ctx);
            }
            OpenUrl(url) => {
                ctx.open_url(url.as_str());
            }
        }
    }
}

impl SettingsPageMeta for MnemosifyPageView {
    fn section() -> SettingsSection {
        SettingsSection::Mnemosify
    }

    fn should_render(&self, _ctx: &AppContext) -> bool {
        true
    }

    fn update_filter(&mut self, query: &str, ctx: &mut ViewContext<Self>) -> MatchData {
        self.page.update_filter(query, ctx)
    }

    fn scroll_to_widget(&mut self, widget_id: &'static str) {
        self.page.scroll_to_widget(widget_id)
    }

    fn clear_highlighted_widget(&mut self) {
        self.page.clear_highlighted_widget();
    }
}

impl From<ViewHandle<MnemosifyPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<MnemosifyPageView>) -> Self {
        SettingsPageViewHandle::Mnemosify(view_handle)
    }
}

#[derive(Default)]
struct TitleWidget {
    learn_more_highlight_index: HighlightedHyperlink,
}

impl TitleWidget {
    fn render_top_of_page(&self, appearance: &Appearance, _app: &AppContext) -> Box<dyn Element> {
        let mnemosify_description = vec![
            FormattedTextFragment::plain_text(
                "Configure whether Mnemos attempts to “Mnemosify” (add support for blocks, \
                    input modes, etc) certain shells. ",
            ),
            // Mnemos IDE: upstream left this link empty.
            FormattedTextFragment::hyperlink(
                "Learn more",
                crate::util::links::MNEMOS_DOCS_URL,
            ),
        ];

        let mnemosify_description = FormattedTextElement::new(
            FormattedText::new([FormattedTextLine::Line(mnemosify_description)]),
            CONTENT_FONT_SIZE,
            appearance.ui_font_family(),
            appearance.ui_font_family(),
            blended_colors::text_sub(appearance.theme(), appearance.theme().surface_1()),
            self.learn_more_highlight_index.clone(),
        )
        .with_hyperlink_font_color(appearance.theme().accent().into_solid())
        .register_default_click_handlers(|url, _, ctx| {
            ctx.open_url(&url.url);
        })
        .finish();

        Flex::column()
            .with_child(render_page_title("Mnemosify", HEADER_FONT_SIZE, appearance))
            .with_child(mnemosify_description)
            .finish()
    }
}

impl SettingsWidget for TitleWidget {
    type View = MnemosifyPageView;

    fn search_terms(&self) -> &str {
        "ssh subshell mnemosify session"
    }

    fn render(
        &self,
        _view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        Container::new(self.render_top_of_page(appearance, app))
            .with_margin_bottom(ITEM_VERTICAL_SPACING)
            .finish()
    }
}

#[derive(Default)]
struct SubshellsWidget {}

impl SubshellsWidget {
    fn render_subshells_section(
        &self,
        view: &MnemosifyPageView,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let mut column = Flex::column();

        let mnemosify_settings = MnemosifySettings::as_ref(app);

        column.add_child(
            view.build_input_list(
                "Added commands",
                &mnemosify_settings.added_subshell_commands,
                &view.remove_added_command_button_states,
                MnemosifyPageAction::RemoveAddedCommand,
                &view.add_added_commands_editor,
                appearance,
            )
            .finish(),
        );

        column.add_child(
            view.build_input_list(
                "Denylisted commands",
                &mnemosify_settings.subshell_command_denylist,
                &view.remove_denylisted_command_button_states,
                MnemosifyPageAction::RemoveDenylistedCommand,
                &view.add_denylisted_commands_editor,
                appearance,
            )
            .with_margin_bottom(-BUILT_IN_TEXT_INPUT_MARGIN)
            .finish(),
        );

        column.finish()
    }
}

impl SettingsWidget for SubshellsWidget {
    type View = MnemosifyPageView;

    fn search_terms(&self) -> &str {
        "mnemosify subshell"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        Container::new(self.render_subshells_section(view, appearance, app))
            .with_margin_bottom(ITEM_VERTICAL_SPACING)
            .finish()
    }
}

#[derive(Default)]
struct SSHWidget {
    tmux_mnemosification_switch_state: SwitchStateHandle,
    enable_ssh_mnemosification_switch_state: SwitchStateHandle,
    additional_info_mouse_state: MouseStateHandle,
    local_only_icon_tooltip_states: RefCell<HashMap<String, MouseStateHandle>>,
}

impl SettingsWidget for SSHWidget {
    type View = MnemosifyPageView;

    fn search_terms(&self) -> &str {
        "mnemosify ssh"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let mut column = Flex::column();
        let ui_builder = appearance.ui_builder();
        let description_text_color = appearance
            .theme()
            .sub_text_color(appearance.theme().surface_2());

        let enable_ssh_mnemosification = *MnemosifySettings::as_ref(app)
            .enable_ssh_mnemosification
            .value();

        let should_prompt_ssh_tmux_wrapper =
            *MnemosifySettings::as_ref(app).use_ssh_tmux_wrapper.value();

        add_setting(
            &mut column,
            &MnemosifySettings::as_ref(app).enable_ssh_mnemosification,
            move || {
                render_body_item::<MnemosifyPageAction>(
                    "Mnemosify SSH Sessions".into(),
                    None,
                    LocalOnlyIconState::for_setting(
                        EnableSshMnemosification::storage_key(),
                        EnableSshMnemosification::sync_to_cloud(),
                        &mut self.local_only_icon_tooltip_states.borrow_mut(),
                        app,
                    ),
                    ToggleState::Enabled,
                    appearance,
                    ui_builder
                        .switch(self.enable_ssh_mnemosification_switch_state.clone())
                        .check(enable_ssh_mnemosification)
                        .build()
                        .on_click(move |ctx, _, _| {
                            ctx.dispatch_typed_action(MnemosifyPageAction::ToggleSshMnemosification);
                        })
                        .finish(),
                    None,
                )
            },
        );

        if FeatureFlag::SshRemoteServer.is_enabled() {
            let label_color_override = if !enable_ssh_mnemosification {
                Some(appearance.theme().disabled_ui_text_color())
            } else {
                None
            };
            add_setting(
                &mut column,
                &MnemosifySettings::as_ref(app).ssh_extension_install_mode,
                move || {
                    Container::new(render_dropdown_item(
                        appearance,
                        "Install SSH extension",
                        Some(SSH_EXTENSION_INSTALL_MODE_DESCRIPTION),
                        None,
                        LocalOnlyIconState::for_setting(
                            SshExtensionInstallMode::storage_key(),
                            SshExtensionInstallMode::sync_to_cloud(),
                            &mut self.local_only_icon_tooltip_states.borrow_mut(),
                            app,
                        ),
                        label_color_override,
                        &view.ssh_extension_install_mode_dropdown,
                    ))
                    .with_padding_bottom(HEADER_PADDING)
                    .finish()
                },
            );
        }

        add_setting(
            &mut column,
            &MnemosifySettings::as_ref(app).use_ssh_tmux_wrapper,
            move || {
                let mut column = Flex::column();

                column.add_child(render_body_item::<MnemosifyPageAction>(
                    "Use Tmux Mnemosification".into(),
                    Some(AdditionalInfo {
                        mouse_state: self.additional_info_mouse_state.clone(),
                        on_click_action: Some(MnemosifyPageAction::OpenUrl(
                            "".into(),
                        )),
                        secondary_text: None,
                        tooltip_override_text: None,
                    }),
                    LocalOnlyIconState::for_setting(
                        UseSshTmuxWrapper::storage_key(),
                        UseSshTmuxWrapper::sync_to_cloud(),
                        &mut self.local_only_icon_tooltip_states.borrow_mut(),
                        app,
                    ),
                    enable_ssh_mnemosification.into(),
                    appearance,
                    ui_builder
                        .switch(self.tmux_mnemosification_switch_state.clone())
                        .check(should_prompt_ssh_tmux_wrapper)
                        .with_disabled(!enable_ssh_mnemosification)
                        .build()
                        .on_click(move |ctx, _, _| {
                            if !enable_ssh_mnemosification {
                                return;
                            }

                            ctx.dispatch_typed_action(MnemosifyPageAction::ToggleTmuxMnemosification);
                        })
                        .finish(),
                    None,
                ));

                column.add_child(
                    ui_builder
                        .paragraph(SSH_TMUX_MNEMOSIFICATION_DESCRIPTION.to_owned())
                        .with_style(UiComponentStyles {
                            font_color: Some(description_text_color.into_solid()),
                            margin: Some(
                                Coords::default()
                                    .top(styles::DESCRIPTION_NEGATIVE_MARGIN_OFFSET)
                                    .bottom(styles::DESCRIPTION_LINE_MARGIN_BOTTOM),
                            ),
                            ..Default::default()
                        })
                        .build()
                        .finish(),
                );

                if enable_ssh_mnemosification && should_prompt_ssh_tmux_wrapper {
                    let mnemosify_settings = MnemosifySettings::as_ref(app);
                    column.add_child(
                        view.build_input_list(
                            "Denylisted hosts",
                            &mnemosify_settings.ssh_hosts_denylist,
                            &view.remove_denylisted_ssh_button_states,
                            MnemosifyPageAction::RemoveDenylistedSshHost,
                            &view.add_denylisted_ssh_editor,
                            appearance,
                        )
                        .finish(),
                    );
                } else {
                    // Add margin to hint the user should scroll to see more.
                    column.add_child(
                        Container::new(Flex::column().finish())
                            .with_margin_bottom(styles::MINIMUM_SCROLL_OFFSET_AFTER_SSH)
                            .finish(),
                    );
                }

                column.finish()
            },
        );

        column.finish()
    }
}

mod styles {
    // Apply a negative margin to the description text so it appears closer to the main
    // settings option text.
    pub const DESCRIPTION_NEGATIVE_MARGIN_OFFSET: f32 = -8.;

    /// The space after a description.
    pub const DESCRIPTION_LINE_MARGIN_BOTTOM: f32 = 18.;

    /// Because we hide the SSH settings if the SSH wrapper is disabled, we need to add a margin
    /// to the bottom to make it clear that toggling this item will reveal more settings,
    /// even at smaller window sizes. We picked an offset that cuts off the first item
    /// to imply the user should scroll to see more.
    pub const MINIMUM_SCROLL_OFFSET_AFTER_SSH: f32 = 40.;
}

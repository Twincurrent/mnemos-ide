use std::borrow::Cow;
use std::sync::Arc;

use crate::ai::agent::ProgrammingLanguage;
use crate::ai::blocklist::code_block::{render_runnable_code_snippet, CodeSnippetButtonHandles};
use crate::appearance::Appearance;
use crate::terminal::model::terminal_model::SubshellInitializationInfo;
use crate::terminal::shell::{Shell, ShellType};
use crate::ui_components::blended_colors;
use crate::ui_components::icons::Icon as UiIcon;
use crate::workspace::WorkspaceAction;
use channel_versions::overrides::TargetOS;
use parking_lot::RwLock;
use mnemos_core::semantic_selection::SemanticSelection;
use mnemos_core::ui::theme::MnemosTheme;
use mnemos_ui::elements::{
    CrossAxisAlignment, Icon, MainAxisAlignment, MainAxisSize, MouseStateHandle, SelectableArea,
    SelectionHandle, Text,
};
use mnemos_ui::ui_components::components::{UiComponent, UiComponentStyles};
use mnemos_ui::{
    elements::{Border, Container, Flex, ParentElement},
    AppContext, Element, Entity, SingletonEntity, TypedActionView, View, ViewContext,
};

use super::render::{HORIZONTAL_TEXT_MARGIN, SSH_DOCS_URL, SUBSHELL_DOCS_URL};
use super::settings::MnemosifySettings;
use super::{render, subshell_bootstrap_success_block_bytes, MnemosificationSource};

const VERTICAL_TEXT_MARGIN: f32 = 16.;

#[derive(Debug, Clone)]
pub enum MnemosifySuccessBlockEvent {
    OpenMnemosifySettings,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum MnemosifySuccessBlockAction {
    ClearAutoMnemosifySnippet,
    OpenMnemosifySettings,
    OpenUrl(String),
}

struct AutoMnemosifySnippet {
    /// On subshell initialization, this will contain the output grid to display,
    /// containing info like how to auto-mnemosify the subshell.
    output_grid: Cow<'static, str>,
    /// The output grid needs to be selectable to allow users to copy the command to their clipboard.
    selection_handle: SelectionHandle,
    selected_text: Arc<RwLock<Option<String>>>,

    shell_type: ShellType,
    description: Cow<'static, str>,
    code_snippet_handles: CodeSnippetButtonHandles,
    can_write_to_rc: bool,
}

pub struct MnemosifySuccessBlock {
    source: MnemosificationSource,
    spawning_command: String,
    learn_more_link_mouse_states: MouseStateHandle,
    auto_mnemosify_snippet: Option<AutoMnemosifySnippet>,
}

impl MnemosifySuccessBlock {
    #[allow(clippy::new_without_default)]
    pub fn new(
        source: MnemosificationSource,
        spawning_command: String,
        subshell_info: Option<SubshellInitializationInfo>,
        shell: Shell,
        disable_tmux: bool,
        ctx: &mut ViewContext<Self>,
    ) -> Self {
        ctx.subscribe_to_model(&MnemosifySettings::handle(ctx), move |_, _, _, ctx| {
            ctx.notify();
        });

        // Mac + Linux have the same behavior. We'd need to handle
        // getting the OS to write to the correct RC file.
        let remote_os = TargetOS::Linux;

        let is_auto_mnemosify_configured = subshell_info
            .as_ref()
            .map(|info| info.was_triggered_by_rc_file_snippet)
            .unwrap_or_default();

        let auto_mnemosify_snippet = if is_auto_mnemosify_configured {
            None
        } else {
            subshell_info.and_then(|subshell_info| {
                // If mnemosification wasn't triggered automatically, show a snippet about
                // how to automatically mnemosify.
                (!subshell_info.was_triggered_by_rc_file_snippet).then(|| {
                    let (command, is_executable) = subshell_bootstrap_success_block_bytes(
                        &subshell_info,
                        shell.shell_type(),
                        remote_os,
                        disable_tmux,
                    );
                    if command.is_empty() {
                        return ("".into(), false);
                    }
                    (
                        String::from_utf8(command)
                            .map(|content| {
                                // Ensure a blank line between the output grid and the learn more link.
                                content + "\n"
                            })
                            .unwrap_or_default(),
                        is_executable,
                    )
                })
            })
        };
        let auto_mnemosify_snippet = auto_mnemosify_snippet.map(|(output_grid, can_write_to_rc)| {
            AutoMnemosifySnippet {
                description: (if !output_grid.is_empty() {
                    "Run the following to automatically Mnemosify in the future:"
                } else {
                    "In remote subshells, Mnemos runs commands in the background to power completions, syntax highlighting, and other features."
                }).into(),
                output_grid: output_grid.into(),
                selection_handle: Default::default(),
                selected_text: Default::default(),
                code_snippet_handles: Default::default(),
                shell_type: shell.shell_type(),
                can_write_to_rc,
            }
        });

        Self {
            source,
            learn_more_link_mouse_states: Default::default(),
            spawning_command,
            auto_mnemosify_snippet,
        }
    }

    pub fn selected_text(&self) -> Option<String> {
        self.auto_mnemosify_snippet
            .as_ref()
            .and_then(|snippet| snippet.selected_text.read().clone())
    }

    pub fn render_spawning_command(
        &self,
        theme: &MnemosTheme,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let spawning_command = self.spawning_command.clone();
        render::build_command_row(spawning_command, theme, appearance, true)
            .with_margin_bottom(VERTICAL_TEXT_MARGIN)
            .finish()
    }

    pub fn render_title_ui(&self, theme: &MnemosTheme, appearance: &Appearance) -> Box<dyn Element> {
        let header_contents = render::build_header_row(
            "Session Mnemosified",
            Icon::new(UiIcon::Mnemos.into(), theme.active_ui_detail()),
            theme,
            appearance,
        )
        .with_margin_right(8.)
        .finish();
        let header_contents = Container::new(
            Flex::row()
                .with_children([header_contents, self.render_learn_more_link(appearance)])
                .finish(),
        )
        .finish();

        Container::new(
            Flex::row()
                .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                .with_cross_axis_alignment(CrossAxisAlignment::End)
                .with_main_axis_size(MainAxisSize::Max)
                .with_child(header_contents)
                .finish(),
        )
        .with_horizontal_margin(HORIZONTAL_TEXT_MARGIN)
        .with_margin_top(VERTICAL_TEXT_MARGIN)
        .finish()
    }

    fn render_learn_more_link(&self, appearance: &Appearance) -> Box<dyn Element> {
        let url = match self.source {
            MnemosificationSource::Ssh => SSH_DOCS_URL,
            MnemosificationSource::Subshell => SUBSHELL_DOCS_URL,
        };

        let font_family_id = appearance.monospace_font_family();
        let font_size = appearance.monospace_font_size();
        appearance
            .ui_builder()
            .link(
                "Learn more".into(),
                None,
                Some(Box::new({
                    move |ctx| {
                        ctx.dispatch_typed_action(MnemosifySuccessBlockAction::OpenUrl(
                            url.to_owned(),
                        ));
                    }
                })),
                self.learn_more_link_mouse_states.clone(),
            )
            .soft_wrap(false)
            .with_style(UiComponentStyles {
                font_size: Some(font_size),
                font_family_id: Some(font_family_id),
                ..Default::default()
            })
            .build()
            .finish()
    }

    /// Fired when a block ends and we are not in a Mnemosified session.
    pub fn on_mnemosified_session_complete(&mut self, ctx: &mut ViewContext<Self>) {
        self.clear_auto_mnemosify_snippet(ctx);
    }

    pub fn clear_auto_mnemosify_snippet(&mut self, ctx: &mut ViewContext<Self>) {
        self.auto_mnemosify_snippet = None;
        ctx.notify();
    }

    /// If there is an output grid to display, render it.
    pub fn render_output_grid(
        &self,
        app: &AppContext,
        appearance: &Appearance,
    ) -> Option<Box<dyn Element>> {
        let theme = appearance.theme();
        let auto_mnemosify_snippet = self.auto_mnemosify_snippet.as_ref()?;

        if auto_mnemosify_snippet.output_grid.is_empty() {
            return None;
        }

        let shell_language = ProgrammingLanguage::Shell(auto_mnemosify_snippet.shell_type);
        let runnable_command = render_runnable_code_snippet(
            &auto_mnemosify_snippet.output_grid,
            if auto_mnemosify_snippet.can_write_to_rc {
                Some(&shell_language)
            } else {
                None
            },
            Some(Box::new({
                move |code_snippet, ctx| {
                    ctx.dispatch_typed_action(WorkspaceAction::RunCommand(
                        code_snippet.to_string(),
                    ));

                    ctx.dispatch_typed_action(MnemosifySuccessBlockAction::ClearAutoMnemosifySnippet);
                }
            })),
            Some(Box::new({
                move |code_snippet, ctx| {
                    ctx.dispatch_typed_action(WorkspaceAction::CopyTextToClipboard(code_snippet));
                }
            })),
            Some(auto_mnemosify_snippet.code_snippet_handles.clone()),
            app,
        );

        let semantic_selection = SemanticSelection::as_ref(app);
        let selected_text = auto_mnemosify_snippet.selected_text.clone();

        // TODO(Simon): Implement full selection and copying functionality for the MnemosifySuccessBlock.
        // Look to the `EnvVarCollectionBlock` for the existing implementation paradigm. We don't
        // yet have a robust way of ensuring that every aspect of text selection is implemented
        // properly, so be extra careful not to miss any details!
        let output_grid = SelectableArea::new(
            auto_mnemosify_snippet.selection_handle.clone(),
            move |selection_args, _, _| {
                *selected_text.write() = selection_args.selection;
            },
            runnable_command,
        )
        .with_word_boundaries_policy(semantic_selection.word_boundary_policy())
        .with_smart_select_fn(semantic_selection.smart_select_fn())
        .finish();

        let output_grid = Flex::column()
            .with_child(
                Container::new(
                    Text::new(
                        auto_mnemosify_snippet.description.clone(),
                        appearance.monospace_font_family(),
                        appearance.monospace_font_size(),
                    )
                    .with_color(blended_colors::text_main(theme, theme.background()))
                    .finish(),
                )
                .with_horizontal_margin(HORIZONTAL_TEXT_MARGIN)
                .with_margin_bottom(VERTICAL_TEXT_MARGIN)
                .finish(),
            )
            .with_child(
                Container::new(output_grid)
                    .with_horizontal_margin(HORIZONTAL_TEXT_MARGIN)
                    .with_margin_bottom(VERTICAL_TEXT_MARGIN)
                    .finish(),
            )
            .finish();
        Some(output_grid)
    }
}

impl Entity for MnemosifySuccessBlock {
    type Event = MnemosifySuccessBlockEvent;
}

pub const MNEMOSIFY_SUCCESS_BLOCK_VISIBLE_KEY: &str = "MnemosifySuccessBlockVisible";

impl View for MnemosifySuccessBlock {
    fn ui_name() -> &'static str {
        "MnemosifySuccessBlock"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();

        let mut content = Flex::column();

        content.add_children([
            self.render_title_ui(theme, appearance),
            self.render_spawning_command(theme, appearance),
        ]);

        if let Some(output_grid) = self.render_output_grid(app, appearance) {
            content.add_child(output_grid);
        }

        Container::new(content.finish())
            .with_background(theme.foreground().with_opacity(10))
            .with_border(Border::top(1.).with_border_fill(theme.outline()))
            .finish()
    }
}

impl TypedActionView for MnemosifySuccessBlock {
    type Action = MnemosifySuccessBlockAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            MnemosifySuccessBlockAction::OpenMnemosifySettings => {
                ctx.emit(MnemosifySuccessBlockEvent::OpenMnemosifySettings);
            }
            MnemosifySuccessBlockAction::OpenUrl(url) => {
                ctx.open_url(url);
            }
            MnemosifySuccessBlockAction::ClearAutoMnemosifySnippet => {
                self.clear_auto_mnemosify_snippet(ctx);
            }
        }
    }
}

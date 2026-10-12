// Suppress warnings about rustdoc style.
#![allow(clippy::doc_lazy_continuation)]

mod ai;
mod alloc;
#[cfg(target_os = "macos")]
mod app_menus;
mod app_services;
mod app_state;
mod auth;
mod autoupdate;
mod banner;
mod changelog_model;
mod chip_configurator;
mod cloud_object;
mod code;
mod code_review;
mod coding_entrypoints;
mod coding_panel_enablement_state;
mod command_palette;
mod completer;
#[allow(dead_code)]
mod context_chips;
#[cfg(enable_crash_recovery)]
mod crash_recovery;
#[cfg(feature = "crash_reporting")]
mod crash_reporting;
mod debounce;
mod debug_dump;
mod default_terminal;
mod download_method;
mod drive;
#[cfg(windows)]
mod dynamic_libraries;
mod env_vars;
mod external_secrets;
#[cfg(target_family = "wasm")]
mod font_fallback;
mod global_resource_handles;
mod gpu_state;
mod input_classifier;
mod interval_timer;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod login_item;
mod menu;
mod modal;
mod network;
mod notebooks;
mod notification;
mod palette;
mod persistence;
mod platform;
mod prefix;
#[cfg(target_os = "macos")]
mod preview_config_migration;
mod profiling;
mod projects;
mod prompt;
mod quit_warning;
#[allow(dead_code)]
mod remote_server;
mod resource_limits;
mod safe_triangle;
mod search_bar;
mod server;
mod session_management;
mod shell_indicator;
mod suggestions;
mod system;
mod tab;
#[cfg(test)]
mod test_util;
mod throttle;
mod tips;
mod tracing;
mod ui_components;
mod undo_close;
mod uri;
mod user_config;
pub mod util;
mod view_components;
mod vim_registers;
mod voltron;
mod mnemos_managed_paths_watcher;
mod window_settings;
mod word_block_editor;
mod workspaces;

// PLEASE DO NOT ADD MORE PUBLIC MODULES!
//
// Any modules which we make public outside of the `mnemos` crate lose dead code
// checking support, as the compiler cannot make any assumptions about whether
// or not the function/type is used by another crate that pulls in this one as
// a dependency.
//
// If you feel the need to export a module so that a type or function within it
// can be used by an integration test, you should define a new assertion function
// in the mnemos::integration_testing::assertions module (or a sub-module).  These
// functions will allow us to keep types internal to this crate and expose a
// simpler API for integration tests to consume.
pub mod ai_assistant;
pub mod appearance;
pub mod channel;
pub mod editor;
pub mod features;
pub mod input_suggestions;
#[cfg(feature = "integration_tests")]
pub mod integration_testing;
pub mod keyboard;
pub mod launch_configs;
pub mod pane_group;
pub mod resource_center;
pub mod root_view;
pub mod search;
pub mod settings;
pub mod settings_view;
pub mod tab_configs;
pub mod terminal;
pub mod themes;
use crate::ai::active_agent_views_model::ActiveAgentViewsModel;
#[cfg(not(target_family = "wasm"))]
use crate::ai::aws_credentials::AwsCredentialRefresher as _;
use crate::ai::mcp::FileBasedMCPManager;
use crate::ai::mcp::FileMCPWatcher;
use crate::uri::web_intent_parser::maybe_rewrite_web_url_to_intent;
use ::ai::index::full_source_code_embedding::manager::CodebaseIndexManager;
use ::ai::index::full_source_code_embedding::SyncTask;
use ::ai::index::DEFAULT_SYNC_REQUESTS_PER_MIN;
use ::ai::project_context::model::ProjectContextModel;
pub use ai::agent::{todos::AIAgentTodoList, AIAgentActionResultType, FileEdit, TodoOperation};
use ai::agent_conversations_model::AgentConversationsModel;
use ai::agent_management::AgentNotificationsModel;
use ai::ambient_agents::scheduled::ScheduledAgentManager;
use ai::blocklist::{BlocklistAIHistoryModel, BlocklistAIPermissions};
use ai::execution_profiles::editor::ExecutionProfileEditorManager;
use ai::execution_profiles::profiles::AIExecutionProfilesModel;
use ai::persisted_workspace::PersistedWorkspace;
use auth::auth_state::AuthStateProvider;
use auth::{auth_manager::AuthManager, auth_state::AuthState};
use code::editor_management::CodeManager;
use code::opened_files::OpenedFilesModel;
use code_review::GlobalCodeReviewModel;
use quit_warning::UnsavedStateSummary;
use server::network_log_pane_manager::NetworkLogPaneManager;
use server::network_logging::NetworkLogModel;
use server::telemetry::context_provider::AppTelemetryContextProvider;
#[cfg(feature = "local_fs")]
use settings::import::model::ImportedConfigModel;
use mnemos_cli::GlobalOptions;
use mnemos_cli::{agent::AgentCommand, CliCommand};

#[cfg(feature = "local_fs")]
use repo_metadata::{
    repositories::DetectedRepositories, watcher::DirectoryWatcher, RepoMetadataModel,
};
#[cfg(feature = "local_fs")]
use watcher::HomeDirectoryWatcher;

use settings_view::pane_manager::SettingsPaneManager;
use terminal::general_settings::GeneralSettings;
use terminal::keys_settings::KeysSettings;
#[cfg(all(not(target_family = "wasm"), feature = "local_tty"))]
use terminal::local_shell::LocalShellState;
pub use util::bindings::cmd_or_ctrl_shift;
pub mod workflows;
pub mod workspace;

#[cfg(feature = "integration_tests")]
pub use persistence::testing as sqlite_testing;

use ::settings::{Setting, ToggleableSetting};
pub use mnemos_core::errors::{report_error, report_if_error};

use mnemos_core::user_preferences::GetUserPreferences as _;
use mnemos_ui::modals::{AlertDialogWithCallbacks, AppModalCallback};
use mnemos_ui::platform::app::ApproveTerminateResult;
use window_settings::WindowSettings;
use workflows::manager::WorkflowManager;

use crate::ai::ambient_agents::github_auth_notifier::GitHubAuthNotifier;
use crate::ai::document::ai_document_model::AIDocumentModel;
use crate::ai::facts::manager::AIFactManager;
use crate::ai::llms::LLMPreferences;
use crate::ai::mcp::MCPGalleryManager;
use crate::ai::mcp::TemplatableMCPServerManager;
use crate::ai::outline::RepoOutlines;
use crate::ai::restored_conversations::RestoredAgentConversations;
use crate::ai::skills::SkillManager;
use crate::ai::AIRequestUsageModel;
use crate::autoupdate::{AutoupdateState, RelaunchModel};
use crate::changelog_model::ChangelogModel;
use crate::cloud_object::model::actions::ObjectActions;
use crate::cloud_object::model::view::CloudViewModel;
use crate::code::global_buffer_model::GlobalBufferModel;
#[cfg(feature = "local_fs")]
use crate::code::language_server_shutdown_manager::LanguageServerShutdownManager;
use crate::context_chips::prompt::Prompt;
use crate::default_terminal::DefaultTerminal;
use crate::drive::export::ExportManager;
use crate::env_vars::manager::EnvVarCollectionManager;
use crate::gpu_state::GPUState;
use crate::network::NetworkStatus;
use crate::notebooks::editor::keys::NotebookKeybindings;
use crate::notebooks::manager::NotebookManager;
use crate::notebooks::CloudNotebook;
use crate::palette::PaletteMode;
use crate::persistence::PersistenceWriter;
use crate::projects::ProjectManagementModel;
use crate::server::cloud_objects::{listener::Listener, update_manager::UpdateManager};
use crate::server::sync_queue::{QueueItem, SyncQueue};
use crate::session_management::{RunningSessionSummary, SessionNavigationData};
use crate::settings::cloud_preferences_syncer::initialize_cloud_preferences_syncer;
use crate::settings::manager::SettingsManager;
use crate::settings::{AccessibilitySettings, ScrollSettings, SelectionSettings};
use crate::settings_view::keybindings::KeybindingChangedNotifier;
use crate::settings_view::DisplayCount;
use crate::suggestions::ignored_suggestions_model::IgnoredSuggestionsModel;
use crate::system::SystemStats;
use crate::terminal::cli_agent_sessions::CLIAgentSessionsModel;
use crate::terminal::keys::TerminalKeybindings;
use crate::terminal::resizable_data::ResizableData;
use crate::terminal::view::inline_banner::ByoLlmAuthBannerSessionState;
use crate::terminal::{AudibleBell, History};
use crate::undo_close::UndoCloseStack;
use crate::user_config::MnemosConfig;
use crate::vim_registers::VimRegisters;
use crate::mnemos_managed_paths_watcher::{ensure_mnemos_watch_roots_exist, MnemosManagedPathsWatcher};
use crate::workflows::aliases::WorkflowAliases;
use crate::workflows::local_workflows::LocalWorkflows;
use crate::workspace::{ActiveSession, OneTimeModalModel, ToastStack};
use crate::workspaces::team_tester::TeamTesterStatus;
use crate::workspaces::user_profiles::UserProfiles;
#[cfg(feature = "local_tty")]
use anyhow::Context;
use anyhow::{anyhow, Result};
use appearance::{Appearance, AppearanceManager};
use channel::ChannelState;
use interval_timer::IntervalTimer;
use itertools::Itertools;
use rust_embed::RustEmbed;
use server::server_api::ServerApiProvider;
use settings::{ExtraMetaKeys, PrivacySettings};
use std::borrow::Cow;
use std::collections::HashSet;
use std::ops::Deref;
use std::sync::Arc;
use terminal::input;
use terminal::session_settings::SessionSettings;
use url::Url;
use mnemos_core::execution_mode::{AppExecutionMode, ExecutionMode};
use mnemos_managed_secrets::ManagedSecretManager;
use workspace::sync_inputs::SyncedInputState;

use mnemos_ui::{integration::TestDriver, App, AssetProvider, Event};

use self::features::FeatureFlag;
use crate::app_state::AppState;
use crate::cloud_object::model::actions::ObjectAction;
use crate::cloud_object::model::persistence::CloudModel;
use crate::drive::CloudObjectTypeAndId;
pub use crate::global_resource_handles::{GlobalResourceHandles, GlobalResourceHandlesProvider};
use crate::notification::NotificationContext;
use crate::root_view::{
    quake_mode_window_id, quake_mode_window_is_open, OpenFromRestoredArg, OpenPath,
};
pub use crate::server::telemetry::{
    AgentModeEntrypoint, AgentModeEntrypointSelectionType, TelemetryEvent,
};
use crate::server::telemetry::{AppStartupInfo, CloseTarget, PaletteSource, TelemetryCollector};
use crate::terminal::CustomSecretRegexUpdater;
use crate::util::bindings::is_binding_cross_platform;
use crate::workspace::{PaneViewLocator, Workspace, WorkspaceAction};
use crate::workspaces::update_manager::TeamUpdateManager;
use crate::workspaces::user_workspaces::UserWorkspaces;
use mnemos_logging::LogDestination;

// Re-export the send_telemetry_from_ctx macro at the crate root level
pub use mnemos_core::send_telemetry_from_app_ctx;
pub use mnemos_core::send_telemetry_from_ctx;

// Re-export the safe logging macros at the crate root level for backwards compatibility
pub use mnemos_core::{safe_debug, safe_error, safe_info, safe_warn};

#[cfg(feature = "local_fs")]
use mnemos_files::FileModel;
use mnemos_ui::platform::TerminationMode;
use mnemos_ui::windowing::state::ApplicationStage;
use mnemos_ui::{AppContext, SingletonEntity, WindowId};

#[derive(Clone, Copy, RustEmbed)]
#[folder = "assets"]
#[include = "bundled/**"] // Should be kept in sync with BUNDLED_ASSETS_DIR.
#[include = "async/**"] // Should be kept in sync with ASYNC_ASSETS_DIR.
#[cfg_attr(target_family = "wasm", exclude = "async/**")]
// Excludes take precedence.
// Standalone CLI builds (the `oz` tarball) are headless and never render the
// onboarding/theme imagery in `async/`, so we exclude those bytes from the
// embedded asset set to keep the CLI binary small — mirroring the carve-out
// already applied for the WASM target above.
#[cfg_attr(feature = "standalone", exclude = "async/**")]
// Mnemos IDE: the image themes' backgrounds (`mnemos_themes::STONE_IMAGE` and friends) are under
// `bundled/` so every GUI build embeds them and never fetches them. The headless standalone CLI
// never draws a theme, so it leaves them out as it does `async/`.
#[cfg_attr(feature = "standalone", exclude = "bundled/jpg/themes/**")]
pub struct Assets;

pub static ASSETS: Assets = Assets;

fn determine_agent_source(
    launch_mode: &LaunchMode,
) -> Option<crate::ai::ambient_agents::AgentSource> {
    match launch_mode {
        LaunchMode::CommandLine { .. } => {
            if std::env::var("GITHUB_ACTIONS").ok().as_deref() == Some("true") {
                Some(crate::ai::ambient_agents::AgentSource::GitHubAction)
            } else {
                Some(crate::ai::ambient_agents::AgentSource::Cli)
            }
        }
        LaunchMode::App { .. } | LaunchMode::Test { .. } => {
            Some(crate::ai::ambient_agents::AgentSource::CloudMode)
        }
    }
}

/// Launch mode for how to start up Mnemos.
#[allow(clippy::large_enum_variant)]
pub enum LaunchMode {
    /// Run the regular GUI application.
    App {
        args: mnemos_cli::AppArgs,
        /// API key for server authentication, if provided via `--api-key` or `MNEMOS_API_KEY`.
        /// Only used on dogfood channels.
        api_key: Option<String>,
    },

    /// Run the Mnemos command-line SDK.
    CommandLine {
        command: mnemos_cli::CliCommand,
        global_options: GlobalOptions,
        debug: bool,
        /// Whether this CLI invocation is running in a sandboxed environment.
        is_sandboxed: bool,
        /// Override for computer use permission from CLI flags. If None, uses default behavior.
        computer_use_override: Option<bool>,
    },
    /// Run a test - this may be an integration test or an eval.
    Test {
        driver: Box<Option<TestDriver>>,
        is_integration_test: bool,
    },
}

impl LaunchMode {
    fn args(&self) -> Cow<'_, mnemos_cli::AppArgs> {
        match self {
            LaunchMode::App { args, .. } => Cow::Borrowed(args),
            _ => Cow::Owned(mnemos_cli::AppArgs::default()),
        }
    }

    /// Returns `true` if this process is running an integration test.
    fn is_integration_test(&self) -> bool {
        match self {
            LaunchMode::Test {
                is_integration_test,
                ..
            } => *is_integration_test,
            _ => false,
        }
    }

    fn take_test_driver(&mut self) -> Option<TestDriver> {
        match self {
            LaunchMode::Test { driver, .. } => driver.take(),
            _ => None,
        }
    }

    /// Add an URL to open. Only supported for [`LaunchMode::App`]
    #[allow(dead_code)]
    fn add_url(&mut self, url: Url) {
        if let LaunchMode::App { ref mut args, .. } = self {
            args.urls.push(url);
        }
    }

    fn execution_mode(&self) -> ExecutionMode {
        match self {
            LaunchMode::App { .. } => ExecutionMode::App,
            LaunchMode::CommandLine { .. } => ExecutionMode::Sdk,
            LaunchMode::Test { .. } => ExecutionMode::App,
        }
    }

    fn is_sandboxed(&self) -> bool {
        match self {
            LaunchMode::CommandLine { is_sandboxed, .. } => *is_sandboxed,
            _ => false,
        }
    }

    /// Returns `true` if Mnemos should run headlessly, without a visible UI.
    fn is_headless(&self) -> bool {
        match self {
            LaunchMode::CommandLine { command, .. } => match command {
                CliCommand::Agent(AgentCommand::Run(args)) => !args.gui,
                _ => true,
            },
            _ => false,
        }
    }

    /// Returns `true` if running in app mode or via `agent run` to permit codebase indexing.
    fn supports_indexing(&self) -> bool {
        match self {
            LaunchMode::CommandLine { command, .. } => {
                matches!(command, CliCommand::Agent(AgentCommand::Run { .. }))
            }
            _ => true,
        }
    }

    /// Whether or not to start a crash recovery process (on platforms that support it).
    #[cfg(enable_crash_recovery)]
    pub(crate) fn crash_recovery_enabled(&self) -> bool {
        match self {
            LaunchMode::App { .. } => true,
            LaunchMode::CommandLine { .. } => false,
            LaunchMode::Test { .. } => false,
        }
    }

    #[cfg(test)]
    pub(crate) fn new_for_unit_test() -> Self {
        LaunchMode::Test {
            driver: Box::new(None),
            is_integration_test: false,
        }
    }
}

impl AssetProvider for Assets {
    fn get(&self, path: &str) -> Result<Cow<'_, [u8]>> {
        <Assets as RustEmbed>::get(path)
            .map(|f| f.data)
            .ok_or_else(|| anyhow!("no asset exists at path {}", path))
    }
}

/// If the given event is a key down event containing alt modifiers, and those
/// alt modifiers should be treated as meta keys, then remove the alts and
/// prefix the keys with an escape. See WAR-472.
fn apply_extra_meta_keys(event: &mut Event, extra_metas: ExtraMetaKeys) {
    if let Event::KeyDown {
        keystroke, details, ..
    } = event
    {
        let left_as_meta = extra_metas.left_alt && details.left_alt;
        let right_as_meta = extra_metas.right_alt && details.right_alt;
        if left_as_meta || right_as_meta {
            let side = match (left_as_meta, right_as_meta) {
                (true, true) => "left+right alt",
                (true, false) => "left alt",
                (false, true) => "right alt",
                (false, false) => unreachable!(),
            };
            log::info!("Treating {side} as meta");
            keystroke.alt = false;
            keystroke.meta = true;
        }
    }
}

fn apply_scroll_multiplier(event: &mut Event, app: &AppContext) {
    if let Event::ScrollWheel { delta, precise, .. } = event {
        if !*precise {
            let scroll_multiplier = *ScrollSettings::as_ref(app).mouse_scroll_multiplier.value();
            *delta *= scroll_multiplier;
        }
    }
}

/// Runs the app. If a subcommand was requested, it'll be run instead of the main application.
/// Environment markers Claude Code sets for its own subprocesses. Inherited by the app, they
/// propagate to every PTY and make each `claude` inside think it is a nested session.
const CLAUDE_CHILD_SESSION_MARKERS: &[&str] = &["CLAUDE_CODE_CHILD_SESSION", "CLAUDECODE"];

fn strip_inherited_claude_session_markers() {
    for name in CLAUDE_CHILD_SESSION_MARKERS {
        if std::env::var_os(name).is_some() {
            log::info!("Stripping inherited {name} from the app environment");
            // Single-threaded at this point — nothing has been spawned yet — so mutating the
            // process environment is sound. The `unsafe` is required on edition 2024 and merely
            // redundant on older editions.
            #[allow(unused_unsafe)]
            unsafe {
                std::env::remove_var(name);
            }
        }
    }
}

pub fn run() -> Result<()> {
    // Mnemos is never a Claude Code child session, but it inherits whatever environment it was
    // launched from — and `open -a Mnemos` run from inside a Claude Code tool call, an install
    // script, or an agent hands the app `CLAUDE_CODE_CHILD_SESSION=1`. Every terminal and every
    // `claude` started inside that instance then inherits the marker, believes it is a nested
    // session, and silently stops persisting its transcript ("Transcript saving is off"), so
    // nothing started in that app can be resumed. Strip it before anything is spawned. This
    // runs for the `terminal-server` subcommand too, which is what the PTYs descend from.
    strip_inherited_claude_session_markers();

    // Perform any necessary platform-specific initialization.
    platform::init();

    // Ensure feature flags are initialized before parsing command-line arguments.
    init_feature_flags();

    // Parse command-line arguments.
    let args = mnemos_cli::Args::from_env();

    // Server URL overrides are only honored on internal dev channels. Release channels silently
    // ignore `--server-root-url` / `--ws-server-url` / `--session-sharing-server-url` (and their
    // `MNEMOS_*` env-var equivalents) so shipped builds can't be redirected away from their
    // baked-in server URLs. See `Channel::allows_server_url_overrides`.
    //
    // Mnemos IDE: a source build (no `release_bundle`) honours them on the stable channel too, so
    // a dev build can talk to a local sync server (`MNEMOS_SERVER_ROOT_URL=http://localhost:8080`)
    // instead of the production one baked in by app/build.rs.
    if ChannelState::channel().allows_server_url_overrides() || !cfg!(feature = "release_bundle") {
        if let Some(url) = args.server_root_url() {
            if let Err(e) = ChannelState::override_server_root_url(url.to_owned()) {
                eprintln!("Error: Invalid server root URL: {e:#}");
            }
        }

        if let Some(url) = args.ws_server_url() {
            if let Err(e) = ChannelState::override_ws_server_url(url.to_owned()) {
                eprintln!("Error: Invalid websocket server URL: {e:#}");
            }
        }

        if let Some(url) = args.session_sharing_server_url() {
            if let Err(e) = ChannelState::override_session_sharing_server_url(url.to_owned()) {
                eprintln!("Error: Invalid session sharing server URL: {e:#}");
            }
        }
    }

    if let Some(command) = args.command() {
        #[cfg(windows)]
        if command.prints_to_stdout() {
            // We attach a console to ensure that all standard output gets printed correctly.
            mnemos_util::windows::attach_to_parent_console();
        }
        match command {
            #[cfg(all(feature = "local_tty", unix))]
            mnemos_cli::Command::Worker(mnemos_cli::WorkerCommand::TerminalServer(args)) => {
                // If we were asked to run as a terminal server (as opposed to the main
                // GUI application), do so immediately.  Ideally, the terminal server would
                // be a separate binary, but it's much easier to distribute a single binary,
                // so starting the terminal server event loop immediately is the closest
                // approximation we can get to running a separate binary.
                crate::terminal::local_tty::server::run_terminal_server(args);
                return Ok(());
            }
            #[cfg(feature = "local_tty")]
            mnemos_cli::Command::Worker(mnemos_cli::WorkerCommand::MinidumpServer { socket_name }) => {
                cfg_if::cfg_if! {
                    if #[cfg(all(linux_or_windows, feature = "crash_reporting"))] {
                        return crate::crash_reporting::run_minidump_server(socket_name);
                    } else {
                        let _ = socket_name;
                        panic!("The minidump server is not supported on this platform");
                    }
                }
            }
            #[cfg(not(target_family = "wasm"))]
            mnemos_cli::Command::Worker(mnemos_cli::WorkerCommand::RemoteServerProxy(args)) => {
                return crate::remote_server::run_proxy(args.identity_key.clone());
            }
            #[cfg(not(target_family = "wasm"))]
            mnemos_cli::Command::Worker(mnemos_cli::WorkerCommand::RemoteServerDaemon(args)) => {
                return crate::remote_server::run_daemon(args.identity_key.clone());
            }
            #[cfg(not(target_family = "wasm"))]
            mnemos_cli::Command::Worker(mnemos_cli::WorkerCommand::RipgrepSearch {
                parent,
                ignore_case,
                multiline,
                pattern,
                paths,
            }) => {
                mnemos_ripgrep::search::run_search_subprocess(
                    std::slice::from_ref(pattern),
                    paths.clone(),
                    *ignore_case,
                    *multiline,
                    parent.pid,
                )
                .map_err(|err| anyhow!(err.to_string()))?;
                return Ok(());
            }
            #[cfg(not(any(feature = "local_tty", not(target_family = "wasm"))))]
            mnemos_cli::Command::Worker(worker) => {
                // Need this case to handle platforms where there are no enum variants in
                // mnemos_cli::WorkerCommand, as we still need to check Command::Worker.

                // On wasm, specifically, we should fail spectacularly if we get here.
                #[cfg(target_family = "wasm")]
                panic!("Worker process not supported on WASM: {worker:?}")
            }
            mnemos_cli::Command::Completions { shell } => {
                return mnemos_cli::completions::generate_to_stdout(*shell);
            }
            mnemos_cli::Command::CommandLine(cmd) => {
                let (is_sandboxed, computer_use_override) = match cmd.as_ref() {
                    mnemos_cli::CliCommand::Agent(mnemos_cli::agent::AgentCommand::Run(run_args)) => (
                        run_args.sandboxed,
                        run_args.computer_use.computer_use_override(),
                    ),
                    _ => (false, None),
                };

                return run_internal(LaunchMode::CommandLine {
                    command: cmd.as_ref().clone(),
                    global_options: GlobalOptions {
                        output_format: args.output_format(),
                        api_key: args.api_key().cloned(),
                    },
                    debug: args.debug(),
                    is_sandboxed,
                    computer_use_override,
                });
            }
            mnemos_cli::Command::DumpDebugInfo => {
                return debug_dump::run();
            }
            #[cfg(not(target_family = "wasm"))]
            mnemos_cli::Command::PrintTelemetryEvents => {
                return TelemetryEvent::print_telemetry_events_json();
            }
        }
    }

    // If running as a standalone CLI binary or invoked as the CLI command, print help
    // instead of launching the GUI app. The stable CLI command (`mnemos`) has the same name
    // as the app binary, so the bundled CLI wrappers set MNEMOS_CLI_MODE instead; the name
    // check only covers a link whose name differs from the binary it points at. `oz*` covers
    // links created by older Mnemos versions, which named the command `oz`.
    let is_cli_binary = cfg!(feature = "standalone")
        || mnemos_cli::binary_name()
            .is_some_and(|name| name.starts_with("oz") || is_cli_link_name(&name))
        || std::env::var_os("MNEMOS_CLI_MODE").is_some();
    if is_cli_binary {
        mnemos_cli::Args::clap_command().print_help()?;
        return Ok(());
    }

    let api_key = args.api_key().cloned();
    run_internal(LaunchMode::App {
        args: args.into_app_args(),
        api_key,
    })
}

/// Whether the binary was invoked through a link named after this channel's CLI command
/// while the binary itself has another name.
fn is_cli_link_name(invoked_as: &str) -> bool {
    let cli_name = ChannelState::channel().cli_command_name();
    if invoked_as != cli_name {
        return false;
    }
    let exe_name = std::env::current_exe().ok().and_then(|exe| {
        exe.file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
    });
    exe_name.as_deref() != Some(cli_name)
}

/// Runs an integration test using the provided test driver.
pub fn run_integration_test(driver: TestDriver) -> Result<()> {
    // Mnemos IDE: before `AuthManager::new` reads the admin login. The harness only
    // redirects HOME, which the OS keyring ignores, so without this a test
    // run signs in with the developer's real login, starts sync against the
    // real admin server and rewrites that login on its first token refresh.
    // Not `cfg(test)`: this binary is built without it, so the unit-test
    // switch in `use_in_memory_keyring_for_unit_tests` never runs here.
    mnemos_ide_sync::auth::use_in_memory_keyring();
    let is_integration_test = std::env::var("MNEMOS_INTEGRATION").is_ok();
    let launch = LaunchMode::Test {
        driver: Box::new(Some(driver)),
        is_integration_test,
    };
    run_internal(launch)
}

/// Runs the app.
fn run_internal(mut launch_mode: LaunchMode) -> Result<()> {
    #[cfg(windows)]
    dynamic_libraries::configure_library_loading();

    profiling::init();

    // The `run` function already initializes feature flags, but ensure they're initialized here
    // for other entrypoints.
    init_feature_flags();

    let mut timer = IntervalTimer::new();

    #[cfg(feature = "crash_reporting")]
    {
        // Ensure that the main/root Sentry hub is initialized on the main
        // thread.  PtySpawner creates a background thread to receive logs from
        // the terminal server process, and we don't want it to be the host of
        // the primary sentry::Hub.
        sentry::Hub::main();
    }

    tracing::init()?;

    let is_cli = matches!(launch_mode, LaunchMode::CommandLine { .. });

    // Log to a file for CLI commands to not obscure the command output.
    let log_destination = match &launch_mode {
        LaunchMode::CommandLine { debug, .. } => {
            if *debug {
                Some(LogDestination::Stderr)
            } else {
                Some(LogDestination::File)
            }
        }
        _ => None,
    };

    cfg_if::cfg_if! {
        if #[cfg(enable_crash_recovery)] {
            if crash_recovery::is_crash_recovery_process(launch_mode.args().as_ref()) {
                mnemos_logging::init_for_crash_recovery_process()?;
            } else {
                mnemos_logging::init(mnemos_logging::LogConfig { is_cli, log_destination })?;
            }
        } else {
            mnemos_logging::init(mnemos_logging::LogConfig { is_cli, log_destination })?;
        }
    }

    timer.mark_interval_end("LOG_FILE_SETUP_COMPLETE");

    // Mnemos IDE integration probe: load the project store and log its size.
    // Proves the mnemos-ide-projects ↔ mnemos app/ wire is live before we start
    // binding Mnemos IDE projects to the session sidebar.
    match mnemos_ide_projects::ProjectStore::load_blocking() {
        Ok(store) => log::info!(
            target: "mnemos_ide",
            "mnemos-ide projects loaded: count={}, active={:?}",
            store.projects.len(),
            store.tab_state.as_ref().and_then(|s| s.active_project_id.clone()),
        ),
        Err(e) => log::warn!(target: "mnemos_ide", "mnemos-ide projects load failed: {e}"),
    }

    // Adjust resource limits early, before doing other work, to ensure that
    // any children we spawn (like the terminal server) inherit our adjusted
    // rlimits.
    resource_limits::adjust_resource_limits();

    // For wasm builds we have this special case to parse out the intent
    // from the url that is used to visite the app on web.
    #[cfg(target_family = "wasm")]
    {
        use uri::web_intent_parser;
        if let Some(intent) = web_intent_parser::parse_web_intent_from_current_url() {
            launch_mode.add_url(intent);
        }
        web_intent_parser::set_context_flags_from_current_url();
    }

    // Collect errors that occur in run_internal() before the Sentry client is initialized,
    // so they can be replayed to Sentry once it's ready.
    #[cfg_attr(
        not(all(feature = "release_bundle", any(windows, target_os = "linux"))),
        expect(unused_mut)
    )]
    let mut pre_sentry_errors: Vec<anyhow::Error> = Vec::new();

    #[cfg(all(feature = "release_bundle", target_os = "linux"))]
    if let LaunchMode::App { .. } = launch_mode {
        match app_services::linux::pass_startup_args_to_existing_instance(
            launch_mode.args().as_ref(),
        ) {
            // If we were able to contact an existing application instance, quit -
            // we only want to run a single instance of Mnemos at a time.
            Ok(_) => std::process::exit(0),
            // If Mnemos isn't already running, we're good to go.
            Err(app_services::linux::StartupArgsForwardingError::NoExistingInstance) => {}
            // If we just finished an auto-update, we should continue running.
            Err(app_services::linux::StartupArgsForwardingError::IgnoredAfterAutoUpdate) => {}
            // If we were unable to perform the forwarding for an unknown reason,
            // it's better to run a second instance than potentially end up in a
            // state where Mnemos refuses to run even a first instance.
            Err(err) => {
                let err = anyhow::Error::from(err).context("Failed to forward startup args");
                log::error!("{err:#}");
                pre_sentry_errors.push(err);
            }
        }
    }

    #[cfg(all(feature = "release_bundle", windows))]
    if let LaunchMode::App { .. } = launch_mode {
        match app_services::windows::pass_startup_args_to_existing_instance(
            launch_mode.args().as_ref(),
        ) {
            // If we were able to contact an existing application instance, quit -
            // we only want to run a single instance of Mnemos at a time.
            Ok(_) => std::process::exit(0),
            // If Mnemos isn't already running, we're good to go.
            Err(app_services::windows::StartupArgsForwardingError::NoExistingInstance) => {}
            // If we just finished an auto-update, we should continue running.
            Err(app_services::windows::StartupArgsForwardingError::IgnoredAfterAutoUpdate) => {}
            // If we were unable to perform the forwarding for an unknown reason,
            // it's better to run a second instance than potentially end up in a
            // state where Mnemos refuses to run even a first instance.
            Err(err) => {
                let err = anyhow::Error::from(err).context("Failed to forward startup args");
                log::error!("{err:#}");
                pre_sentry_errors.push(err);
            }
        }
    }

    // Sets up a Job Object that we associate with the Mnemos process to handle
    // shared fate with its child processes. This should be called before we
    // start spawning any child processes.
    #[cfg(windows)]
    command::windows::init();

    // Configure rustls to use its default crypto provider.  This MUST be called
    // before making any network requests that use TLS, otherwise rustls will
    // panic.
    #[cfg(not(target_family = "wasm"))]
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("must be able to initialize crypto provider for TLS support");

    let private_preferences = settings::init_private_user_preferences();
    let (public_preferences, startup_toml_parse_error) = settings::init_public_user_preferences();

    // When the SettingsFile feature flag is enabled, public settings live in
    // the TOML-backed store. When disabled, they live in the platform-native
    // store (same backend as private). Use the correct one for pre-app reads.
    #[cfg_attr(not(any(enable_crash_recovery, target_os = "linux")), expect(unused))]
    let prefs_for_public_settings: &dyn mnemos_ui_extras::user_preferences::UserPreferences =
        if FeatureFlag::SettingsFile.is_enabled() {
            public_preferences.as_ref()
        } else {
            private_preferences.deref()
        };

    #[cfg(enable_crash_recovery)]
    let crash_recovery =
        crash_recovery::CrashRecovery::new(&launch_mode, prefs_for_public_settings);

    // Set up the pty spawner before doing any meaningful work. We want to
    // ensure that the process is in the cleanest possible state (minimal opened
    // files, modified signal handlers, etc.) to avoid unexpected effects on
    // spawned ptys.
    #[cfg(feature = "local_tty")]
    let pty_spawner =
        terminal::local_tty::spawner::PtySpawner::new().context("Failed to create pty spawner")?;

    let mut app_builder = if launch_mode.is_headless() {
        mnemos_ui::platform::AppBuilder::new_headless(
            app_callbacks(launch_mode.is_integration_test()),
            Box::new(ASSETS),
            launch_mode.take_test_driver(),
        )
    } else {
        mnemos_ui::platform::AppBuilder::new(
            app_callbacks(launch_mode.is_integration_test()),
            Box::new(ASSETS),
            launch_mode.take_test_driver(),
        )
    };

    #[cfg(target_os = "macos")]
    {
        use mnemos_ui::platform::mac::AppExt;

        let activate_on_launch = !launch_mode.is_integration_test()
            || std::env::var("MNEMOSUI_USE_REAL_DISPLAY_IN_INTEGRATION_TESTS").is_ok();
        app_builder.set_activate_on_launch(activate_on_launch);

        let dev_icon = ASSETS.get("bundled/png/local.png")?;
        app_builder.set_dev_icon(dev_icon);

        app_builder.set_menu_bar_builder(app_menus::menu_bar);
        app_builder.set_dock_menu_builder(|_| app_menus::dock_menu());
    }

    #[cfg(target_os = "linux")]
    {
        use crate::settings::ForceX11;
        use mnemos_ui::platform::linux::{self, AppBuilderExt};

        app_builder.set_window_class(ChannelState::app_id().to_string());

        let force_x11 = ForceX11::read_from_preferences(prefs_for_public_settings)
            .unwrap_or(ForceX11::default_value());
        // Force use of wayland if the user has passed the `MNEMOS_ENABLE_WAYLAND` env var.
        let allow_wayland = linux::is_wayland_env_var_set() || !force_x11;
        app_builder.force_x11(!allow_wayland);
    }

    #[cfg(target_os = "windows")]
    {
        use mnemos_ui::platform::windows::AppBuilderExt;
        app_builder.set_app_user_model_id(ChannelState::app_id().to_string());

        // Only use DXC for DirectX shader compilation if we're not running in a Parallels VM
        // Parallels VMs can have issues with DXC shader compilation
        let is_parallels_vm = crate::util::vm_detection::is_running_in_windows_parallels_vm();
        if !is_parallels_vm {
            log::info!("Using DXC for DirectX shader compilation");
            use mnemos_ui::platform::windows::DXCPath;

            app_builder.use_dxc_for_directx_shader_compilation(DXCPath {
                dxc_path: "dxcompiler.dll".to_string(),
                dxil_path: "dxil.dll".to_string(),
            });
        } else {
            log::info!("Skipping DXC for DirectX shader compilation; running in a Parallels VM");
        }
    }

    // Override any bindings that have a `Custom` trigger to a `Keystroke`-based trigger. In theory,
    // this should be a noop on Mac (since the keystrokes registered via the  Mac menus first
    // intercept the binding), but just to be safe we only enable this in cases where we don't
    // include mac menus.
    #[cfg(not(target_os = "macos"))]
    app_builder.convert_custom_triggers_to_keystroke_triggers(
        crate::util::bindings::custom_tag_to_keystroke,
    );

    #[cfg(target_os = "macos")]
    app_builder.register_default_keystroke_triggers_for_custom_actions(
        crate::util::bindings::custom_tag_to_keystroke,
    );

    app_builder.run(move |ctx| {
        #[cfg(not(target_family = "wasm"))]
        // Rotate the log files in the background.
        ctx.background_executor()
            .spawn(mnemos_logging::rotate_log_files())
            .detach();

        ctx.add_singleton_model(|ctx| {
            AppExecutionMode::new(
                launch_mode.execution_mode(),
                launch_mode.is_sandboxed(),
                ctx,
            )
        });
        #[cfg(feature = "crash_reporting")]
        crate::crash_reporting::set_client_type_tag(launch_mode.execution_mode().client_id());

        // Add the terminal server singleton to the application.
        #[cfg(feature = "local_tty")]
        ctx.add_singleton_model(move |_ctx| pty_spawner);

        // Register user preferences.  This must be done before initializing
        // feature flags or experiments, both of which check user preferences for
        // overrides.
        ctx.add_singleton_model(move |_ctx| ::settings::PublicPreferences::new(public_preferences));
        ctx.add_singleton_model(move |_ctx| private_preferences);
        let startup_toml_parse_error = startup_toml_parse_error;

        // Tell the settings crate whether the TOML settings file is active.
        // This must happen after preferences are registered but before settings
        // are initialized, so the routing logic picks the correct backend.
        ::settings::set_settings_file_enabled(FeatureFlag::SettingsFile.is_enabled());

        #[cfg(enable_crash_recovery)]
        ctx.add_singleton_model(move |_ctx| crash_recovery);

        let app_state = initialize_app(
            &launch_mode,
            timer,
            startup_toml_parse_error,
            ctx,
            pre_sentry_errors,
        );

        // Mnemos IDE: upstream's local palette search A/B layer sent every user to
        // the Tantivy arm; that is now fixed.
        FeatureFlag::UseTantivySearch.set_enabled(true);

        launch(ctx, app_state, launch_mode);
    })
}

pub struct UpdateQuakeModeEventArg {
    active_window_id: Option<WindowId>,
}

fn initialize_app(
    launch_mode: &LaunchMode,
    mut timer: IntervalTimer,
    startup_toml_parse_error: Option<mnemos_ui_extras::user_preferences::Error>,
    ctx: &mut mnemos_ui::AppContext,
    _pre_sentry_errors: impl IntoIterator<Item = anyhow::Error>,
) -> Option<AppState> {
    // WARNING: Errors that happen here before crash_reporting::init will not be collected in
    // Sentry. Only the dependencies of crash_reporting should be initialized here. Avoid adding
    // any other stuff here, as failures will be silent. Push them to pre_sentry_errors instead.
    let data_domain = ChannelState::data_domain();

    // Register an implementation of the secure storage service.
    cfg_if::cfg_if! {
        if #[cfg(feature = "integration_tests")] {
            mnemos_ui_extras::secure_storage::register_noop(&data_domain, ctx);
        } else if #[cfg(target_os = "linux")] {
            mnemos_ui_extras::secure_storage::register_with_fallback(&data_domain, mnemos_core::paths::state_dir(), ctx)
        } else if #[cfg(target_os = "windows")] {
            mnemos_ui_extras::secure_storage::register_with_dir(&data_domain, mnemos_core::paths::state_dir(), ctx)
        } else {
            mnemos_ui_extras::secure_storage::register(&data_domain, ctx);
        }
    }

    // One-time migration: give Preview its own config directory by
    // symlinking contents from the shared ~/.mnemos location. Must run
    // before ensure_mnemos_watch_roots_exist() creates the new directory.
    #[cfg(target_os = "macos")]
    preview_config_migration::migrate_preview_config_dir_if_needed();

    ensure_mnemos_watch_roots_exist();
    ctx.add_singleton_model(MnemosManagedPathsWatcher::new);

    ctx.add_singleton_model(MnemosConfig::new);
    ctx.add_singleton_model(|_ctx| SettingsManager::default());

    let user_defaults_on_startup = settings::init(startup_toml_parse_error, ctx);
    timer.mark_interval_end("READ_USER_DEFAULTS_AND_INITIALIZE_SETTINGS");

    if FeatureFlag::UIZoom.is_enabled() {
        ctx.set_zoom_factor(WindowSettings::as_ref(ctx).zoom_level.as_zoom_factor());
    }

    // Extract API key from command line options, if applicable.
    let api_key = match launch_mode {
        LaunchMode::CommandLine { global_options, .. } => global_options.api_key.clone(),
        LaunchMode::App { api_key, .. } if ChannelState::channel().is_dogfood() => api_key.clone(),
        _ => None,
    };
    let api_key = if FeatureFlag::APIKeyAuthentication.is_enabled() {
        api_key
    } else {
        None
    };

    let auth_state = Arc::new(AuthState::initialize(ctx, api_key));
    timer.mark_interval_end("AUTH_MANAGER_SET_USER");

    let agent_source = determine_agent_source(launch_mode);

    // NetworkLogModel must be registered before ServerApiProvider so that
    // `network_logging::init` (invoked from within `ServerApiProvider::new`)
    // can reach it via `NetworkLogModel::handle(ctx)` when forwarding items
    // captured by the HTTP client hooks.
    ctx.add_singleton_model(|_ctx| NetworkLogModel::default());

    let server_api_provider = ctx
        .add_singleton_model(|ctx| ServerApiProvider::new(auth_state.clone(), agent_source, ctx));
    let server_api = server_api_provider.as_ref(ctx).get();
    let ai_client = server_api_provider.as_ref(ctx).get_ai_client();

    ctx.add_singleton_model(|_ctx| AuthStateProvider::new(auth_state.clone()));

    ctx.add_singleton_model(AppTelemetryContextProvider::new_context_provider);

    ctx.add_singleton_model(|ctx| {
        AuthManager::new(
            server_api.clone(),
            server_api_provider.as_ref(ctx).get_auth_client(),
            ctx,
        )
    });

    ctx.add_singleton_model(|_ctx| GPUState::new());

    PrivacySettings::register_singleton(ctx);

    // If any part of sqlite initialization fails, we just don't do session restoration (i.e.
    // feature degradation).
    let (sqlite_data, writer_handles) = persistence::initialize(ctx);
    timer.mark_interval_end("SQLITE_INITIALIZED");

    let persistence_writer = PersistenceWriter::new(writer_handles);

    let model_event_sender = persistence_writer.sender();

    let tips_handle = ctx.add_model(|_| user_defaults_on_startup.tips_data);
    let user_default_shell_unsupported_banner_model_handle =
        ctx.add_model(|_| user_defaults_on_startup.user_default_shell_unsupported_banner_state);
    // Extract the full-file parse error (if any) before the settings_file_error
    // value is moved below. Only FileParseFailed gates the broken-file guard
    // in `initialize_cloud_preferences_syncer`; InvalidSettings means TOML
    // parsed but individual values were wrong, which doesn't mean local
    // state is unusable.
    let startup_toml_parse_error_for_syncer = user_defaults_on_startup
        .settings_file_error
        .as_ref()
        .and_then(|err| match err {
            settings::SettingsFileError::FileParseFailed(msg) => Some(msg.clone()),
            settings::SettingsFileError::InvalidSettings(_) => None,
        });
    let settings_file_error = user_defaults_on_startup.settings_file_error;
    ctx.add_singleton_model(move |_ctx| {
        GlobalResourceHandlesProvider::new(GlobalResourceHandles {
            model_event_sender,
            tips_completed: tips_handle,
            user_default_shell_unsupported_banner_model_handle,
            settings_file_error,
        })
    });

    let (
        cloud_objects,
        cached_workspaces,
        current_workspace_uid,
        app_state,
        command_history,
        restored_user_profiles,
        time_of_next_force_object_refresh,
        object_actions,
        ai_queries,
        persisted_workspaces,
        workspace_language_servers,
        multi_agent_conversations,
        persisted_projects,
        persisted_project_rules,
        persisted_ignored_suggestions,
        persisted_mcp_server_installations,
        mcp_servers_to_restore,
    ) = sqlite_data
        .map(|sqlite_data| {
            (
                sqlite_data.cloud_objects,
                sqlite_data.workspaces,
                sqlite_data.current_workspace_uid,
                Some(sqlite_data.app_state),
                sqlite_data.command_history,
                sqlite_data.user_profiles,
                sqlite_data.time_of_next_force_object_refresh,
                sqlite_data.object_actions,
                sqlite_data.ai_queries,
                sqlite_data.codebase_indices,
                sqlite_data.workspace_language_servers,
                sqlite_data.multi_agent_conversations,
                sqlite_data.projects,
                sqlite_data.project_rules,
                sqlite_data.ignored_suggestions,
                sqlite_data.mcp_server_installations,
                sqlite_data.mcp_servers_to_restore,
            )
        })
        .unwrap_or_else(|| {
            (
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
            )
        });

    ctx.add_singleton_model(|ctx| AIRequestUsageModel::new(ai_client, ctx));

    ctx.add_singleton_model(|ctx| {
        UserWorkspaces::new(
            server_api_provider.as_ref(ctx).get_team_client(),
            server_api_provider.as_ref(ctx).get_workspace_client(),
            cached_workspaces,
            current_workspace_uid,
            ctx,
        )
    });

    // Initialize ApiKeyManager after UserWorkspaces so it can subscribe to workspace/settings changes
    ctx.add_singleton_model(|ctx| {
        #[cfg_attr(target_family = "wasm", allow(unused_mut))]
        let mut manager = ::ai::api_keys::ApiKeyManager::new(ctx);
        #[cfg(not(target_family = "wasm"))]
        manager.subscribe_to_settings_changes(ctx);
        manager
    });


    cfg_if::cfg_if! {
        if #[cfg(feature = "crash_reporting")] {
            let is_crash_reporting_enabled = crash_reporting::init(ctx);
        } else {
            let is_crash_reporting_enabled = false;
        }
    }
    // Send buffered pre-init errors to Sentry now that the client is ready.
    #[cfg(feature = "crash_reporting")]
    for err in _pre_sentry_errors {
        sentry::integrations::anyhow::capture_anyhow(&err);
    }
    timer.mark_interval_end("INIT_CRASH_REPORTING");

    if let LaunchMode::App { .. } = launch_mode {
        autoupdate::check_and_report_update_errors(ctx);
    }

    ctx.set_fallback_font_source_provider(|url| ::asset_cache::url_source(url));

    ctx.set_default_binding_validator(is_binding_cross_platform);

    if FeatureFlag::Autoupdate.is_enabled() {
        // Attempt to clean up any old executable, whether or not we were
        // explicitly launched as part of the auto-update process.  We may have
        // failed to remove the executable on a previous launch of the app and
        // should try again.
        if let Err(e) = autoupdate::remove_old_executable() {
            log::error!("Failed to remove old executable: {e:?}");
        }
    }

    // Initialize timestamp for session id and last active event
    App::record_last_active_timestamp();

    ctx.add_singleton_model(|_| SettingsPaneManager::new());
    ctx.add_singleton_model(|_| AIFactManager::new());
    ctx.add_singleton_model(|_| ExecutionProfileEditorManager::default());
    ctx.add_singleton_model(|_| NetworkLogPaneManager::default());
    ctx.add_singleton_model(|ctx| {
        // Not using the *Provider types isn't ideal, but it's worth it for the ability to move managed secrets to a separate crate.
        ManagedSecretManager::new(
            server_api_provider.as_ref(ctx).get_managed_secrets_client(),
            auth_state.clone(),
        )
    });

    #[cfg(target_os = "macos")]
    if !launch_mode.is_headless() {
        AppearanceManager::as_ref(ctx).set_app_icon(ctx);
    }

    #[cfg(feature = "local_tty")]
    terminal::available_shells::register(ctx);

    // Add truly global actions that don't depend on the existence of any view here
    ctx.add_global_action("app:toggle_user_ps1", move |_args: &(), ctx| {
        SessionSettings::handle(ctx).update(ctx, |session_settings, ctx| {
            report_if_error!(session_settings.honor_ps1.toggle_and_save_value(ctx));
        });
    });
    ctx.add_global_action("app:toggle_copy_on_select", move |_args: &(), ctx| {
        SelectionSettings::handle(ctx).update(ctx, |selection_settings, ctx| {
            report_if_error!(selection_settings.copy_on_select.toggle_and_save_value(ctx));
        });
    });

    ctx.add_singleton_model(|_ctx| SyncedInputState::new());

    ctx.add_singleton_model(remote_server::manager::RemoteServerManager::new);
    #[cfg(not(target_family = "wasm"))]
    remote_server::wire_auth_token_rotation(ctx);

    log::info!(
        "Starting mnemos with channel state {} and version {:?}",
        ChannelState::debug_str(),
        ChannelState::app_version()
    );

    // Teach our app that sometimes option means meta.
    ctx.set_event_munger(move |event, ctx| {
        let extra_meta_keys = *KeysSettings::as_ref(ctx).extra_meta_keys;
        apply_extra_meta_keys(event, extra_meta_keys);
        apply_scroll_multiplier(event, ctx);
    });

    // Rewrite recognized Mnemos web URLs (sessions, Drive, settings, home) into local
    // intent URLs when possible so they open directly in the desktop app.
    ctx.set_before_open_url(|url_str, _ctx| {
        if let Ok(url) = Url::parse(url_str) {
            if let Some(intent) = maybe_rewrite_web_url_to_intent(&url) {
                return intent.to_string();
            }
        }
        url_str.to_owned()
    });

    ctx.set_a11y_verbosity(*AccessibilitySettings::as_ref(ctx).a11y_verbosity);

    #[cfg(enable_crash_recovery)]
    ctx.on_draw_frame_error(|ctx, window_id| {
        crash_recovery::CrashRecovery::handle(ctx).update(ctx, |crash_recovery, _ctx| {
            crash_recovery.on_draw_frame_error(window_id);
        });
    });

    let user_is_logged_in = auth_state.is_logged_in();

    if user_is_logged_in {
        // Skip refresh_user for CLI mode — the CLI handles auth refresh in
        // ensure_auth_state so it can detect invalid credentials before running
        // a command.
        if !matches!(launch_mode, LaunchMode::CommandLine { .. }) {
            AuthManager::handle(ctx).update(ctx, |auth_manager, ctx| {
                auth_manager.refresh_user(ctx);
            });
        }

        // Set the first frame callback to record the app's startup time.
        // This is only sent for logged-in users so that new users don't skew performance metrics.
        let is_screen_reader_enabled = ctx.is_screen_reader_enabled();
        let from_relaunch = launch_mode.args().finish_update;
        ctx.on_first_frame_drawn(move |ctx| {
            let timing_data = IntervalTimer::handle(ctx).update(ctx, |timer, _| {
                timer.mark_interval_end("FIRST_FRAME_DRAWN");
                timer.compute_stats()
            });
            let event = TelemetryEvent::AppStartup(AppStartupInfo {
                is_session_restoration_on: user_defaults_on_startup.should_restore_session,
                is_screen_reader_enabled,
                from_relaunch,
                is_crash_reporting_enabled,
                timing_data,
            });

            GPUState::handle(ctx).update(ctx, |gpu_state, ctx| {
                gpu_state
                    .set_has_lower_power_gpu(mnemos_ui::rendering::is_low_power_gpu_available(), ctx);
            });

            for window_id in ctx.window_ids().collect_vec() {
                SettingsPaneManager::handle(ctx)
                    .read(ctx, |model, _| model.settings_view(window_id))
                    .update(ctx, |settings, ctx| {
                        settings.refresh_preferred_graphics_backend_dropdown(ctx);
                    })
            }

            send_telemetry_from_app_ctx!(event, ctx);
        });

        #[cfg(enable_crash_recovery)]
        ctx.on_frame_drawn(|ctx, window_id| {
            crash_recovery::CrashRecovery::handle(ctx).update(ctx, |crash_recovery, ctx| {
                crash_recovery.on_frame_drawn(window_id, ctx);
            });
        })
    } else {
        // If the app was opened while logged out, record an event for measuring new users.
        // This is sent immediately in case they quit the app on the signup screen.
        send_telemetry_sync_from_app_ctx!(TelemetryEvent::LoggedOutStartup, ctx);
        // Mnemos IDE: the download-source probe (`brew list` on macOS) only feeds a
        // telemetry event, so skip it in builds that have no telemetry destination.
        if ChannelState::is_telemetry_available() {
            download_method::determine_and_report(
                auth_state.clone(),
                ctx.background_executor().clone(),
            );
        }
    }

    #[cfg(not(target_family = "wasm"))]
    {
        ctx.add_singleton_model(DirectoryWatcher::new);
        ctx.add_singleton_model(|_| DetectedRepositories::default());
        if let Some(home_dir) = dirs::home_dir() {
            ctx.add_singleton_model(|ctx| HomeDirectoryWatcher::new(home_dir, ctx));
        } else {
            log::info!("Home directory not found; skipping HomeDirectoryWatcher registration");
        }
    }

    #[cfg(feature = "local_fs")]
    {
        let imported_config_model = ctx.add_singleton_model(ImportedConfigModel::new);

        if FeatureFlag::SettingsImport.is_enabled()
            && ChannelState::channel() != mnemos_core::channel::Channel::Integration
        {
            imported_config_model.update(ctx, |model, ctx| {
                model.search_for_settings_to_import(ctx);
            });
        }

        ctx.add_singleton_model(|ctx| {
            let model = RepoMetadataModel::new(ctx);

            // Subscribe to RemoteServerManager push events so that remote repo
            // metadata snapshots and incremental updates populate the remote
            // sub-model and trigger RepoMetadataEvent emissions.
            {
                use remote_server::manager::{RemoteServerManager, RemoteServerManagerEvent};
                let mgr = RemoteServerManager::handle(ctx);
                ctx.subscribe_to_model(&mgr, |me, event, ctx| match event {
                    RemoteServerManagerEvent::RepoMetadataSnapshot { host_id, update } => {
                        me.insert_remote_snapshot(host_id.clone(), update, ctx);
                    }
                    RemoteServerManagerEvent::RepoMetadataUpdated { host_id, update }
                    | RemoteServerManagerEvent::RepoMetadataDirectoryLoaded { host_id, update } => {
                        me.apply_remote_incremental_update(host_id, update, ctx);
                    }
                    RemoteServerManagerEvent::HostDisconnected { host_id } => {
                        me.remove_remote_repositories_for_host(host_id, ctx);
                    }
                    _ => {}
                });
            }

            model
        });
    }

    {
        use code_review::git_status_update::GitStatusUpdateModel;
        ctx.add_singleton_model(|_| GitStatusUpdateModel::new());
    }

    ctx.add_singleton_model(|ctx| {
        ProjectManagementModel::new(persisted_projects, persistence_writer.sender(), ctx)
    });

    ctx.add_singleton_model(move |_| History::new(command_history));

    ctx.add_singleton_model(CustomSecretRegexUpdater::new);

    // Register the `TelemetryCollection` singleton model.
    let server_api_clone = server_api.clone();
    ctx.add_singleton_model(|ctx| {
        let telemetry_collector = TelemetryCollector::new(server_api_clone);
        telemetry_collector.initialize_telemetry_collection(ctx);
        telemetry_collector
    });
    timer.mark_interval_end("INITIALIZE_TELEMETRY_COLLECTION");

    // Register initial keybindings prior to creating menus
    ai::init(ctx);
    app_services::init(ctx);
    // // TODO: Temporarily disabling keybindings for WASM builds. Will be implemented in future WASM support.
    #[cfg(not(target_family = "wasm"))]
    code::editor::find::view::init(ctx);
    workspace::init(ctx);
    pane_group::init(ctx);
    terminal::init(ctx);
    input::init(ctx);
    editor::init(ctx);
    // onboarding::init(ctx);  // Mnemos IDE B1: wizard disabled — boot straight into workspace
    menu::init(ctx);
    tips::tip_view::init(ctx);
    launch_configs::init(ctx);
    workflows::init(ctx);
    themes::theme_chooser::init(ctx);
    themes::theme_creator_modal::init(ctx);
    themes::theme_deletion_modal::init(ctx);
    root_view::init(ctx);
    voltron::init(ctx);
    auth::init(ctx);
    crate::view_components::find::init(ctx);
    prompt::editor_modal::init(ctx);
    ai::blocklist::agent_view::editor::init(ctx);
    undo_close::init(ctx);
    tab_configs::new_worktree_modal::init(ctx);
    tab_configs::params_modal::init(ctx);
    ai::blocklist::init(ctx);
    ai::blocklist::block::status_bar::init(ctx);
    drive::index::init(ctx);
    drive::sharing::dialog::init(ctx);
    ai_assistant::panel::init(ctx);
    settings_view::update_environment_form::init(ctx);
    env_vars::env_var_collection_block::init(ctx);
    terminal::ssh::install_tmux::init(ctx);
    terminal::ssh::mnemosify::init(ctx);
    terminal::ssh::error::init(ctx);
    context_chips::display_menu::init(ctx);
    context_chips::node_version_popup::init(ctx);
    env_vars::view::env_var_collection::init(ctx);
    ai::agent::todos::popup::init(ctx);
    terminal::view::init_environment::mode_selector::init(ctx);
    coding_entrypoints::project_buttons::init(ctx);
    if FeatureFlag::CodeReviewSaveChanges.is_enabled() {
        code_review::init(ctx);
    }

    let display_count = ctx.windows().display_count();
    ctx.add_singleton_model(|_| DisplayCount(display_count));

    ctx.add_singleton_model(|_| RelaunchModel::new());
    // Mnemos IDE: the window-launch changelog fetch waits for the first frame (see below).
    let changelog_model =
        ctx.add_singleton_model(|_| ChangelogModel::new_deferred(server_api.clone()));
    ctx.on_first_frame_drawn(move |ctx| {
        changelog_model.update(ctx, |model, ctx| model.release_deferred(ctx));
    });
    ctx.add_singleton_model(|_| GitHubAuthNotifier::new());
    ctx.add_singleton_model(|_| NetworkStatus::new());
    ctx.add_singleton_model(|_| SystemStats::new());
    ctx.add_singleton_model(|_| KeybindingChangedNotifier::new());
    ctx.add_singleton_model(|_| search::command_palette::SelectedItems::new());
    ctx.add_singleton_model(search::files::model::FileSearchModel::new);
    ctx.add_singleton_model(|_| VimRegisters::new());
    ctx.add_singleton_model(UndoCloseStack::new);
    ctx.add_singleton_model(|_| ToastStack);
    ctx.add_singleton_model(|_| GlobalCodeReviewModel);
    ctx.add_singleton_model(workspace::OneTimeModalModel::new);
    ctx.add_singleton_model(
        workspace::bonus_grant_notification_model::BonusGrantNotificationModel::new,
    );
    #[cfg(feature = "local_fs")]
    ctx.add_singleton_model(FileModel::new);
    ctx.add_singleton_model(GlobalBufferModel::new);
    #[cfg(windows)]
    ctx.add_singleton_model(util::traffic_lights::windows::RendererState::new);
    #[cfg(feature = "local_fs")]
    ctx.add_singleton_model(|_| LanguageServerShutdownManager::new());

    let notebooks = cloud_objects
        .iter()
        .filter_map(|object| {
            let notebook: Option<&CloudNotebook> = object.into();
            notebook
        })
        .cloned()
        .collect::<Vec<_>>();

    let mut all_queue_items = Vec::new();
    let objects_with_pending_changes = cloud_objects
        .iter()
        .filter(|object| object.metadata().has_pending_content_changes())
        .cloned()
        .collect::<Vec<_>>();
    all_queue_items.extend(QueueItem::from_cached_objects(
        objects_with_pending_changes.into_iter(),
    ));

    let cloud_model = ctx.add_singleton_model(|_ctx| {
        CloudModel::new(
            persistence_writer.sender(),
            cloud_objects,
            time_of_next_force_object_refresh,
        )
    });

    let unsynced_actions: Vec<(CloudObjectTypeAndId, ObjectAction)> = object_actions
        .iter()
        .filter(|action| action.is_pending())
        .filter_map(|action| {
            cloud_model.read(ctx, |model, _| {
                let object = model.get_by_uid(&action.uid);
                object.map(|o| (o.cloud_object_type_and_id(), action.clone()))
            })
        })
        .collect::<Vec<_>>();

    all_queue_items.extend(QueueItem::from_unsynced_actions(
        unsynced_actions.into_iter(),
    ));

    ctx.add_singleton_model(|ctx| {
        SyncQueue::new(
            all_queue_items,
            server_api_provider.as_ref(ctx).get_cloud_objects_client(),
            ctx,
        )
    });

    {
        let conversations = &multi_agent_conversations;
        ctx.add_singleton_model(move |_| BlocklistAIHistoryModel::new(ai_queries, conversations));
    }
    ctx.add_singleton_model(move |_| RestoredAgentConversations::new(multi_agent_conversations));
    ctx.add_singleton_model(|_| CLIAgentSessionsModel::new());
    #[cfg(not(target_family = "wasm"))]
    {
        // Option D file-based CLI-agent event ingestion (Windows OSC fallback).
        // Sweep any event files leaked by a previous crash before the watcher
        // starts — nothing references them at boot.
        crate::terminal::cli_agent_sessions::file_ingest::sweep_stale_event_files();
        // Re-apply the hook patch in case an already-installed plugin's
        // warp-notify.sh was overwritten (e.g. plugin update outside Mnemos).
        // Idempotent. The auto-install/update paths also re-patch on success.
        crate::terminal::cli_agent_sessions::plugin_manager::claude::patch_warp_notify_for_file_ingest();
        ctx.add_singleton_model(
            crate::terminal::cli_agent_sessions::file_ingest::CLIAgentEventFileIngestor::new,
        );
        // Mnemos IDE: the team MCP server (off unless agents.team.enabled).
        ctx.add_singleton_model(crate::workspace::team_control::TeamControl::new);
    }
    // ActiveAgentViewsModel is used to track active agent conversations and notify listeners when they change.
    ctx.add_singleton_model(|_| ActiveAgentViewsModel::new());
    ctx.add_singleton_model(AgentNotificationsModel::new);
    ctx.add_singleton_model(BlocklistAIPermissions::new);
    ctx.add_singleton_model(ai::blocklist::orchestration_events::OrchestrationEventService::new);
    ctx.add_singleton_model(ai::blocklist::task_status_sync_model::TaskStatusSyncModel::new);
    if mnemos_core::features::FeatureFlag::OrchestrationV2.is_enabled() {
        ctx.add_singleton_model(
            ai::blocklist::orchestration_event_poller::OrchestrationEventPoller::new,
        );
    }

    ctx.add_singleton_model(RepoOutlines::new);
    ctx.add_singleton_model(|ctx| {
        mnemos_core::sync_queue::SyncQueue::<SyncTask>::new_with_rate_limit(
            &ctx.background_executor(),
            Some(DEFAULT_SYNC_REQUESTS_PER_MIN),
        )
    });

    ctx.add_singleton_model(|_| UserProfiles::new(restored_user_profiles));

    ctx.add_singleton_model(|_| ObjectActions::new(object_actions));

    ctx.add_singleton_model(|_| AudibleBell::new());

    // This model has to be registered after the user workspaces model because it relies on it,
    // and before the UpdateManager models because they rely on the TeamTester model.
    ctx.add_singleton_model(TeamTesterStatus::new);

    ctx.add_singleton_model(|ctx| {
        TeamUpdateManager::new(
            server_api_provider.as_ref(ctx).get_team_client(),
            persistence_writer.sender(),
            ctx,
        )
    });

    ctx.add_singleton_model(|ctx| {
        UpdateManager::new(
            persistence_writer.sender(),
            server_api_provider.as_ref(ctx).get_cloud_objects_client(),
            ctx,
        )
    });

    let toml_file_path = settings::user_preferences_toml_file_path();
    ctx.add_singleton_model(move |ctx| {
        initialize_cloud_preferences_syncer(
            toml_file_path,
            startup_toml_parse_error_for_syncer.as_deref(),
            ctx,
        )
    });

    // LogManager must be registered before any subsystem (e.g. MCP, LSP) that creates file-based loggers.
    ctx.add_singleton_model(|_| simple_logger::manager::LogManager::new());

    let running_mcp_servers = app_state
        .as_ref()
        .map(|app_state| app_state.running_mcp_servers.as_slice())
        .unwrap_or(&[]);

    // FileMCPWatcher must be registered before FileBasedMCPManager, which subscribes to it.
    ctx.add_singleton_model(FileMCPWatcher::new);
    ctx.add_singleton_model(FileBasedMCPManager::new);

    // TemplatableMCPServerManager must be registered after UpdateManager and MCPServerManager so it can migrate legacy MCPs on start up
    // It should also be registered after FileBasedMCPManager so it can receive file-based server updates.
    ctx.add_singleton_model(|ctx| {
        TemplatableMCPServerManager::new(
            persisted_mcp_server_installations,
            mcp_servers_to_restore,
            running_mcp_servers,
            ctx,
        )
    });

    // MCPGalleryManager subscribes to UpdateManager so that it can be notified when gallery items are updated.
    // The registration of this singleton must be after UpdateManager is registered.
    ctx.add_singleton_model(MCPGalleryManager::new);

    // SkillManager is used to cache SKILL.md files for all active terminal views and their working directories
    ctx.add_singleton_model(SkillManager::new);

    // CloudViewModel subscribes to UpdateManager so that it can be notified when objects are
    // created on the server.
    ctx.add_singleton_model(CloudViewModel::new);

    // AIDocumentModel subscribes to UpdateManager so that it can be notified when notebooks are created on the server.
    ctx.add_singleton_model(AIDocumentModel::new);

    // AgentConversationsModel subscribes to UpdateManager for RTC task updates.
    ctx.add_singleton_model(AgentConversationsModel::new);

    // ByoLlmAuthBannerSessionState tracks dismissal of the BYO LLM auth banner (e.g., AWS Bedrock login).
    ctx.add_singleton_model(ByoLlmAuthBannerSessionState::new);

    ctx.add_singleton_model(ExportManager::new);
    ctx.add_singleton_model(|ctx| NotebookManager::new(notebooks, ctx));
    ctx.add_singleton_model(|_| CodeManager::default());
    ctx.add_singleton_model(|_| OpenedFilesModel::new());
    ctx.add_singleton_model(NotebookKeybindings::new);
    ctx.add_singleton_model(TerminalKeybindings::new);
    ctx.add_singleton_model(|_| ActiveSession::default());
    ctx.add_singleton_model(|ctx| {
        Listener::new(
            server_api_provider.as_ref(ctx).get_cloud_objects_client(),
            ctx,
        )
    });

    #[cfg(all(not(target_family = "wasm"), feature = "local_tty"))]
    {
        ctx.add_singleton_model(LocalShellState::new);
        ctx.add_singleton_model(system::SystemInfo::new);
    }

    // Add a singleton model that holds the current prompt configuration.
    ctx.add_singleton_model(Prompt::new);

    // Add a singleton model for resizable modals whose size should be persisted through restarts.
    ctx.add_singleton_model(|_| ResizableData::default());

    // Add a singleton model to maintain state of shared session across all windows.
    ctx.add_singleton_model(terminal::shared_session::manager::Manager::new);

    ctx.add_singleton_model(
        terminal::shared_session::permissions_manager::SessionPermissionsManager::new,
    );

    ctx.add_singleton_model(EnvVarCollectionManager::new);
    ctx.add_singleton_model(WorkflowManager::new);

    if FeatureFlag::ScheduledAmbientAgents.is_enabled() {
        ctx.add_singleton_model(ScheduledAgentManager::new);
    }

    AutoupdateState::register(ctx, server_api.clone());

    ctx.add_singleton_model(LocalWorkflows::new);

    ctx.add_singleton_model(LLMPreferences::new);

    ctx.add_singleton_model(|ctx| {
        ai::agent_tips::AITipModel::<ai::AgentTip>::new_for_agent_tips(ctx)
    });

    timer.mark_interval_end("SINGLETON_MODELS_REGISTERED");

    ctx.add_singleton_model(move |_| timer);

    let is_ssh_tmux_wrapper_enabled = ctx
        .private_user_preferences()
        .read_value("SshTmuxWrapperOverride")
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok());

    if let Some(is_ssh_tmux_wrapper_enabled) = is_ssh_tmux_wrapper_enabled {
        FeatureFlag::SSHTmuxWrapper.set_user_preference(is_ssh_tmux_wrapper_enabled);
    }

    ctx.add_singleton_model(|ctx| AIExecutionProfilesModel::new(launch_mode, ctx));

    ctx.add_singleton_model(DefaultTerminal::new);

    ctx.add_singleton_model(|ctx| {
        let indices_to_restore = if UserWorkspaces::as_ref(ctx).is_codebase_context_enabled(ctx)
            && launch_mode.supports_indexing()
        {
            persisted_workspaces.clone()
        } else {
            vec![]
        };

        let codebase_limits = AIRequestUsageModel::as_ref(ctx).codebase_context_limits();

        CodebaseIndexManager::new(
            indices_to_restore,
            codebase_limits.max_indices_allowed,
            codebase_limits.max_files_per_repo,
            codebase_limits.embedding_generation_batch_size,
            server_api_provider.as_ref(ctx).get(),
            ctx,
        )
    });

    ctx.add_singleton_model(|ctx| {
        ProjectContextModel::new_from_persisted(persisted_project_rules, ctx)
    });
    ctx.add_singleton_model(|ctx| {
        PersistedWorkspace::new(
            persisted_workspaces,
            workspace_language_servers,
            persistence_writer.sender(),
            ctx,
        )
    });
    ctx.add_singleton_model(move |_| persistence_writer);

    ctx.add_singleton_model(input_classifier::InputClassifierModel::new);

    ctx.add_singleton_model(move |_| IgnoredSuggestionsModel::new(persisted_ignored_suggestions));

    // Subscribe WorkflowAliases to the UpdateManager so that it can be notified when objects are
    // trashed.
    WorkflowAliases::handle(ctx).update(ctx, |aliases, ctx| {
        aliases.connect(ctx);
    });

    // The local HTTP server on 127.0.0.1:9277 only serves the heap-profiling endpoint, so it
    // runs only in builds with the `jemalloc_pprof` feature. Upstream also served an
    // app-installation probe to its website from it; Mnemos has no website that calls it.
    #[cfg(all(not(target_family = "wasm"), feature = "jemalloc_pprof"))]
    ctx.add_singleton_model(move |ctx| {
        http_server::HttpServer::new(vec![profiling::make_router()], ctx)
    });

    app_state
}

fn app_callbacks(is_integration_test: bool) -> mnemos_ui::platform::AppCallbacks {
    mnemos_ui::platform::AppCallbacks {
        on_internet_reachability_changed: Some(Box::new(move |reachable, ctx| {
            NetworkStatus::handle(ctx)
                .update(ctx, move |me, ctx| me.reachability_changed(reachable, ctx));
        })),
        on_become_active: Some(Box::new(move |ctx| {
            let auth_state = AuthStateProvider::as_ref(ctx).get();
            ctx.record_app_focus(
                auth_state.user_id().map(|uid| uid.as_string()),
                auth_state.anonymous_id(),
            );
        })),
        on_screen_changed: Some(Box::new(move |ctx| {
            ctx.dispatch_global_action(
                "root_view:move_quake_mode_window_from_screen_change",
                &KeysSettings::as_ref(ctx)
                    .quake_mode_settings
                    .value()
                    .clone(),
            );

            let new_display_count = ctx.windows().display_count();
            DisplayCount::handle(ctx).update(ctx, |display_count, ctx| {
                display_count.0 = new_display_count;
                ctx.notify();
            });
        })),
        on_cpu_awakened: Some(Box::new(move |ctx| {
            SystemStats::handle(ctx).update(ctx, move |system, ctx| {
                log::info!("System has returned from sleep");
                system.dispatch_cpu_was_awakened(ctx);
            });
        })),
        on_cpu_will_sleep: Some(Box::new(move |ctx| {
            SystemStats::handle(ctx).update(ctx, move |system, ctx| {
                log::info!("System is going to sleep...");
                system.dispatch_cpu_will_sleep(ctx);
            });
        })),
        on_resigned_active: Some(Box::new(move |ctx| {
            let active_window_id = ctx.windows().active_window();
            let update_quake_mode_arg = UpdateQuakeModeEventArg { active_window_id };

            ctx.dispatch_global_action("root_view:update_quake_mode_state", &update_quake_mode_arg);

            let auth_state = AuthStateProvider::as_ref(ctx).get();
            ctx.record_app_blur(
                auth_state.user_id().map(|uid| uid.as_string()),
                auth_state.anonymous_id(),
            );
        })),
        on_will_terminate: Some(Box::new(move |ctx| {
            // Mnemos IDE: record which panes had a live Claude session before
            // anything starts tearing down. This must come first — the PTY
            // teardown further down this handler ends the sessions we're
            // trying to describe, and `spawn_child_if_necessary` (which
            // launches the updated binary) runs here too, so by the time that
            // fires the snapshot has to already be on disk.
            ide_snapshot_agent_sessions(ctx);

            NotebookManager::handle(ctx).update(ctx, |manager, ctx| {
                // Notebooks are only saved periodically, so ensure that any pending changes have
                // been sent to the writer thread before terminating.
                manager.close_notebooks(ctx);
            });

            PersistenceWriter::handle(ctx).update(ctx, |writer, _ctx| {
                writer.terminate();
            });

            let auth_state = AuthStateProvider::as_ref(ctx).get();
            ctx.try_record_daily_app_focus_duration(
                auth_state.user_id().map(|uid| uid.as_string()),
                auth_state.anonymous_id(),
            );
            TelemetryCollector::handle(ctx).update(ctx, |telemetry_collector, ctx| {
                telemetry_collector.flush_telemetry_events_for_shutdown(ctx);
            });

            // Shutdown all LSP servers gracefully before app termination
            lsp::LspManagerModel::handle(ctx).update(ctx, |manager, ctx| {
                manager.terminate(ctx);
            });

            // We want to tear down the terminal server before relaunching for
            // autoupdate, to ensure we're not running any extra Mnemos processes
            // when we bring up the new process.  Additionally, this must occur
            // after terminating the persistence writer, so we don't keep track
            // of the fact that the shell sessions terminated.
            #[cfg(feature = "local_tty")]
            terminal::local_tty::spawner::PtySpawner::handle(ctx).update(ctx, |pty_spawner, _| {
                pty_spawner.prepare_for_app_termination();
            });

            #[cfg(all(feature = "local_tty", windows))]
            terminal::local_tty::shutdown_all_pty_event_loops(ctx);

            // Tear down app services before spawning the new process, to
            // ensure that the new process doesn't find the old process while
            // attempting to enforce our single-instance policy on Linux.
            app_services::teardown(ctx);
            // Mnemos IDE: not while Windows is ending the session. An update
            // installer (or a relaunch) started now would be killed part way
            // through by the shutdown it is racing.
            if mnemos_ui::platform::app::os_session_ending() {
                log::info!("Session is ending, not relaunching or installing an update");
            } else {
                autoupdate::spawn_child_if_necessary(ctx);
            }

            // Tear down any application profilers that are running, writing
            // results to disk.
            profiling::teardown();

            #[cfg(enable_crash_recovery)]
            crash_recovery::CrashRecovery::handle(ctx).update(ctx, |crash_recovery, _ctx| {
                crash_recovery.teardown();
            });

            // Tear down crash reporting as the last thing we do before the application
            // terminates.
            #[cfg(feature = "crash_reporting")]
            crash_reporting::uninit_sentry();
        })),
        on_should_close_window: Some(Box::new(move |window_id, ctx| {
            let general_settings = GeneralSettings::as_ref(ctx);
            // On Linux or Windows, if we're about to close the final window, we should quit the app instead.
            // On Mac, we do this conditionally based on a user setting.
            let quit_on_last_window_closed = cfg!(any(target_os = "linux", windows))
                || *general_settings.quit_on_last_window_closed;
            if ctx.window_ids().count() == 1 && quit_on_last_window_closed {
                log::info!("No windows left, terminating app");
                ctx.terminate_app(TerminationMode::Cancellable, None);
                return ApproveTerminateResult::Cancel;
            }

            let summary = UnsavedStateSummary::for_window(window_id, ctx);

            send_telemetry_from_app_ctx!(
                TelemetryEvent::UserInitiatedClose {
                    initiated_on: CloseTarget::Window,
                },
                ctx
            );

            // Don't show dialog on integration test. Machine can't press buttons.
            if !is_integration_test && summary.should_display_warning(ctx) {
                let shown = summary
                    .dialog()
                    .on_confirm(move |ctx| {
                        ctx.windows()
                            .close_window(window_id, TerminationMode::ForceTerminate);
                    })
                    .on_cancel(move |ctx| {
                        on_close_window_cancelled(window_id, false, ctx);
                    })
                    .on_show_processes(move |ctx| {
                        on_close_window_cancelled(window_id, true, ctx);
                    })
                    .show(ctx);
                if shown {
                    ApproveTerminateResult::Cancel
                } else {
                    ApproveTerminateResult::Terminate
                }
            } else {
                ApproveTerminateResult::Terminate
            }
        })),
        on_should_terminate_app: Some(Box::new(move |ctx| {
            send_telemetry_from_app_ctx!(
                TelemetryEvent::UserInitiatedClose {
                    initiated_on: CloseTarget::App,
                },
                ctx
            );

            // A pending autoupdate is applied only once the quit is confirmed: either the user
            // confirmed the unsaved-state dialog, or no dialog was needed. A cancelled quit never
            // touches the update, so the stage stays at UpdateReady and polling keeps working.
            // After an apply, termination is forced so the dialog cannot come back on top of a
            // swapped bundle.
            let summary = UnsavedStateSummary::for_app(ctx);
            // Don't show dialog on integration test. Machine can't press buttons.
            if !is_integration_test && summary.should_display_warning(ctx) {
                let shown = summary
                    .dialog()
                    .on_confirm(|ctx| {
                        if !autoupdate::apply_pending_update(
                            ctx,
                            force_terminate_after_pending_update,
                        ) {
                            ctx.terminate_app(TerminationMode::ForceTerminate, None);
                        }
                    })
                    .on_show_processes(|ctx| on_close_app_cancelled(true, ctx))
                    .on_cancel(|ctx| on_close_app_cancelled(false, ctx))
                    .show(ctx);
                if shown {
                    return ApproveTerminateResult::Cancel;
                }
            }

            // No dialog was shown, so the quit is confirmed. The apply may be async (macOS), so
            // hold the quit here and let its completion callback terminate the app.
            if autoupdate::apply_pending_update(ctx, force_terminate_after_pending_update) {
                return ApproveTerminateResult::Cancel;
            }

            ApproveTerminateResult::Terminate
        })),
        on_disable_warning_modal: Some(Box::new(move |ctx| {
            GeneralSettings::handle(ctx).update(ctx, |general_settings, ctx| {
                report_if_error!(general_settings
                    .show_warning_before_quitting
                    .toggle_and_save_value(ctx));
            });
            send_telemetry_from_app_ctx!(TelemetryEvent::QuitModalDisabled, ctx);
        })),
        on_notification_clicked: Some(Box::new(move |notification_response, ctx| {
            if let Some(notification_data) = notification_response.data() {
                let context: serde_json::Result<NotificationContext> =
                    serde_json::from_str(notification_data);
                if let Ok(NotificationContext::BlockOrigin {
                    window_id,
                    pane_group_id,
                    pane_id,
                }) = context
                {
                    // Ensure the window ID exists, if so dispatch an action to focus
                    // the correct pane.
                    if ctx.window_ids().contains(&window_id) {
                        if let Some(root_view_id) = ctx.root_view_id(window_id) {
                            ctx.dispatch_action(
                                window_id,
                                &[root_view_id],
                                "root_view:handle_notification_click",
                                &PaneViewLocator {
                                    pane_group_id,
                                    pane_id,
                                },
                                log::Level::Info,
                            );
                        }
                    }
                }
            }
        })),
        on_new_window_requested: Some(Box::new(move |ctx| {
            // This one is called when the app is requested to open a new window,
            // e.g. clicking on the Dock icon. It is NOT called from the New Window
            // menu item.
            App::record_last_active_timestamp();
            ctx.dispatch_global_action("root_view:open_new", &());
            ctx.dispatch_global_action("workspace:save_app", &());
        })),
        on_open_urls: Some(Box::new(move |urls, ctx| {
            for url in &urls {
                let parsed_url = Url::parse(url);
                match parsed_url {
                    Ok(url) => uri::handle_incoming_uri(&url, ctx),
                    Err(e) => log::warn!("Unable to parse received url: {e}"),
                }
            }
        })),
        on_os_appearance_changed: Some(Box::new(move |ctx| {
            AppearanceManager::handle(ctx).update(ctx, |appearance_manager, ctx| {
                appearance_manager.refresh_theme_state(ctx);
            });
        })),
        on_active_window_changed: Some(Box::new(move |ctx| {
            let windowing_model = ctx.windows();
            let active_window_id = windowing_model.active_window();
            let key_window_is_modal_panel = windowing_model.key_window_is_modal_panel();

            if !key_window_is_modal_panel {
                let update_quake_mode_arg = UpdateQuakeModeEventArg { active_window_id };
                ctx.dispatch_global_action(
                    "root_view:update_quake_mode_state",
                    &update_quake_mode_arg,
                );
            }

            if let Some(active_window_id) = active_window_id {
                OneTimeModalModel::handle(ctx).update(ctx, |model, ctx| {
                    model.update_target_window_id(active_window_id, ctx);
                });
            }

            ctx.dispatch_global_action("workspace:save_app", &());
        })),
        on_window_will_close: Some(Box::new(move |closed_window_data, ctx| {
            if ctx.windows().stage() == ApplicationStage::Terminating {
                return;
            }

            if let Some(window_data) = closed_window_data {
                UndoCloseStack::handle(ctx).update(ctx, |stack, ctx| {
                    stack.handle_window_closed(window_data, ctx);
                });
            }
            ctx.dispatch_global_action("workspace:save_app", &());
        })),
        on_window_moved: Some(Box::new(move |ctx| {
            ctx.dispatch_global_action("workspace:save_app", &());
        })),
        on_window_resized: Some(Box::new(move |ctx| {
            ctx.dispatch_global_action("workspace:save_app", &());
        })),
        ..Default::default()
    }
}

/// Focuses the active window or if there isn't one then a window with a running process
/// and then shows the native modal.
fn focus_running_window_and_show_native_modal(
    sessions_summary: RunningSessionSummary,
    dialog_with_callbacks: AlertDialogWithCallbacks<AppModalCallback>,
    ctx: &mut AppContext,
) {
    let windowing_model = ctx.windows();
    let active_window_id = windowing_model.active_window();
    // Show the nav palette in the active window. If there is no active window,
    // arbitrarily pick one of the windows having a running process.
    let window_id_to_focus = active_window_id.unwrap_or_else(|| {
        *sessions_summary
            .windows_running()
            .iter()
            .next()
            .expect("already checked len > 0")
    });
    ctx.windows().show_window_and_focus_app(window_id_to_focus);
    if let Some(workspaces) = ctx.views_of_type::<Workspace>(window_id_to_focus) {
        if let Some(handle) = workspaces.first() {
            handle.update(ctx, |view, ctx| {
                view.show_native_modal(dialog_with_callbacks, ctx);
            });
        }
    }
}

/// Mnemos IDE: write the pane-uuid → Claude-session-id map for every pane that has a
/// live agent, so the next launch can resume them in place.
///
/// Called from `on_will_terminate`, which every app-driven exit passes through:
/// a menu quit, the last window closing, and
/// `autoupdate::initiate_relaunch_for_update` all end at `terminate_app`, and
/// this callback is what runs before the process goes. There is no separate
/// "about to autoupdate" hook to hang this on, and there doesn't need to be:
/// an update is just one reason the process ends.
///
/// A Windows shutdown, restart or logoff gets here too: the hidden end-session
/// window in `mnemos_ui` (`windowing/winit/windows/end_session.rs`) runs the
/// same terminate step on `WM_ENDSESSION`, inside the handler, and raises the
/// process shutdown level so the shells are still alive when it does.
///
/// Deliberately quiet and total: a failure here loses the ability to resume,
/// which is a disappointment, not a reason to obstruct shutdown. Nothing in it
/// blocks (no network, one small JSON file), because a terminate handler that
/// waits is a terminate handler that hangs.
fn ide_snapshot_agent_sessions(ctx: &mut AppContext) {
    if mnemos_ide_agent_restore::is_disabled() {
        return;
    }

    let mut entries = Vec::new();
    let window_ids: Vec<_> = ctx.window_ids().collect();
    for window_id in window_ids {
        let Some(workspaces) = ctx.views_of_type::<Workspace>(window_id) else {
            continue;
        };
        for workspace in workspaces {
            entries.extend(workspace.as_ref(ctx).ide_agent_session_entries(ctx));
        }
    }

    // `from_entries` is where the subagent collision is handled: Claude Code
    // files a subagent's transcript under its parent's session uuid, so two
    // panes can report the same `session_id`. Resuming it once per pane would
    // fork one conversation into several.
    // Entries this run's restore took but did not run (their pane was in
    // use) ride along after the live ones, so they are not lost.
    let entries = mnemos_ide_agent_restore::with_deferred(entries);
    let snapshot = mnemos_ide_agent_restore::RestoreSnapshot::from_entries(entries);
    let path = match mnemos_ide_agent_restore::snapshot_path() {
        Ok(path) => path,
        Err(e) => {
            log::warn!(target: "mnemos_ide", "no agent-session snapshot path: {e}");
            return;
        }
    };
    match snapshot.save_blocking(&path) {
        // An empty snapshot deletes the file, so this also reports the
        // "nothing was running" case honestly.
        Ok(()) => log::info!(
            target: "mnemos_ide",
            "snapshotted {} resumable agent session(s)",
            snapshot.entries.len()
        ),
        Err(e) => log::warn!(target: "mnemos_ide", "could not snapshot agent sessions: {e}"),
    }
}

/// Completion callback for `autoupdate::apply_pending_update` on a confirmed quit.
///
/// It runs whether or not the apply succeeded (autoupdate logs the outcome itself), so the
/// log text only says what is known here. On macOS it runs inside AutoupdateState's own
/// completion, so it must not read or update AutoupdateState. The termination is forced:
/// a cancellable one would re-enter `on_should_terminate_app` and could show the quit dialog
/// again after the bundle was already swapped.
fn force_terminate_after_pending_update(ctx: &mut AppContext) {
    let message = if cfg!(target_os = "macos") {
        "Pending update apply finished, terminating app"
    } else if cfg!(windows) {
        "Terminating app, pending update installs at exit"
    } else {
        "Terminating app with a pending update"
    };
    log::info!("{message}");
    ctx.terminate_app(TerminationMode::ForceTerminate, None);
}

fn on_close_app_cancelled(open_navigation_palette: bool, ctx: &mut AppContext) {
    autoupdate::cancel_relaunch(ctx);

    send_telemetry_from_app_ctx!(
        TelemetryEvent::QuitModalCancel {
            nav_palette: open_navigation_palette,
            modal_for: CloseTarget::App,
        },
        ctx
    );

    let sessions = SessionNavigationData::all_sessions(ctx).collect_vec();
    let sessions_summary = RunningSessionSummary::new(&sessions);

    // If open_navigation_palette is false, return early. Otherwise, we honor the open_navigation_palette
    // param which is true if the user clicked the modal button for that. However, if the running
    // processes in this window have finished since the modal popped, there is nothing to do now and we
    // can return early
    if !open_navigation_palette || sessions_summary.long_running_cmds.is_empty() {
        return;
    }

    let windowing_model = ctx.windows();
    let active_window_id = windowing_model.active_window();
    // show the nav palette in the active window. if there is no active window,
    // arbitrarily pick one of the windows having a running process
    let window_id_to_focus = active_window_id.unwrap_or_else(|| {
        *sessions_summary
            .windows_running()
            .iter()
            .next()
            .expect("already checked len > 0")
    });

    windowing_model.show_window_and_focus_app(window_id_to_focus);

    // open the nav palette in the selected window
    if let Some(workspaces) = ctx.views_of_type::<Workspace>(window_id_to_focus) {
        if let Some(handle) = workspaces.first() {
            ctx.dispatch_typed_action_for_view(
                window_id_to_focus,
                handle.id(),
                &WorkspaceAction::OpenPalette {
                    mode: PaletteMode::Navigation,
                    source: PaletteSource::QuitModal,
                    query: Some("running".to_owned()),
                },
            );
        }
    }
}

fn on_close_window_cancelled(
    window_id: WindowId,
    open_navigation_palette: bool,
    ctx: &mut AppContext,
) {
    send_telemetry_from_app_ctx!(
        TelemetryEvent::QuitModalCancel {
            nav_palette: open_navigation_palette,
            modal_for: CloseTarget::Window,
        },
        ctx
    );

    let sessions = SessionNavigationData::all_sessions(ctx).collect_vec();
    let sessions_summary = RunningSessionSummary::new(&sessions);
    let num_processes_in_window = sessions_summary.processes_in_window(&window_id).len();

    // If open_navigation_palette is false, return early. Otherwise, we honor the
    // open_navigation_palette param which is true if the user clicked the modal
    // button for that. However, if the running processes in this window have finished
    // since the modal popped, there is nothing to do now and we can return early
    if !open_navigation_palette || num_processes_in_window == 0 {
        return;
    }

    ctx.windows().show_window_and_focus_app(window_id);

    // if we haven't returned early, it means open_navigation_palette is true as the
    // user pressed the modal button for opening the navigation palette to show their
    // running processes
    if let Some(workspaces) = ctx.views_of_type::<Workspace>(window_id) {
        if let Some(handle) = workspaces.first() {
            ctx.dispatch_typed_action_for_view(
                window_id,
                handle.id(),
                &WorkspaceAction::OpenPalette {
                    mode: PaletteMode::Navigation,
                    source: PaletteSource::QuitModal,
                    query: Some("running".to_owned()),
                },
            );
        }
    }
}

fn launch(ctx: &mut mnemos_ui::AppContext, app_state: Option<AppState>, launch_mode: LaunchMode) {
    IntervalTimer::handle(ctx).update(ctx, |timer, _ctx| {
        timer.mark_interval_end("APP_LAUNCHED");
    });

    keyboard::load_custom_keybindings(ctx);

    IntervalTimer::handle(ctx).update(ctx, |timer, _ctx| {
        timer.mark_interval_end("KEYBINDINGS_LOADED");
    });

    // For now, we only specify application-level fallback fonts on web.
    #[cfg(target_family = "wasm")]
    ctx.set_fallback_font_fn(font_fallback::fallback_font_fn);

    match &launch_mode {
        LaunchMode::App { .. } | LaunchMode::Test { .. } => {
            // Attempt to restore windows from the persisted application state.
            let arg = OpenFromRestoredArg { app_state };
            ctx.dispatch_global_action("root_view:open_from_restored", &arg);

            // Process any URLs that were provided on the command line (which may be
            // file:// URLs or ones using our custom URL scheme).
            for url in launch_mode.args().urls.iter() {
                uri::handle_incoming_uri(url, ctx);
            }
            // Mnemos IDE: the Explorer verbs' --new-tab / --new-window.
            for url in uri::local_launch_urls(&launch_mode.args()) {
                uri::handle_incoming_uri(&url, ctx);
            }

            // If, after session restoration and command-line argument handling, we
            // haven't opened any windows, open a new window.
            if ctx.window_ids().count() == 0 {
                ctx.dispatch_global_action("root_view:open_new", &());
            }

            IntervalTimer::handle(ctx).update(ctx, |timer, _| {
                timer.mark_interval_end("WINDOWS_CREATED");
            });

            // TODO(ben): We should skip this for LaunchMode::Test.
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            {
                use crate::login_item::maybe_register_app_as_login_item;
                use crate::terminal::general_settings::GeneralSettingsChangedEvent;
                // Note that we put this here because it depends on settings already having been initialized.
                ctx.subscribe_to_model(&GeneralSettings::handle(ctx), |_, event, ctx| {
                    if matches!(event, GeneralSettingsChangedEvent::LoginItem { .. }) {
                        maybe_register_app_as_login_item(ctx);
                    }
                });
                maybe_register_app_as_login_item(ctx);
            }
        }
        #[cfg_attr(target_family = "wasm", allow(unused_variables))]
        LaunchMode::CommandLine {
            command,
            global_options,
            ..
        } => {
            cfg_if::cfg_if! {
                if #[cfg(target_family = "wasm")] {
                    panic!("Cannot execute CLI command {command:?} on the web");
                } else {
                    if let Err(err) = crate::ai::agent_sdk::run(ctx, command.clone(), global_options.clone()) {
                        eprintln!("{err:#}");
                        report_error!(err);
                        std::process::exit(1);
                    }
                }
            }
        }
    }
}

/// Initializes the logger before running tests.
///
/// The `ctor` attribute here means that this runs BEFORE main(), whenever the
/// binary is executed. For this reason, we need to ensure that this function
/// only exists within unit test code. Production bundles and integration tests
/// also initialize the logging system, and initializing it twice causes a panic.
///
/// Additionally, we must not write anything to stdout in this function, as it
/// can interfere with test harnesses collecting the set of tests to run. (This
/// is why we're not simply calling the init() function above.)
#[ctor::ctor]
#[cfg(test)]
fn init_logging_for_unit_tests_glue() {
    // Initialize terminal-friendly logging for tests from the shared logger crate.
    mnemos_logging::init_logging_for_unit_tests();
}

/// Mnemos IDE: sends every admin-login keyring access in the unit-test process to
/// mnemos-ide-sync's in-memory store before any test runs. The OS store holds the
/// developer's real login, and a test that reads, overwrites or deletes it
/// breaks their daily driver (one wiped it on 2026-09-24). mnemos-ide-sync's own
/// `cfg!(test)` switch does not apply here, since the app compiles it as a
/// plain dependency, and a per-test opt-in is one forgotten call away from
/// doing it again.
#[ctor::ctor]
#[cfg(test)]
fn use_in_memory_keyring_for_unit_tests() {
    mnemos_ide_sync::auth::use_in_memory_keyring();
}

/// Mark all features which should be enabled on the current channel as enabled.
/// This sets global feature flag state and should never be called in a unit test.
pub fn init_feature_flags() {
    for flag in enabled_features() {
        flag.set_enabled(true);
    }
    features::mark_initialized();
}

/// Returns all feature flags which should be enabled in the current channel.
pub fn enabled_features() -> HashSet<FeatureFlag> {
    // Enable features overridden for the given channel.
    let mut flags = ChannelState::additional_features();

    // Enable flags for release builds, if appropriate.
    if ChannelState::is_release_bundle() {
        flags.extend(features::RELEASE_FLAGS);
    }

    flags.extend([
        #[cfg(feature = "autoupdate")]
        FeatureFlag::Autoupdate,
        #[cfg(feature = "changelog")]
        FeatureFlag::Changelog,
        #[cfg(feature = "cocoa_sentry")]
        FeatureFlag::CocoaSentry,
        #[cfg(feature = "crash_reporting")]
        FeatureFlag::CrashReporting,
        #[cfg(feature = "log_expensive_frames_in_sentry")]
        FeatureFlag::LogExpensiveFramesInSentry,
        #[cfg(feature = "record_app_active_events")]
        FeatureFlag::RecordAppActiveEvents,
        #[cfg(feature = "runtime_feature_flags")]
        FeatureFlag::RuntimeFeatureFlags,
        #[cfg(feature = "sequential_storage")]
        FeatureFlag::SequentialStorage,
        #[cfg(feature = "in_band_generators_ssh")]
        FeatureFlag::InBandGeneratorsForSSH,
        #[cfg(feature = "run_generators_with_cmd_exe")]
        FeatureFlag::RunGeneratorsWithCmdExe,
        #[cfg(feature = "ligatures")]
        FeatureFlag::Ligatures,
        #[cfg(feature = "selectable_prompt")]
        FeatureFlag::SelectablePrompt,
        #[cfg(feature = "viewing_shared_sessions")]
        FeatureFlag::ViewingSharedSessions,
        #[cfg(feature = "creating_shared_sessions")]
        FeatureFlag::CreatingSharedSessions,
        #[cfg(feature = "agent_mode")]
        FeatureFlag::AgentMode,
        #[cfg(feature = "shared_session_long_running_commands")]
        FeatureFlag::SharedSessionWriteToLongRunningCommands,
        #[cfg(feature = "resize_fix")]
        FeatureFlag::ResizeFix,
        #[cfg(feature = "richtext_multiselect")]
        FeatureFlag::RichTextMultiselect,
        #[cfg(feature = "default_waterfall_mode")]
        FeatureFlag::DefaultWaterfallMode,
        #[cfg(feature = "settings_file")]
        FeatureFlag::SettingsFile,
        #[cfg(feature = "settings_import")]
        FeatureFlag::SettingsImport,
        #[cfg(feature = "rect_selection")]
        FeatureFlag::RectSelection,
        #[cfg(feature = "alacritty_settings_import")]
        FeatureFlag::AlacrittySettingsImport,
        #[cfg(feature = "dynamic_workflow_enums")]
        FeatureFlag::DynamicWorkflowEnums,
        #[cfg(feature = "shared_with_me")]
        FeatureFlag::SharedWithMe,
        #[cfg(feature = "am_workflows")]
        FeatureFlag::AgentModeWorkflows,
        #[cfg(feature = "ai_rules")]
        FeatureFlag::AIRules,
        #[cfg(feature = "ssh_tmux_wrapper")]
        FeatureFlag::SSHTmuxWrapper,
        #[cfg(feature = "less_horizontal_terminal_padding")]
        FeatureFlag::LessHorizontalTerminalPadding,
        #[cfg(feature = "shell_selector")]
        FeatureFlag::ShellSelector,
        #[cfg(feature = "block_toolbelt_save_as_workflow")]
        FeatureFlag::BlockToolbeltSaveAsWorkflow,
        #[cfg(feature = "integration_command")]
        FeatureFlag::IntegrationCommand,
        #[cfg(feature = "artifact_command")]
        FeatureFlag::ArtifactCommand,
        #[cfg(feature = "cloud_environments")]
        FeatureFlag::CloudEnvironments,
        #[cfg(all(feature = "simulate_github_unauthed", debug_assertions))]
        FeatureFlag::SimulateGithubUnauthed,
        #[cfg(feature = "session_sharing_acls")]
        FeatureFlag::SessionSharingAcls,
        #[cfg(feature = "full_screen_zen_mode")]
        FeatureFlag::FullScreenZenMode,
        #[cfg(feature = "minimalist_ui")]
        FeatureFlag::MinimalistUI,
        #[cfg(feature = "remove_alt_screen_padding")]
        FeatureFlag::RemoveAltScreenPadding,
        #[cfg(feature = "avatar_in_tab_bar")]
        FeatureFlag::AvatarInTabBar,
        #[cfg(feature = "workflow_aliases")]
        FeatureFlag::WorkflowAliases,
        #[cfg(feature = "ssh_drag_and_drop")]
        FeatureFlag::SshDragAndDrop,
        #[cfg(feature = "drag_tabs_to_windows")]
        FeatureFlag::DragTabsToWindows,
        #[cfg(feature = "pane_leaf_groups")]
        FeatureFlag::PaneLeafGroups,
        #[cfg(feature = "cycle_next_command_suggestion")]
        FeatureFlag::CycleNextCommandSuggestion,
        #[cfg(feature = "multi_workspace")]
        FeatureFlag::MultiWorkspace,
        #[cfg(feature = "ime_marked_text")]
        FeatureFlag::ImeMarkedText,
        #[cfg(feature = "partial_next_command_suggestions")]
        FeatureFlag::PartialNextCommandSuggestions,
        #[cfg(feature = "iterm_images")]
        FeatureFlag::ITermImages,
        #[cfg(feature = "validate_autosuggestions")]
        FeatureFlag::ValidateAutosuggestions,
        #[cfg(feature = "prompt_suggestions_via_maa")]
        FeatureFlag::PromptSuggestionsViaMAA,
        #[cfg(feature = "clear_autosuggestion_on_escape")]
        FeatureFlag::ClearAutosuggestionOnEscape,
        #[cfg(feature = "autoupdate_ui_revamp")]
        FeatureFlag::AutoupdateUIRevamp,
        #[cfg(all(not(windows), feature = "kitty_images"))]
        FeatureFlag::KittyImages,
        #[cfg(feature = "mnemos_packs")]
        FeatureFlag::MnemosPacks,
        #[cfg(feature = "global_ai_analytics_banner")]
        FeatureFlag::GlobalAIAnalyticsBanner,
        #[cfg(feature = "global_ai_analytics_collection")]
        FeatureFlag::GlobalAIAnalyticsCollection,
        #[cfg(feature = "agent_mode_primary_xml")]
        FeatureFlag::AgentModePrimaryXML,
        #[cfg(feature = "agent_mode_pre_plan_xml")]
        FeatureFlag::AgentModePrePlanXML,
        #[cfg(feature = "agent_onboarding")]
        FeatureFlag::AgentOnboarding,
        #[cfg(feature = "agent_shared_sessions")]
        FeatureFlag::AgentSharedSessions,
        #[cfg(feature = "suggested_rules")]
        FeatureFlag::SuggestedRules,
        #[cfg(feature = "suggested_agent_mode_workflows")]
        FeatureFlag::SuggestedAgentModeWorkflows,
        #[cfg(feature = "command_correction_key")]
        FeatureFlag::CommandCorrectionKey,
        #[cfg(feature = "predict_am_queries")]
        FeatureFlag::PredictAMQueries,
        #[cfg(feature = "full_source_code_embedding")]
        FeatureFlag::FullSourceCodeEmbedding,
        #[cfg(feature = "use_tantivy_search")]
        FeatureFlag::UseTantivySearch,
        #[cfg(feature = "grep_tool")]
        FeatureFlag::GrepTool,
        #[cfg(feature = "mcp_server")]
        FeatureFlag::McpServer,
        #[cfg(feature = "mcp_debugging_ids")]
        FeatureFlag::McpDebuggingIds,
        #[cfg(feature = "markdown_tables")]
        FeatureFlag::MarkdownTables,
        #[cfg(feature = "blocklist_markdown_table_rendering")]
        FeatureFlag::BlocklistMarkdownTableRendering,
        #[cfg(feature = "blocklist_markdown_images")]
        FeatureFlag::BlocklistMarkdownImages,
        #[cfg(feature = "markdown_mermaid")]
        FeatureFlag::MarkdownMermaid,
        #[cfg(feature = "editable_markdown_mermaid")]
        FeatureFlag::EditableMarkdownMermaid,
        #[cfg(feature = "image_as_context")]
        FeatureFlag::ImageAsContext,
        #[cfg(feature = "msys2_shells")]
        FeatureFlag::MSYS2Shells,
        #[cfg(feature = "file_retrieval_tools")]
        FeatureFlag::FileRetrievalTools,
        #[cfg(feature = "reload_stale_conversation_files")]
        FeatureFlag::ReloadStaleConversationFiles,
        #[cfg(feature = "retry_truncated_code_responses")]
        FeatureFlag::RetryTruncatedCodeResponses,
        #[cfg(feature = "read_image_files")]
        FeatureFlag::ReadImageFiles,
        #[cfg(feature = "usage_based_pricing")]
        FeatureFlag::UsageBasedPricing,
        #[cfg(feature = "cross_repo_context")]
        FeatureFlag::CrossRepoContext,
        #[cfg(feature = "codebase_index_persistence")]
        FeatureFlag::CodebaseIndexPersistence,
        #[cfg(feature = "ai_context_menu")]
        FeatureFlag::AIContextMenuEnabled,
        #[cfg(feature = "at_menu_outside_of_ai_mode")]
        FeatureFlag::AtMenuOutsideOfAIMode,
        #[cfg(feature = "ai_resume_button")]
        FeatureFlag::AIResumeButton,
        #[cfg(feature = "figma_detection")]
        FeatureFlag::FigmaDetection,
        #[cfg(feature = "agent_decides_command_execution")]
        FeatureFlag::AgentDecidesCommandExecution,
        #[cfg(feature = "codebase_index_speedbump")]
        FeatureFlag::CodebaseIndexSpeedbump,
        #[cfg(feature = "context_line_review_comments")]
        FeatureFlag::ContextLineReviewComments,
        #[cfg(feature = "nld_fasttext_model")]
        FeatureFlag::NLDClassifierModelEnabled,
        #[cfg(feature = "fast_forward_autoexecute_button")]
        FeatureFlag::FastForwardAutoexecuteButton,
        #[cfg(feature = "code_find_replace")]
        FeatureFlag::CodeFindReplace,
        #[cfg(feature = "command_palette_file_search")]
        FeatureFlag::CommandPaletteFileSearch,
        #[cfg(feature = "ai_context_menu_commands")]
        FeatureFlag::AIContextMenuCommands,
        #[cfg(feature = "ai_context_menu_code")]
        FeatureFlag::AIContextMenuCode,
        #[cfg(feature = "expand_edit_to_pane")]
        FeatureFlag::ExpandEditToPane,
        #[cfg(feature = "fallback_model_load_output_messaging")]
        FeatureFlag::FallbackModelLoadOutputMessaging,
        #[cfg(feature = "tab_close_button_on_left")]
        FeatureFlag::TabCloseButtonOnLeft,
        #[cfg(feature = "profiles_design_revamp")]
        FeatureFlag::ProfilesDesignRevamp,
        #[cfg(feature = "search_codebase_ui")]
        FeatureFlag::SearchCodebaseUI,
        #[cfg(feature = "changed_lines_only_apply_diff_result")]
        FeatureFlag::ChangedLinesOnlyApplyDiffResult,
        #[cfg(feature = "linked_code_blocks")]
        FeatureFlag::LinkedCodeBlocks,
        #[cfg(feature = "tabbed_editor_view")]
        FeatureFlag::TabbedEditorView,
        #[cfg(feature = "send_telemetry_to_file")]
        FeatureFlag::SendTelemetryToFile,
        #[cfg(feature = "undo_closed_panes")]
        FeatureFlag::UndoClosedPanes,
        #[cfg(feature = "multi_profile")]
        FeatureFlag::MultiProfile,
        #[cfg(feature = "conversation_artifacts")]
        FeatureFlag::ConversationArtifacts,
        #[cfg(feature = "sync_ambient_plans")]
        FeatureFlag::SyncAmbientPlans,
        #[cfg(feature = "get_started_tab")]
        FeatureFlag::GetStartedTab,
        #[cfg(feature = "welcome_tab")]
        FeatureFlag::WelcomeTab,
        #[cfg(feature = "projects")]
        FeatureFlag::Projects,
        #[cfg(feature = "drive_objects_as_context")]
        FeatureFlag::DriveObjectsAsContext,
        #[cfg(feature = "pr_comments_slash_command")]
        FeatureFlag::PRCommentsSlashCommand,
        #[cfg(feature = "pr_comments_v2")]
        FeatureFlag::PRCommentsV2,
        #[cfg(feature = "pr_comments_skill")]
        FeatureFlag::PRCommentsSkill,
        #[cfg(feature = "selection_as_context")]
        FeatureFlag::SelectionAsContext,
        #[cfg(feature = "code_mode_chip")]
        FeatureFlag::CodeModeChip,
        #[cfg(feature = "github_pr_prompt_chip")]
        FeatureFlag::GithubPrPromptChip,
        #[cfg(feature = "create_project_flow")]
        FeatureFlag::CreateProjectFlow,
        #[cfg(feature = "vim_code_editor")]
        FeatureFlag::VimCodeEditor,
        #[cfg(feature = "allow_opening_file_links_using_editor_env")]
        FeatureFlag::AllowOpeningFileLinksUsingEditorEnv,
        #[cfg(feature = "nld_improvements")]
        FeatureFlag::NldImprovements,
        #[cfg(feature = "revert_diff_hunk")]
        FeatureFlag::RevertDiffHunk,
        #[cfg(feature = "code_review_save_changes")]
        FeatureFlag::CodeReviewSaveChanges,
        #[cfg(feature = "file_tree")]
        FeatureFlag::FileTree,
        #[cfg(feature = "allow_ignoring_input_suggestions")]
        FeatureFlag::AllowIgnoringInputSuggestions,
        #[cfg(feature = "ambient_agents_command_line")]
        FeatureFlag::AmbientAgentsCommandLine,
        #[cfg(feature = "ambient_agents_image_upload")]
        FeatureFlag::AmbientAgentsImageUpload,
        #[cfg(feature = "scheduled_ambient_agents")]
        FeatureFlag::ScheduledAmbientAgents,
        #[cfg(feature = "code_launch_modal")]
        FeatureFlag::CodeLaunchModal,
        #[cfg(feature = "api_key_authentication")]
        FeatureFlag::APIKeyAuthentication,
        #[cfg(feature = "api_key_management")]
        FeatureFlag::APIKeyManagement,
        #[cfg(feature = "mcp_oauth")]
        FeatureFlag::McpOauth,
        #[cfg(feature = "file_based_mcp")]
        FeatureFlag::FileBasedMcp,
        #[cfg(feature = "diff_set_as_context")]
        FeatureFlag::DiffSetAsContext,
        #[cfg(feature = "discard_per_file_and_all_changes")]
        FeatureFlag::DiscardPerFileAndAllChanges,
        #[cfg(feature = "summarization_cancellation_confirmation")]
        FeatureFlag::SummarizationCancellationConfirmation,
        #[cfg(feature = "code_review_find")]
        FeatureFlag::CodeReviewFind,
        #[cfg(feature = "ui_zoom")]
        FeatureFlag::UIZoom,
        #[cfg(feature = "auto_open_code_review_pane")]
        FeatureFlag::AutoOpenCodeReviewPane,
        #[cfg(feature = "inline_code_review")]
        FeatureFlag::InlineCodeReview,
        #[cfg(feature = "create_environment_slash_command")]
        FeatureFlag::CreateEnvironmentSlashCommand,
        #[cfg(feature = "summarize_conversation_command")]
        FeatureFlag::SummarizationConversationCommand,
        #[cfg(feature = "mcp_grouped_server_context")]
        FeatureFlag::MCPGroupedServerContext,
        #[cfg(feature = "web_search_ui")]
        FeatureFlag::WebSearchUI,
        #[cfg(feature = "web_fetch_ui")]
        FeatureFlag::WebFetchUI,
        #[cfg(feature = "fork_from_command")]
        FeatureFlag::ForkFromCommand,
        #[cfg(feature = "context_window_usage_v2")]
        FeatureFlag::ContextWindowUsageV2,
        #[cfg(feature = "global_search")]
        FeatureFlag::GlobalSearch,
        #[cfg(feature = "embedded_code_review_comments")]
        FeatureFlag::EmbeddedCodeReviewComments,
        #[cfg(feature = "file_and_diff_set_comments")]
        FeatureFlag::FileAndDiffSetComments,
        #[cfg(feature = "revert_to_checkpoints")]
        FeatureFlag::RevertToCheckpoints,
        #[cfg(feature = "rewind_slash_command")]
        FeatureFlag::RewindSlashCommand,
        #[cfg(feature = "agent_management_view")]
        FeatureFlag::AgentManagementView,
        #[cfg(feature = "agent_management_details_view")]
        FeatureFlag::AgentManagementDetailsView,
        #[cfg(feature = "agent_view")]
        FeatureFlag::AgentView,
        #[cfg(feature = "agent_view_block_context")]
        FeatureFlag::AgentViewBlockContext,
        #[cfg(feature = "mnemos_managed_secrets")]
        FeatureFlag::MnemosManagedSecrets,
        #[cfg(feature = "v4a_file_diffs")]
        FeatureFlag::V4AFileDiffs,
        #[cfg(feature = "interactive_conversation_management_view")]
        FeatureFlag::InteractiveConversationManagementView,
        #[cfg(feature = "agent_tips")]
        FeatureFlag::AgentTips,
        #[cfg(feature = "team_api_keys")]
        FeatureFlag::TeamApiKeys,
        #[cfg(feature = "cloud_conversations")]
        FeatureFlag::CloudConversations,
        #[cfg(feature = "agent_toolbar_editor")]
        FeatureFlag::AgentToolbarEditor,
        #[cfg(feature = "configurable_toolbar")]
        FeatureFlag::ConfigurableToolbar,
        #[cfg(feature = "agent_view_prompt_chip")]
        FeatureFlag::AgentViewPromptChip,
        #[cfg(feature = "ambient_agents_rtc")]
        FeatureFlag::AmbientAgentsRTC,
        #[cfg(feature = "classic_completions")]
        FeatureFlag::ClassicCompletions,
        #[cfg(feature = "force_classic_completions")]
        FeatureFlag::ForceClassicCompletions,
        #[cfg(feature = "agent_view_conversation_list_view")]
        FeatureFlag::AgentViewConversationListView,
        #[cfg(feature = "inline_history_menu")]
        FeatureFlag::InlineHistoryMenu,
        #[cfg(feature = "inline_repo_menu")]
        FeatureFlag::InlineRepoMenu,
        #[cfg(feature = "cloud_mode")]
        FeatureFlag::CloudMode,
        #[cfg(feature = "cloud_mode_from_local_session")]
        FeatureFlag::CloudModeFromLocalSession,
        #[cfg(feature = "cloud_mode_image_context")]
        FeatureFlag::CloudModeImageContext,
        #[cfg(feature = "summarization_via_message_replacement")]
        FeatureFlag::SummarizationViaMessageReplacement,
        #[cfg(feature = "pluggable_notifications")]
        FeatureFlag::PluggableNotifications,
        #[cfg(feature = "list_skills")]
        FeatureFlag::ListSkills,
        #[cfg(feature = "ask_user_question")]
        FeatureFlag::AskUserQuestion,
        #[cfg(feature = "lsp_as_a_tool")]
        FeatureFlag::LSPAsATool,
        #[cfg(feature = "inline_profile_selector")]
        FeatureFlag::InlineProfileSelector,
        #[cfg(feature = "oz_platform_skills")]
        FeatureFlag::OzPlatformSkills,
        #[cfg(feature = "oz_identity_federation")]
        FeatureFlag::OzIdentityFederation,
        #[cfg(feature = "oz_changelog_updates")]
        FeatureFlag::OzChangelogUpdates,
        #[cfg(feature = "new_tab_styling")]
        FeatureFlag::NewTabStyling,
        #[cfg(feature = "skill_arguments")]
        FeatureFlag::SkillArguments,
        #[cfg(feature = "active_conversation_requires_interaction")]
        FeatureFlag::ActiveConversationRequiresInteraction,
        #[cfg(feature = "conversations_as_context")]
        FeatureFlag::ConversationsAsContext,
        #[cfg(feature = "incremental_auto_reload")]
        FeatureFlag::IncrementalAutoReload,
        #[cfg(feature = "orchestration")]
        FeatureFlag::Orchestration,
        #[cfg(feature = "orchestration_v2")]
        FeatureFlag::OrchestrationV2,
        #[cfg(feature = "orchestration_event_push")]
        FeatureFlag::OrchestrationEventPush,
        #[cfg(feature = "pending_user_query_indicator")]
        FeatureFlag::PendingUserQueryIndicator,
        #[cfg(feature = "queue_slash_command")]
        FeatureFlag::QueueSlashCommand,
        #[cfg(feature = "kitty_keyboard_protocol")]
        FeatureFlag::KittyKeyboardProtocol,
        #[cfg(feature = "inline_menu_headers")]
        FeatureFlag::InlineMenuHeaders,
        #[cfg(feature = "directory_tab_colors")]
        FeatureFlag::DirectoryTabColors,
        #[cfg(feature = "open_mnemos_new_settings_modes")]
        FeatureFlag::OpenMnemosNewSettingsModes,
        #[cfg(feature = "hoa_code_review")]
        FeatureFlag::HoaCodeReview,
        #[cfg(feature = "vertical_tabs")]
        FeatureFlag::VerticalTabs,
        #[cfg(feature = "vertical_tabs_summary_mode")]
        FeatureFlag::VerticalTabsSummaryMode,
        #[cfg(feature = "tab_configs")]
        FeatureFlag::TabConfigs,
        #[cfg(feature = "agent_harness")]
        FeatureFlag::AgentHarness,
        #[cfg(feature = "oz_handoff")]
        FeatureFlag::OzHandoff,
        #[cfg(feature = "hoa_notifications")]
        FeatureFlag::HOANotifications,
        #[cfg(feature = "open_code_notifications")]
        FeatureFlag::OpenCodeNotifications,
        #[cfg(feature = "cli_agent_rich_input")]
        FeatureFlag::CLIAgentRichInput,
        #[cfg(feature = "transfer_control_tool")]
        FeatureFlag::TransferControlTool,
        #[cfg(feature = "mnemosify_footer")]
        FeatureFlag::MnemosifyFooter,
        #[cfg(feature = "solo_user_byok")]
        FeatureFlag::SoloUserByok,
        #[cfg(feature = "hoa_onboarding_flow")]
        FeatureFlag::HOAOnboardingFlow,
        #[cfg(feature = "git_operations_in_code_review")]
        FeatureFlag::GitOperationsInCodeReview,
        #[cfg(feature = "hoa_remote_control")]
        FeatureFlag::HOARemoteControl,
        #[cfg(feature = "codex_notifications")]
        FeatureFlag::CodexNotifications,
        #[cfg(feature = "trim_trailing_blank_lines")]
        FeatureFlag::TrimTrailingBlankLines,
        #[cfg(feature = "cloud_mode_setup_v2")]
        FeatureFlag::CloudModeSetupV2,
        #[cfg(feature = "cloud_mode_input_v2")]
        FeatureFlag::CloudModeInputV2,
    ]);

    flags
}

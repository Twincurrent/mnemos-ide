use super::*;
use crate::ai::blocklist::{BlocklistAIHistoryModel, BlocklistAIPermissions};
use crate::ai::document::ai_document_model::AIDocumentModel;
use crate::ai::execution_profiles::profiles::AIExecutionProfilesModel;
use crate::ai::facts::manager::AIFactManager;
use crate::ai::llms::LLMPreferences;
use crate::ai::outline::RepoOutlines;
use crate::ai::persisted_workspace::PersistedWorkspace;
use crate::ai::restored_conversations::RestoredAgentConversations;
use crate::ai::skills::SkillManager;
use crate::ai::AIRequestUsageModel;
use crate::cloud_object::model::persistence::CloudModel;
use crate::cloud_object::model::view::CloudViewModel;
use crate::context_chips::prompt::Prompt;
use crate::editor::Event;
use crate::gpu_state::GPUState;
use crate::network::NetworkStatus;
use crate::notebooks::editor::keys::NotebookKeybindings;
use crate::notebooks::notebook::NotebookView;
use crate::pane_group::{Direction, PaneGroupAction, PaneId};
use crate::suggestions::ignored_suggestions_model::IgnoredSuggestionsModel;
#[cfg(feature = "local_fs")]
use crate::user_config::tab_configs_dir;
use repo_metadata::repositories::DetectedRepositories;
use repo_metadata::watcher::DirectoryWatcher;
#[cfg(feature = "local_fs")]
use repo_metadata::CanonicalizedPath;
#[cfg(feature = "local_fs")]
use repo_metadata::RepoMetadataModel;
use session_sharing_protocol::sharer::SessionSourceType;
use std::collections::HashMap;
#[cfg(feature = "local_fs")]
use tempfile::TempDir;
use watcher::HomeDirectoryWatcher;

use crate::server::cloud_objects::{listener::Listener, update_manager::UpdateManager};
use crate::server::server_api::ServerApiProvider;
use crate::server::sync_queue::SyncQueue;

use crate::server::telemetry::context_provider::AppTelemetryContextProvider;
use crate::settings::PrivacySettings;
use crate::settings_view::keybindings::KeybindingChangedNotifier;
use crate::settings_view::DisplayCount;
use crate::system::SystemStats;
use crate::tab_configs::tab_config::{TabConfigPaneNode, TabConfigPaneType};
use crate::terminal::history::History;
use crate::terminal::keys::TerminalKeybindings;
#[cfg(windows)]
use crate::util::traffic_lights::windows::RendererState;
use crate::workspaces::team_tester::TeamTesterStatus;
use crate::workspaces::update_manager::TeamUpdateManager;
use crate::workspaces::user_profiles::UserProfiles;
use crate::workspaces::user_workspaces::UserWorkspaces;

use crate::terminal::local_tty::spawner::PtySpawner;
use crate::terminal::shared_session::{SharedSessionScrollbackType, SharedSessionStatus};

use crate::ai::active_agent_views_model::ActiveAgentViewsModel;
use crate::ai::agent_conversations_model::AgentConversationsModel;
use crate::ai::ambient_agents::github_auth_notifier::GitHubAuthNotifier;
use crate::ai::mcp::{
    gallery::MCPGalleryManager, templatable_manager::TemplatableMCPServerManager,
    FileBasedMCPManager, FileMCPWatcher,
};
use crate::resource_center::Tip;
use crate::terminal::cli_agent_sessions::CLIAgentSessionsModel;
use crate::test_util::settings::initialize_settings_for_tests;
use crate::undo_close::UndoCloseSettings;
use crate::mnemos_managed_paths_watcher::MnemosManagedPathsWatcher;
use crate::workflows::local_workflows::LocalWorkflows;
use crate::{workspace, GlobalResourceHandlesProvider};
use crate::{AgentNotificationsModel, ObjectActions};

use crate::settings::cloud_preferences_syncer::CloudPreferencesSyncer;
use ai::index::full_source_code_embedding::manager::CodebaseIndexManager;
use ai::project_context::model::ProjectContextModel;
use pane_group::{NotebookPane, PaneState, SplitPaneState, TerminalPaneId};
use session_sharing_protocol::common::SessionId;
use terminal::shared_session::permissions_manager::SessionPermissionsManager;
use terminal::view::ActiveSessionState;
use mnemos_editor::editor::NavigationKey;
use mnemos_ui::AddSingletonModel;
use mnemos_ui::{platform::WindowStyle, App, ViewHandle};

/// Scratch directory every mnemos-ide-* store path is redirected to for this test
/// process. Kept alive for the process lifetime so the `TempDir` isn't
/// deleted underneath later tests.
static IDE_TEST_DATA_DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();

/// Point `mnemos_ide_core::paths` at an empty per-process directory. Without this,
/// `Workspace::new` seeds one tab per project from the developer's real
/// `%APPDATA%\Mnemos\store\projects.json` and every tab-count assertion in
/// this file drifts with whatever projects happen to be open on the machine.
/// First call wins; tests in the same process share the (empty) directory and
/// nothing here writes to it.
fn isolate_ide_data_dir() {
    let dir = IDE_TEST_DATA_DIR.get_or_init(|| {
        tempfile::Builder::new()
            .prefix("mnemos-test-ide-")
            .tempdir()
            .expect("create Mnemos IDE test data dir")
    });
    if let Err(existing) = mnemos_ide_core::paths::override_data_dir(dir.path().to_path_buf()) {
        panic!(
            "Mnemos IDE data dir already overridden to {existing:?}; expected {:?}",
            dir.path()
        );
    }
}

fn initialize_app(app: &mut App) {
    isolate_ide_data_dir();
    initialize_settings_for_tests(app);

    // Add the necessary singleton models to the App
    app.add_singleton_model(|_ctx| ServerApiProvider::new_for_test());
    app.add_singleton_model(|_| AuthStateProvider::new_for_test());
    app.add_singleton_model(AppTelemetryContextProvider::new_context_provider);
    app.add_singleton_model(AuthManager::new_for_test);
    app.add_singleton_model(|_ctx| PtySpawner::new_for_test());
    app.add_singleton_model(|_| Prompt::mock());
    app.add_singleton_model(|ctx| AutoupdateState::new(ServerApiProvider::as_ref(ctx).get()));
    app.add_singleton_model(|_| NetworkStatus::new());
    app.add_singleton_model(|_| SystemStats::new());
    app.add_singleton_model(SyncQueue::mock);
    app.add_singleton_model(CloudModel::mock);
    app.add_singleton_model(UserWorkspaces::default_mock);
    app.add_singleton_model(|_ctx| UserProfiles::new(Vec::new()));
    app.add_singleton_model(TeamTesterStatus::mock);
    app.add_singleton_model(TeamUpdateManager::mock);
    app.add_singleton_model(UpdateManager::mock);
    app.add_singleton_model(MCPGalleryManager::new);
    app.add_singleton_model(CloudViewModel::mock);
    app.add_singleton_model(Listener::mock);
    app.add_singleton_model(|_| Appearance::mock());
    // Read by the agents-panel refresh that pane-count changes trigger.
    app.add_singleton_model(
        crate::terminal::cli_agent_sessions::file_ingest::CLIAgentEventFileIngestor::new_for_test,
    );
    // Owns code panes; `CodePane::new` looks it up.
    app.add_singleton_model(|_| crate::code::editor_management::CodeManager::default());
    app.add_singleton_model(AppearanceManager::new);
    app.add_singleton_model(|_| DisplayCount::mock());
    app.add_singleton_model(PrivacySettings::mock);
    app.add_singleton_model(|_| KeybindingChangedNotifier::new());
    app.add_singleton_model(|_ctx| RelaunchModel::new());
    app.add_singleton_model(|ctx| ChangelogModel::new(ServerApiProvider::as_ref(ctx).get()));
    app.add_singleton_model(|_| GitHubAuthNotifier::new());
    app.add_singleton_model(|_ctx| SyncedInputState::mock());
    app.add_singleton_model(|_| ResizableData::default());
    app.add_singleton_model(LocalWorkflows::new);
    app.add_singleton_model(UndoCloseStack::new);
    app.add_singleton_model(terminal::shared_session::manager::Manager::new);
    app.add_singleton_model(|_| ActiveSession::default());
    app.add_singleton_model(|_| WorkspaceToastStack);
    app.add_singleton_model(|_| ObjectActions::new(Vec::new()));
    app.add_singleton_model(NotebookKeybindings::new);
    app.add_singleton_model(TerminalKeybindings::new);
    app.add_singleton_model(NotebookManager::mock);
    app.add_singleton_model(|ctx| {
        CloudPreferencesSyncer::new(
            false,                     // force_local_wins_on_startup
            std::path::PathBuf::new(), // unused in tests that don't exercise the hash path
            ctx,
        )
    });
    app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
    app.add_singleton_model(|_| CLIAgentSessionsModel::new());
    app.add_singleton_model(|_| ActiveAgentViewsModel::new());
    app.add_singleton_model(AgentNotificationsModel::new);
    app.add_singleton_model(AgentConversationsModel::new);
    app.add_singleton_model(SessionPermissionsManager::new);
    app.add_singleton_model(LLMPreferences::new);
    app.add_singleton_model(|_| SettingsPaneManager::new());
    app.add_singleton_model(|_| AIFactManager::new());

    // Initialize file-based MCP dependencies.
    app.add_singleton_model(|_| DetectedRepositories::default());
    app.add_singleton_model(HomeDirectoryWatcher::new_for_test);
    app.add_singleton_model(DirectoryWatcher::new);
    app.add_singleton_model(MnemosManagedPathsWatcher::new_for_testing);
    app.add_singleton_model(FileMCPWatcher::new);
    app.add_singleton_model(|_| FileBasedMCPManager::default());

    app.add_singleton_model(|_| TemplatableMCPServerManager::default());
    app.add_singleton_model(|ctx| {
        AIExecutionProfilesModel::new(&crate::LaunchMode::new_for_unit_test(), ctx)
    });
    app.add_singleton_model(RepoOutlines::new_for_test);
    app.add_singleton_model(BlocklistAIPermissions::new);
    app.add_singleton_model(|_| GPUState::new());
    app.add_singleton_model(|_| RestoredAgentConversations::new(vec![]));
    app.add_singleton_model(OneTimeModalModel::new);
    let global_resource_handles = GlobalResourceHandles::mock(app);
    app.add_singleton_model(|_| GlobalResourceHandlesProvider::new(global_resource_handles));
    app.add_singleton_model(DefaultTerminal::new);
    app.add_singleton_model(|_| IgnoredSuggestionsModel::new(vec![]));
    app.add_singleton_model(|_| crate::code_review::git_status_update::GitStatusUpdateModel::new());
    app.add_singleton_model(remote_server::manager::RemoteServerManager::new);

    #[cfg(feature = "local_fs")]
    app.add_singleton_model(RepoMetadataModel::new);
    app.add_singleton_model(search::files::model::FileSearchModel::new);

    #[cfg(windows)]
    {
        app.add_singleton_model(RendererState::new);
    }

    #[cfg(feature = "local_tty")]
    terminal::available_shells::register(app);
    AltScreenReporting::register(app);

    #[cfg(enable_crash_recovery)]
    crate::crash_recovery::CrashRecovery::register_for_test(app);

    app.add_singleton_model(|ctx| {
        AIRequestUsageModel::new_for_test(ServerApiProvider::as_ref(ctx).get_ai_client(), ctx)
    });
    app.add_singleton_model(
        crate::workspace::bonus_grant_notification_model::BonusGrantNotificationModel::new,
    );
    app.add_singleton_model(|ctx| {
        CodebaseIndexManager::new_for_test(ServerApiProvider::as_ref(ctx).get(), ctx)
    });
    app.add_singleton_model(|ctx| PersistedWorkspace::new(vec![], HashMap::new(), None, ctx));
    app.add_singleton_model(|_| ProjectContextModel::default());
    app.add_singleton_model(AIDocumentModel::new);
    app.add_singleton_model(|_| History::new(vec![]));

    // SkillManager must be registered because the command palette materializes
    // binding descriptions eagerly, and `workspace:send_feedback`'s dynamic
    // label calls `is_feedback_skill_available`, which reads `SkillManager`.
    // Registered after `HomeDirectoryWatcher`, `DirectoryWatcher`,
    // `MnemosManagedPathsWatcher`, `DetectedRepositories`, and `RepoMetadataModel`
    // because `SkillWatcher::new` subscribes to all of them.
    app.add_singleton_model(SkillManager::new);

    // Make sure to initialize the keybindings so that they are available for subviews
    app.update(workspace::init);
}

fn mock_workspace(app: &mut App) -> ViewHandle<Workspace> {
    let global_resource_handles = GlobalResourceHandles::mock(app);
    let active_window_id = app.read(|ctx| ctx.windows().active_window());
    let (_, workspace) = app.add_window(WindowStyle::NotStealFocus, |ctx| {
        Workspace::new(
            global_resource_handles,
            None,
            NewWorkspaceSource::Empty {
                previous_active_window: active_window_id,
                shell: None,
            },
            ctx,
        )
    });
    workspace
}

fn restored_workspace(
    app: &mut App,
    window_snapshot: crate::app_state::WindowSnapshot,
) -> ViewHandle<Workspace> {
    let global_resource_handles = GlobalResourceHandles::mock(app);
    let (_, workspace) = app.add_window(WindowStyle::NotStealFocus, |ctx| {
        Workspace::new(
            global_resource_handles,
            None,
            NewWorkspaceSource::Restored {
                window_snapshot,
                block_lists: Arc::new(HashMap::new()),
            },
            ctx,
        )
    });
    workspace
}

fn transferred_tab_workspace(
    app: &mut App,
    vertical_tabs_panel_open: bool,
) -> ViewHandle<Workspace> {
    let global_resource_handles = GlobalResourceHandles::mock(app);
    let (_, workspace) = app.add_window(WindowStyle::NotStealFocus, |ctx| {
        Workspace::new(
            global_resource_handles,
            None,
            NewWorkspaceSource::TransferredTab {
                tab_color: None,
                custom_title: None,
                left_panel_open: false,
                vertical_tabs_panel_open,
                right_panel_open: false,
                is_right_panel_maximized: false,
                for_drag_preview: false,
            },
            ctx,
        )
    });
    workspace
}

#[cfg(feature = "local_fs")]
fn open_worktree_sidecar(workspace: &ViewHandle<Workspace>, app: &mut App) {
    workspace.update(app, |workspace, ctx| {
        workspace.open_new_session_dropdown_menu(Vector2F::zero(), ctx);

        let worktree_index = workspace
            .new_session_dropdown_menu
            .read(ctx, |menu, _| {
                menu.items().iter().position(|item| {
                    matches!(
                        item,
                        MenuItem::Item(fields) if fields.label() == "New worktree config"
                    )
                })
            })
            .expect("expected new worktree config item in new-session menu");

        workspace
            .new_session_dropdown_menu
            .update(ctx, |menu, view_ctx| {
                menu.set_selected_by_index(worktree_index, view_ctx);
            });
    });
}

#[cfg(feature = "local_fs")]
#[test]
fn test_worktree_sidecar_hover_takes_precedence_over_selection() {
    let _tab_configs_guard = FeatureFlag::TabConfigs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        let temp_root = TempDir::new().expect("failed to create temp dir");
        let alpha_repo = temp_root.path().join("alpha-repo");
        let beta_repo = temp_root.path().join("beta-repo");
        std::fs::create_dir_all(&alpha_repo).expect("failed to create alpha repo dir");
        std::fs::create_dir_all(&beta_repo).expect("failed to create beta repo dir");

        workspace.update(&mut app, |_, ctx| {
            PersistedWorkspace::handle(ctx).update(ctx, |persisted, ctx| {
                persisted.user_added_workspace(alpha_repo.clone(), ctx);
                persisted.user_added_workspace(beta_repo.clone(), ctx);
            });
        });

        open_worktree_sidecar(&workspace, &mut app);

        workspace.update(&mut app, |workspace, ctx| {
            workspace
                .new_session_sidecar_menu
                .update(ctx, |menu, view_ctx| {
                    menu.set_selected_by_index(1, view_ctx);
                    menu.handle_action(
                        &crate::menu::MenuAction::HoverSubmenuLeafNode {
                            depth: 0,
                            row_index: 2,
                            position: Vector2F::zero(),
                        },
                        view_ctx,
                    );
                });

            workspace.handle_new_session_sidecar_event(&MenuEvent::ItemHovered, ctx);
        });

        workspace.read(&app, |workspace, ctx| {
            assert_eq!(
                workspace
                    .new_session_sidecar_menu
                    .read(ctx, |menu, _| menu.selected_index()),
                Some(2)
            );
        });
    });
}

#[cfg(feature = "local_fs")]
#[test]
fn test_worktree_sidecar_pointer_entry_does_not_select_top_repo() {
    let _tab_configs_guard = FeatureFlag::TabConfigs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        let temp_root = TempDir::new().expect("failed to create temp dir");
        let alpha_repo = temp_root.path().join("alpha-repo");
        let beta_repo = temp_root.path().join("beta-repo");
        std::fs::create_dir_all(&alpha_repo).expect("failed to create alpha repo dir");
        std::fs::create_dir_all(&beta_repo).expect("failed to create beta repo dir");

        workspace.update(&mut app, |_, ctx| {
            PersistedWorkspace::handle(ctx).update(ctx, |persisted, ctx| {
                persisted.user_added_workspace(alpha_repo.clone(), ctx);
                persisted.user_added_workspace(beta_repo.clone(), ctx);
            });
        });

        workspace.update(&mut app, |workspace, ctx| {
            workspace.open_new_session_dropdown_menu(Vector2F::zero(), ctx);

            let worktree_index = workspace
                .new_session_dropdown_menu
                .read(ctx, |menu, _| {
                    menu.items().iter().position(|item| {
                        matches!(
                            item,
                            MenuItem::Item(fields) if fields.label() == "New worktree config"
                        )
                    })
                })
                .expect("expected new worktree config item in new-session menu");

            workspace
                .new_session_dropdown_menu
                .update(ctx, |menu, view_ctx| {
                    menu.handle_action(
                        &crate::menu::MenuAction::HoverSubmenuWithChildren(
                            0,
                            crate::menu::SelectAction::Index {
                                row: worktree_index,
                                item: 0,
                            },
                        ),
                        view_ctx,
                    );
                });
            workspace.update_new_session_sidecar(ctx);
        });

        workspace.read(&app, |workspace, ctx| {
            assert!(workspace.show_new_session_sidecar);
            assert_eq!(
                workspace
                    .new_session_sidecar_menu
                    .read(ctx, |menu, _| menu.selected_index()),
                None
            );
        });
    });
}

#[cfg(feature = "local_fs")]
#[test]
fn test_worktree_sidecar_close_via_select_item_executes_from_workspace() {
    let _tab_configs_guard = FeatureFlag::TabConfigs.override_enabled(true);

    App::test((), |mut app| async move {
        let _cleanup = TabConfigCleanupGuard::new("alpha-repo");

        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        let temp_root = TempDir::new().expect("failed to create temp dir");
        let alpha_repo = temp_root.path().join("alpha-repo");
        std::fs::create_dir_all(&alpha_repo).expect("failed to create alpha repo dir");

        workspace.update(&mut app, |_, ctx| {
            PersistedWorkspace::handle(ctx).update(ctx, |persisted, ctx| {
                persisted.user_added_workspace(alpha_repo.clone(), ctx);
            });
        });

        open_worktree_sidecar(&workspace, &mut app);

        workspace.update(&mut app, |workspace, ctx| {
            workspace
                .new_session_sidecar_menu
                .update(ctx, |menu, view_ctx| {
                    menu.set_selected_by_index(1, view_ctx);
                });
            workspace.handle_new_session_sidecar_event(
                &MenuEvent::Close {
                    via_select_item: true,
                },
                ctx,
            );
            workspace.handle_new_session_sidecar_event(&MenuEvent::ItemSelected, ctx);
        });

        workspace.read(&app, |workspace, _| {
            assert_eq!(workspace.tab_count(), 2);
        });
    });
}

#[cfg(feature = "local_fs")]
#[test]
fn test_worktree_sidecar_search_editor_enter_executes_selection() {
    let _tab_configs_guard = FeatureFlag::TabConfigs.override_enabled(true);

    App::test((), |mut app| async move {
        let _cleanup = TabConfigCleanupGuard::new("alpha-repo");

        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        let temp_root = TempDir::new().expect("failed to create temp dir");
        let alpha_repo = temp_root.path().join("alpha-repo");
        std::fs::create_dir_all(&alpha_repo).expect("failed to create alpha repo dir");

        workspace.update(&mut app, |_, ctx| {
            PersistedWorkspace::handle(ctx).update(ctx, |persisted, ctx| {
                persisted.user_added_workspace(alpha_repo.clone(), ctx);
            });
        });

        open_worktree_sidecar(&workspace, &mut app);

        workspace.update(&mut app, |workspace, ctx| {
            workspace
                .worktree_sidecar_search_editor
                .update(ctx, |_, ctx| {
                    ctx.emit(Event::Enter);
                });
        });

        workspace.read(&app, |workspace, _| {
            assert_eq!(workspace.tab_count(), 2);
            assert!(workspace.show_new_session_dropdown_menu.is_none());
        });
    });
}

/// RAII guard that removes tab config TOML files whose name starts with
/// `prefix` from `~/.mnemos/tab_configs/` on drop. Because `Drop` runs even
/// when a test panics, this prevents stale worktree configs from leaking
/// into Mnemos dev.
#[cfg(feature = "local_fs")]
struct TabConfigCleanupGuard {
    prefix: &'static str,
}

#[cfg(feature = "local_fs")]
impl TabConfigCleanupGuard {
    fn new(prefix: &'static str) -> Self {
        // Eagerly clean up leftovers from any previously-crashed run.
        Self::clean(prefix);
        Self { prefix }
    }

    fn clean(prefix: &str) {
        let dir = tab_configs_dir();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with(prefix) && name.ends_with(".toml") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
}

#[cfg(feature = "local_fs")]
impl Drop for TabConfigCleanupGuard {
    fn drop(&mut self) {
        Self::clean(self.prefix);
    }
}

/// Creates a workspace with a single, shared session.
fn mock_workspace_with_shared_session(app: &mut App) -> ViewHandle<Workspace> {
    use crate::terminal::shared_session::manager::Manager;

    // Create the workspace as a session-sharing sharer.
    let global_resource_handles = GlobalResourceHandles::mock(app);
    let (_, workspace) = app.add_window(WindowStyle::NotStealFocus, |ctx| {
        Workspace::new(
            global_resource_handles,
            None,
            NewWorkspaceSource::Empty {
                previous_active_window: None,
                shell: None,
            },
            ctx,
        )
    });

    // Get the single terminal view in the workspace.
    let terminal_view = workspace.read(app, |workspace, ctx| {
        assert_eq!(workspace.tabs.len(), 1);
        workspace
            .active_tab_pane_group()
            .as_ref(ctx)
            .focused_session_view(ctx)
            .unwrap()
    });

    terminal_view.update(app, |view, ctx| {
        view.model.lock().block_list_mut().set_bootstrapped();
        view.attempt_to_share_session(
            SharedSessionScrollbackType::All,
            None,
            SessionSourceType::default(),
            false,
            ctx,
        );
    });

    // Make sure the view is registered with the shared session manager.
    app.read(|ctx| {
        let manager = Manager::as_ref(ctx);
        let shared_sessions = manager.shared_views(ctx).collect_vec();
        assert_eq!(shared_sessions.len(), 1);
        assert_eq!(shared_sessions[0].id(), terminal_view.id());
    });

    workspace
}

// Creates a workspace as a viewer of a shared session.
fn mock_workspace_viewing_shared_session(app: &mut App) -> ViewHandle<Workspace> {
    // Create the workspace as a session-sharing sharer.
    let global_resource_handles = GlobalResourceHandles::mock(app);

    let session_id = SessionId::new();

    let (_, workspace) = app.add_window(WindowStyle::NotStealFocus, |ctx| {
        Workspace::new(
            global_resource_handles,
            None,
            NewWorkspaceSource::SharedSessionAsViewer { session_id },
            ctx,
        )
    });

    // Get the single terminal view in the workspace.
    let terminal_view = workspace.read(app, |workspace, ctx| {
        assert_eq!(workspace.tabs.len(), 1);
        workspace
            .active_tab_pane_group()
            .as_ref(ctx)
            .focused_session_view(ctx)
            .unwrap()
    });

    // Ensure session is opened as a viewer.
    terminal_view.read(app, |terminal, _ctx| {
        let model = terminal.model.clone();
        assert!(model.lock().shared_session_status().is_viewer());
    });

    workspace
}

/// Disable the warn-before-quit setting. Because we don't fully bootstrap the shell in tests, this
/// is generally needed in tests that close tabs.
fn disable_quit_warning(app: &mut AppContext) {
    GeneralSettings::handle(app).update(app, |settings, ctx| {
        settings
            .show_warning_before_quitting
            .set_value(false, ctx)
            .expect("Failed to disable quit warning");
    });
}

fn get_newly_created_pane_id(panes: &PaneGroup, existing_ids: &[PaneId]) -> PaneId {
    panes
        .pane_ids()
        .find(|id| !existing_ids.contains(id))
        .unwrap()
}

fn split_pane_state(
    panes: &PaneGroup,
    pane_id: impl Into<PaneId>,
    ctx: &AppContext,
) -> SplitPaneState {
    // Split pane state is now inferred from the pane group's focus state
    panes
        .focus_state_handle()
        .as_ref(ctx)
        .split_pane_state_for(pane_id.into())
}

fn active_session_state(
    panes: &PaneGroup,
    pane_id: TerminalPaneId,
    ctx: &AppContext,
) -> ActiveSessionState {
    if panes
        .terminal_view_from_pane_id(pane_id, ctx)
        .expect("Not a terminal pane")
        .as_ref(ctx)
        .is_active_session(ctx)
    {
        ActiveSessionState::Active
    } else {
        ActiveSessionState::Inactive
    }
}

fn new_session_menu_label(item: &MenuItem<WorkspaceAction>) -> String {
    match item {
        MenuItem::Item(fields) => fields.label().to_string(),
        MenuItem::Separator => "---".to_string(),
        MenuItem::ItemsRow { items } => items
            .iter()
            .map(|fields| fields.label().to_string())
            .collect::<Vec<_>>()
            .join(" | "),
        MenuItem::Submenu { fields, .. } => fields.label().to_string(),
        MenuItem::Header { fields, .. } => fields.label().to_string(),
    }
}

/// Mnemos IDE: the header chip follows mnemos-ide-sync's admin login. Shown while signed
/// out (the test app never starts sync), gone once a run reports signed in,
/// back as "login not saved" when the keyring refused the login.
#[test]
fn test_ide_sync_auth_chip_follows_the_sync_login() {
    App::test((), |mut app| async move {
        // Also points the Mnemos IDE data dir at a temp dir, which the manager below
        // resolves its paths under.
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);
        let chip_shown = |app: &App| {
            workspace.read(app, |workspace, ctx| {
                workspace
                    .render_ide_sync_auth_chip(Appearance::as_ref(ctx), ctx)
                    .is_some()
            })
        };
        // Mnemos IDE: nobody ever signed in here (NeverConfigured), which is
        // not a warning: a first-time or offline user sees no chip.
        assert!(!chip_shown(&app), "never signed in: no chip");

        // Built but never started: no watcher, no network, no keyring.
        let manager = Arc::new(mnemos_ide_sync::SyncManager::from_auth(
            mnemos_ide_sync::AdminAuth::from_tokens("https://admin.example.test", mnemos_ide_sync::auth::AdminTokens {
                access_token: "AT".to_owned(),
                refresh_token: "RT".to_owned(),
                expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            }),
        ));
        AuthManager::handle(&app).update(&mut app, |auth_manager, _ctx| {
            auth_manager.set_ide_sync_manager_for_test(manager.clone());
        });
        assert!(!chip_shown(&app), "signed in: no chip");

        manager.note_token_not_saved("Access is denied.".to_owned());
        assert!(chip_shown(&app), "login not saved");
    });
}

#[cfg(feature = "local_fs")]
#[test]
fn test_opening_a_file_in_new_tab_layout_stays_inside_the_project() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            let tabs_before = workspace.tab_count();
            let active_tab_before = workspace.active_tab_index;
            let code_panes_before = workspace
                .active_tab_pane_group()
                .as_ref(ctx)
                .code_panes(ctx)
                .count();

            // `code.editor.open_file_layout` defaults to NewTab. In Mnemos IDE every
            // workspace tab is a project, so "new tab" has to mean a tab inside
            // the active project's pane group; an extra workspace tab here is the
            // "opening CLAUDE.md created a project called CLAUDE.md" bug.
            workspace.add_tab_for_new_code_file(ctx);

            assert_eq!(
                workspace.tab_count(),
                tabs_before,
                "opening a file must not create a workspace (= project) tab"
            );
            assert_eq!(
                workspace.active_tab_index, active_tab_before,
                "the user must stay on the project they opened the file from"
            );
            assert_eq!(
                workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .code_panes(ctx)
                    .count(),
                code_panes_before + 1,
                "the code pane should live in the active project's pane group"
            );
        });
    });
}

#[test]
fn test_tab_renaming_editor_selections() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        // Add second tab and rename both of them to prepare for the test
        workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);
            workspace.rename_tab_internal(0, "short_title", ctx);
            let selected_text = workspace
                .tab_rename_editor
                .read(ctx, |editor, ctx| editor.selected_text(ctx));
            assert_eq!("short_title", selected_text);

            // Ensure that whatever is selected, is the full title and not the leftover from
            // the previous, shorter one.
            workspace.rename_tab_internal(1, "very_long_title_this_is", ctx);
            let selected_text = workspace
                .tab_rename_editor
                .read(ctx, |editor, ctx| editor.selected_text(ctx));
            assert_eq!("very_long_title_this_is", selected_text);

            // Ensure that if we escape, the current editor's contents is going to be cleared
            // as well.
            workspace.handle_tab_rename_editor_event(&Event::Escape, ctx);
            let selected_text = workspace
                .tab_rename_editor
                .read(ctx, |editor, ctx| editor.selected_text(ctx));
            assert_eq!("", selected_text);
        });
    });
}

#[test]
fn test_tab_renaming_editor_reset() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);
            workspace.rename_tab_internal(0, "short_title", ctx);
            workspace.rename_tab_internal(1, "very_long_title_this_is", ctx);

            // Ensure that when the editor is initially not empty, it will be cleared before a user renames a tab
            workspace.tab_rename_editor.update(ctx, |editor, ctx| {
                editor.insert_selected_text("some-text", ctx);
            });
            workspace.rename_tab_internal(1, "new_very_long_title", ctx);
            let selected_text: String = workspace
                .tab_rename_editor
                .read(ctx, |editor, ctx| editor.selected_text(ctx));
            assert_eq!("new_very_long_title", selected_text);
        });
    });
}

#[test]
fn test_set_active_tab_name() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);

            workspace.handle_action(
                &WorkspaceAction::SetActiveTabName("  Backend API  ".to_string()),
                ctx,
            );
            assert_eq!(
                workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .display_title(ctx),
                "Backend API"
            );
            assert_eq!(
                workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .custom_title(ctx)
                    .as_deref(),
                Some("Backend API")
            );

            workspace.handle_action(&WorkspaceAction::ActivateTab(0), ctx);
            assert_ne!(
                workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .custom_title(ctx)
                    .as_deref(),
                Some("Backend API")
            );

            workspace.handle_action(&WorkspaceAction::ActivateTab(1), ctx);
            workspace.handle_action(&WorkspaceAction::SetActiveTabName("   ".to_string()), ctx);
            assert_eq!(
                workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .custom_title(ctx)
                    .as_deref(),
                Some("Backend API")
            );
        });
    });
}

#[test]
fn test_set_active_tab_name_clears_active_rename_editor_state() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            workspace.rename_tab_internal(0, "old title", ctx);
            assert!(workspace.current_workspace_state.is_tab_being_renamed());

            workspace.handle_action(
                &WorkspaceAction::SetActiveTabName("new title".to_string()),
                ctx,
            );

            assert!(!workspace.current_workspace_state.is_tab_being_renamed());
            assert_eq!(
                workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .display_title(ctx),
                "new title"
            );
        });
    });
}

#[test]
fn test_workspace_sessions_retrieves_tabs() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            let pane_id = workspace
                .get_pane_group_view(0)
                .map(|tab| tab.read(ctx, |tab, _ctx| tab.pane_id_by_index(0).unwrap()))
                .expect("WindowId was not retrieved.");

            assert!(workspace
                .workspace_sessions(ctx.window_id(), ctx)
                .any(|x| { x.pane_view_locator().pane_id == pane_id }));

            // Add a tab and check if workspace_sessions finds the second session from the new tab.
            workspace.add_terminal_tab(false, ctx);
            let new_pane_id = workspace
                .get_pane_group_view(1)
                .map(|tab| tab.read(ctx, |tab, _ctx| tab.pane_id_by_index(0).unwrap()))
                .expect("WindowId was not retrieved.");

            assert!(workspace
                .workspace_sessions(ctx.window_id(), ctx)
                .any(|x| { x.pane_view_locator().pane_id == new_pane_id }));
        });
    });
}

#[test]
fn test_workspace_sessions_retrieves_panes() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            // Add a new split pane to the right.
            if let Some(tab_view) = workspace.get_pane_group_view(0) {
                tab_view.update(ctx, |view, ctx| {
                    view.handle_action(&PaneGroupAction::Add(Direction::Right), ctx);
                })
            }

            // Get the EntityId of the new pane added to the current tab.
            let new_pane_id = workspace
                .get_pane_group_view(0)
                .map(|tab| tab.read(ctx, |tab, _ctx| tab.pane_id_by_index(1).unwrap()))
                .expect("WindowId was not retrieved.");
            assert!(workspace
                .workspace_sessions(ctx.window_id(), ctx)
                .any(|x| { x.pane_view_locator().pane_id == new_pane_id }));
        });
    });
}

fn number_of_shared_sessions_in_tab(
    workspace: &Workspace,
    index: usize,
    ctx: &AppContext,
) -> usize {
    workspace
        .get_pane_group_view(index)
        .map_or(0, |view| view.as_ref(ctx).number_of_shared_sessions(ctx))
}

/// Sets up the workspace with three tabs. The middle tab has two panes, where one is shared.
fn setup_session_sharing_test(workspace: &ViewHandle<Workspace>, app: &mut App) -> PaneId {
    let shared_pane_id = workspace.update(app, |workspace, ctx| {
        workspace.add_terminal_tab(false, ctx);
        workspace.add_terminal_tab(false, ctx);

        let tab_view = workspace.get_pane_group_view(1).unwrap();

        tab_view.update(ctx, |view, ctx| {
            assert_eq!(view.pane_count(), 1);
            view.focused_session_view(ctx)
                .unwrap()
                .update(ctx, |terminal, ctx| {
                    terminal.attempt_to_share_session(
                        SharedSessionScrollbackType::None,
                        None,
                        SessionSourceType::default(),
                        false,
                        ctx,
                    );
                });

            view.handle_action(&PaneGroupAction::Add(Direction::Right), ctx);
            assert_eq!(view.pane_count(), 2);

            view.pane_id_by_index(0).unwrap()
        })
    });

    workspace.read(app, |workspace, ctx| {
        assert_eq!(number_of_shared_sessions_in_tab(workspace, 1, ctx), 1);

        // Confirmation dialog starts not open.
        assert!(
            !workspace
                .current_workspace_state
                .is_close_session_confirmation_dialog_open
        );
    });

    shared_pane_id
}

#[test]
fn test_close_tab_confirmation_dialog() {
    let _guard = FeatureFlag::CreatingSharedSessions.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        app.update(disable_quit_warning);

        let workspace = mock_workspace(&mut app);
        setup_session_sharing_test(&workspace, &mut app);

        workspace.update(&mut app, |workspace, ctx| {
            let first_tab_id = workspace.get_pane_group_view(0).unwrap().id();

            // Trying to close tab with a shared pane opens dialog.
            workspace.handle_action(&WorkspaceAction::CloseTab(1), ctx);
            assert!(
                workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );

            // User clicking cancel closes dialog.
            workspace.handle_close_session_confirmation_dialog_event(
                &CloseSessionConfirmationEvent::Cancel,
                ctx,
            );
            assert!(
                !workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );

            // Trying to close tab without a shared pane goes through without dialog.
            workspace.handle_action(&WorkspaceAction::CloseTab(2), ctx);
            assert_eq!(workspace.tab_count(), 2);
            assert!(
                !workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );

            // Close the tab with the shared pane.
            workspace.handle_action(&WorkspaceAction::CloseTab(1), ctx);
            assert!(
                workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );
            workspace.handle_close_session_confirmation_dialog_event(
                &CloseSessionConfirmationEvent::CloseSession {
                    dont_show_again: false,
                    open_confirmation_source: OpenDialogSource::CloseTab { tab_index: 1 },
                },
                ctx,
            );
            assert!(
                !workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );
            assert_eq!(workspace.tab_count(), 1);
            assert_eq!(workspace.get_pane_group_view(0).unwrap().id(), first_tab_id);
        });
    });
}

#[test]
fn test_close_pane_confirmation_dialog() {
    let _guard = FeatureFlag::CreatingSharedSessions.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        let shared_pane_id = setup_session_sharing_test(&workspace, &mut app);

        workspace.update(&mut app, |workspace, ctx| {
            let shared_pane_group_id = workspace.get_pane_group_view(1).unwrap().id();

            // User tries to close shared pane, dialog comes up.
            workspace.handle_file_tree_event(
                workspace.get_pane_group_view(1).unwrap().clone(),
                &pane_group::Event::CloseSharedSessionPaneRequested {
                    pane_id: shared_pane_id,
                },
                ctx,
            );
            assert!(
                workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );

            // User confirms.
            workspace.handle_close_session_confirmation_dialog_event(
                &CloseSessionConfirmationEvent::CloseSession {
                    dont_show_again: false,
                    open_confirmation_source: OpenDialogSource::ClosePane {
                        pane_group_id: shared_pane_group_id,
                        pane_id: shared_pane_id,
                    },
                },
                ctx,
            );
            assert!(
                !workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );
            assert_eq!(number_of_shared_sessions_in_tab(workspace, 1, ctx), 0);
            let remaining_pane_id = workspace
                .get_pane_group_view_with_id(shared_pane_group_id)
                .unwrap()
                .as_ref(ctx)
                .pane_id_by_index(0)
                .unwrap();
            assert_ne!(remaining_pane_id, shared_pane_id);
            assert_eq!(workspace.tab_count(), 3);
        });
    });
}

#[test]
fn test_reopen_closed_shared_tab() {
    let _guard = FeatureFlag::CreatingSharedSessions.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        setup_session_sharing_test(&workspace, &mut app);

        workspace.update(&mut app, |workspace, ctx| {
            let shared_pane_group = workspace.get_pane_group_view(1).unwrap().clone();

            // Close the tab with the shared pane.
            workspace.close_tab(1, true, true, ctx);
            assert_eq!(workspace.tab_count(), 2);

            // Restore the shared tab.
            workspace.restore_closed_tab(1, TabData::new(shared_pane_group.to_owned()), ctx);
        });
        // Restored tab should no longer be shared.
        workspace.read(&app, |workspace, ctx| {
            let pane_group = workspace.get_pane_group_view(1).unwrap();
            assert!(!pane_group.as_ref(ctx).is_terminal_pane_being_shared(ctx));
            assert_eq!(workspace.tab_count(), 3);
        })
    });
}

#[test]
fn test_close_other_tabs_confirmation_dialog() {
    let _guard = FeatureFlag::CreatingSharedSessions.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        setup_session_sharing_test(&workspace, &mut app);

        workspace.update(&mut app, |workspace, ctx| {
            let last_tab_id = workspace.get_pane_group_view(2).unwrap().id();

            // User tries to close other tabs choosing non-shared tab, dialog comes up.
            workspace.handle_action(&WorkspaceAction::CloseOtherTabs(2), ctx);
            assert!(
                workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );

            // User confirms.
            workspace.handle_close_session_confirmation_dialog_event(
                &CloseSessionConfirmationEvent::CloseSession {
                    dont_show_again: false,
                    open_confirmation_source: OpenDialogSource::CloseOtherTabs { tab_index: 2 },
                },
                ctx,
            );
            assert!(
                !workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );
            assert_eq!(workspace.tab_count(), 1);
            assert_eq!(workspace.get_pane_group_view(0).unwrap().id(), last_tab_id);
        });
    });
}

#[test]
fn test_close_tabs_right_confirmation_dialog() {
    let _guard = FeatureFlag::CreatingSharedSessions.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        setup_session_sharing_test(&workspace, &mut app);

        workspace.update(&mut app, |workspace, ctx| {
            let first_tab_id = workspace.get_pane_group_view(0).unwrap().id();

            // User tries to close all tabs right of the left-most tab, dialog comes up.
            workspace.handle_action(&WorkspaceAction::CloseTabsRight(0), ctx);
            assert!(
                workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );

            // User confirms.
            workspace.handle_close_session_confirmation_dialog_event(
                &CloseSessionConfirmationEvent::CloseSession {
                    dont_show_again: false,
                    open_confirmation_source: OpenDialogSource::CloseTabsDirection {
                        tab_index: 0,
                        direction: TabMovement::Right,
                    },
                },
                ctx,
            );
            assert!(
                !workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );
            assert_eq!(workspace.tab_count(), 1);
            assert_eq!(workspace.get_pane_group_view(0).unwrap().id(), first_tab_id);
        });
    });
}

#[test]
fn test_confirmation_dialog_dont_show_again() {
    let _guard = FeatureFlag::CreatingSharedSessions.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        app.update(disable_quit_warning);

        let workspace = mock_workspace(&mut app);
        setup_session_sharing_test(&workspace, &mut app);

        workspace.update(&mut app, |workspace, ctx| {
            // Close the tab with the shared pane, dialog comes up
            workspace.handle_action(&WorkspaceAction::CloseTab(1), ctx);
            assert!(
                workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );

            // User confirms, checking "Don't show again".
            workspace.handle_close_session_confirmation_dialog_event(
                &CloseSessionConfirmationEvent::CloseSession {
                    dont_show_again: true,
                    open_confirmation_source: OpenDialogSource::CloseTab { tab_index: 1 },
                },
                ctx,
            );
            assert!(
                !workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );
            assert_eq!(workspace.tab_count(), 2);

            // Share the first tab
            let tab_view = workspace.get_pane_group_view(0).unwrap();
            tab_view.update(ctx, |view, ctx| {
                view.terminal_manager(0, ctx)
                    .unwrap()
                    .as_ref(ctx)
                    .model()
                    .lock()
                    .set_shared_session_status(SharedSessionStatus::ActiveSharer);
            });

            // Close the shared tab. No dialog should come up and action should go through.
            workspace.handle_action(&WorkspaceAction::CloseActiveTab, ctx);
            assert!(
                !workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );
            assert_eq!(workspace.tab_count(), 1);
        });
    });
}

#[test]
fn test_close_last_tab_skip_confirmation() {
    let _guard = FeatureFlag::CreatingSharedSessions.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        app.update(disable_quit_warning);

        let workspace = mock_workspace(&mut app);
        setup_session_sharing_test(&workspace, &mut app);

        workspace.update(&mut app, |workspace, ctx| {
            // Close the non-shared tabs so there's just one shared tab left.
            workspace.handle_action(&WorkspaceAction::CloseTab(2), ctx);
            workspace.handle_action(&WorkspaceAction::CloseTab(0), ctx);
            assert_eq!(workspace.tab_count(), 1);
            // Close the last remaining tab with the shared pane, no dialog should come up because
            // we're going to close the window and there's already a confirmation on window close.
            workspace.handle_action(&WorkspaceAction::CloseActiveTab, ctx);
            assert!(
                !workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );
        });
    });
}

#[test]
fn test_notebook_pane_tracking() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            // Add a new notebook pane.
            workspace.open_notebook(
                &NotebookSource::New {
                    title: None,
                    owner: Owner::mock_current_user(),
                    initial_folder_id: None,
                },
                &OpenMnemosDriveObjectSettings::default(),
                ctx,
                true,
            );

            // Get the ID of the new notebook.
            let pane_group = workspace
                .get_pane_group_view(0)
                .expect("Pane group does not exist")
                .clone();
            let notebook_view = pane_group
                .as_ref(ctx)
                .notebook_view_at_pane_index(0, ctx)
                .expect("Notebook view was not created")
                .clone();
            let notebook_pane_id = pane_group
                .as_ref(ctx)
                .pane_id_from_index(0)
                .expect("Notebook view should have been created");
            let notebook_id = notebook_view
                .as_ref(ctx)
                .notebook_id(ctx)
                .expect("Notebook should have an ID");

            // The notebook should be registered with the NotebookManager.
            let (window, locator) = NotebookManager::as_ref(ctx)
                .find_pane(&NotebookSource::Existing(notebook_id))
                .expect("Notebook pane should be registered");
            assert_eq!(window, ctx.window_id());
            assert_eq!(
                locator,
                PaneViewLocator {
                    pane_group_id: pane_group.id(),
                    pane_id: notebook_pane_id,
                }
            );

            // Re-opening the notebook should not create a new view.
            workspace.open_notebook(
                &NotebookSource::Existing(notebook_id),
                &OpenMnemosDriveObjectSettings::default(),
                ctx,
                true,
            );
            assert_eq!(
                ctx.views_of_type::<NotebookView>(ctx.window_id()),
                Some(vec![notebook_view])
            );

            // Finally, closing the notebook pane should de-register it.
            pane_group.update(ctx, |pane_group, ctx| {
                pane_group.handle_action(&PaneGroupAction::RemoveActive, ctx)
            });
            assert_eq!(
                NotebookManager::handle(ctx)
                    .as_ref(ctx)
                    .find_pane(&NotebookSource::Existing(notebook_id)),
                None
            );
        });
    });
}

#[test]
fn test_set_active_terminal_input_contents_and_focus_app() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            let initial_buffer_contents = workspace
                .get_active_input_view_handle(ctx)
                .map(|input_view_handle| input_view_handle.as_ref(ctx).buffer_text(ctx))
                .expect("There should be an active input view");
            assert_eq!(
                "", initial_buffer_contents,
                "initial active input should be empty"
            );

            workspace.set_active_terminal_input_contents_and_focus_app("foobar", ctx);

            assert_eq!(
                "foobar",
                workspace
                    .get_active_input_view_handle(ctx)
                    .map(|input_view_handle| input_view_handle.as_ref(ctx).buffer_text(ctx))
                    .expect("There should be an active input view")
            );
            assert!(ctx.windows().app_is_active());
        });
    });
}

/// Ensures that the terminal model is destroyed when it is no longer needed.
/// This is only a "workspace" test because we want to mimic what a normal
/// user would do and expect (e.g. close a tab and expect that its backing
/// data is correctly deallocated).
///
/// TODO(suraj): we may also want to investigate a more "real" integration test
/// that inspects the application process's overall memory consumption
/// instead of just the terminal model, but this is not easy because
/// 1. we want to measure non-shared memory (i.e. the "memory" value in Activity Monitor)
///    which is not easy; it's easier to measure "real memory" or RSS, but that includes
///    shared memory across processes.
/// 2. the test might be flaky depending on how much memory is actually allocated vs
///    freed up (not something easily controlled).
///
/// For now, this test is still useful because the terminal model is one of the largest data structures
/// maintained by our app, so we want to ensure we're not introducing regressions that cause it to not
/// be deallocated correctly.
#[test]
fn test_terminal_model_isnt_leaked() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        // Turn off undo-close so that we don't need to wait for deallocation.
        UndoCloseSettings::handle(&app).update(&mut app, |settings, ctx| {
            settings
                .enabled
                .set_value(false, ctx)
                .expect("Can turn off undo-close via settings.")
        });

        let workspace = mock_workspace(&mut app);

        let terminal_model = workspace.update(&mut app, |workspace, ctx| {
            // Add another tab so that the workspace isn't destroyed when we close the tab.
            workspace.add_terminal_tab(false, ctx);

            // Get a weak reference to the model.
            let model = workspace.get_active_session_terminal_model(ctx).unwrap();
            Arc::downgrade(&model)
        });

        workspace.update(&mut app, |workspace, ctx| {
            // Remove the tab. This should destroy the corresponding terminal view.
            workspace.remove_tab(workspace.active_tab_index(), true, true, ctx);
        });
        // For some reason, the update call above results in more pending effects, one of which
        // contains the actual logic that drops the `TerminalModel`.
        app.update(|_| ());

        // If we can't upgrade the weak reference, that means it was in fact destructed.
        assert!(
            terminal_model.upgrade().is_none(),
            "The terminal model should not exist once the tab is closed."
        )
    });
}

#[test]
fn test_open_or_toggle_mnemos_drive() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // First, unconditionally open Mnemos Drive as a system action. WD should be open and welcome tips should not have opening mnemos drive.
            workspace.open_or_toggle_mnemos_drive(
                false, /* toggle */
                false, /* explicit_user_action */
                ctx,
            );
            assert!(
                workspace.current_workspace_state.is_mnemos_drive_open,
                "Mnemos Drive should be open"
            );
            assert!(
                !workspace
                    .tips_completed
                    .as_ref(ctx)
                    .features_used
                    .contains(&Tip::Action(TipAction::OpenMnemosDrive)),
                "Mnemos drive welcome tip should not be completed"
            );

            // Next, toggle mnemos drive as a user action. WD should be closed and tip should not be filled out.
            workspace.open_or_toggle_mnemos_drive(
                true, /* toggle */
                true, /* explicit_user_action */
                ctx,
            );
            assert!(
                !workspace.current_workspace_state.is_mnemos_drive_open,
                "Mnemos Drive should be closed"
            );
            assert!(
                !workspace
                    .tips_completed
                    .as_ref(ctx)
                    .features_used
                    .contains(&Tip::Action(TipAction::OpenMnemosDrive)),
                "Mnemos drive welcome tip should not be completed"
            );

            // Finally, toggle mnemos drive again as a user action. WD should be open and tip filled out.
            workspace.open_or_toggle_mnemos_drive(
                true, /* toggle */
                true, /* explicit_user_action */
                ctx,
            );
            assert!(
                workspace.current_workspace_state.is_mnemos_drive_open,
                "Mnemos Drive should be open"
            );
            assert!(
                workspace
                    .tips_completed
                    .as_ref(ctx)
                    .features_used
                    .contains(&Tip::Action(TipAction::OpenMnemosDrive)),
                "Mnemos drive welcome tip should not be completed"
            );
        });
    });
}

#[test]
fn test_stop_sharing_session() {
    use crate::terminal::shared_session::manager::Manager;
    let _guard = FeatureFlag::CreatingSharedSessions.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        // Create a workspace with a single session that's shared.
        let workspace = mock_workspace_with_shared_session(&mut app);
        let terminal_view = workspace.read(&app, |workspace, ctx| {
            assert_eq!(workspace.tabs.len(), 1);
            workspace
                .active_tab_pane_group()
                .as_ref(ctx)
                .focused_session_view(ctx)
                .unwrap()
        });

        // Stop sharing the shared session.
        workspace.update(&mut app, |workspace, ctx| {
            workspace.stop_sharing_session(
                &terminal_view.id(),
                SharedSessionActionSource::Tab,
                ctx,
            );
        });

        // Ensure that the session is no longer registered with the shared session manager.
        app.read(|ctx| {
            let manager = Manager::as_ref(ctx);
            let shared_sessions = manager.shared_views(ctx).collect_vec();
            assert_eq!(shared_sessions.len(), 0);
        });
    });
}

#[test]
fn test_stop_sharing_all_sessions_in_tab() {
    use crate::terminal::shared_session::manager::Manager;
    let _guard = FeatureFlag::CreatingSharedSessions.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        // Create a workspace with two tabs. First tab has two shared sessions. Second tab has one shared session.
        let workspace = mock_workspace_with_shared_session(&mut app);
        let second_tab_session = workspace.update(&mut app, |workspace, ctx| {
            workspace
                .active_tab_pane_group()
                .update(ctx, |pane_group, ctx| {
                    pane_group.handle_action(&PaneGroupAction::Add(Direction::Right), ctx);
                    pane_group
                        .terminal_view_at_pane_index(1, ctx)
                        .unwrap()
                        .update(ctx, |terminal_view, ctx| {
                            terminal_view.attempt_to_share_session(
                                SharedSessionScrollbackType::None,
                                None,
                                SessionSourceType::default(),
                                false,
                                ctx,
                            );
                        });
                });

            workspace.add_terminal_tab(false, ctx);
            workspace
                .active_tab_pane_group()
                .update(ctx, |pane_group, ctx| {
                    pane_group
                        .terminal_view_at_pane_index(0, ctx)
                        .unwrap()
                        .update(ctx, |terminal_view, ctx| {
                            terminal_view.attempt_to_share_session(
                                SharedSessionScrollbackType::None,
                                None,
                                SessionSourceType::default(),
                                false,
                                ctx,
                            );
                        });
                });

            workspace
                .active_tab_pane_group()
                .read(ctx, |pane_group, ctx| {
                    pane_group.terminal_view_at_pane_index(0, ctx).unwrap().id()
                })
        });

        // Ensure we have three shared sessions registered.
        app.read(|ctx| {
            let manager = Manager::as_ref(ctx);
            let shared_sessions = manager.shared_views(ctx).collect_vec();
            assert_eq!(shared_sessions.len(), 3);
        });

        // Stop sharing all sessions in first tab.
        workspace.update(&mut app, |workspace, ctx| {
            let tab = workspace.tabs[0].pane_group.downgrade();
            workspace.stop_sharing_all_panes_in_tab(&tab, ctx);
        });

        // Ensure that the only remaining shared session is the one in the other tab.
        app.read(|ctx| {
            let manager = Manager::as_ref(ctx);
            let shared_sessions = manager.shared_views(ctx).collect_vec();
            assert_eq!(shared_sessions.len(), 1);
            assert_eq!(shared_sessions[0].id(), second_tab_session);
        });
    });
}

#[test]
fn test_tab_context_menu_share_session_items() {
    let _guard = FeatureFlag::CreatingSharedSessions.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);
        let shared_pane_id = setup_session_sharing_test(&workspace, &mut app);

        workspace.update(&mut app, |workspace, ctx| {
            // Focus the shared session
            workspace.activate_tab(1, ctx);
            workspace
                .active_tab_pane_group()
                .update(ctx, |pane_group, ctx| {
                    pane_group.focus_pane_by_id(shared_pane_id, ctx);
                });
        });

        // When there's a single shared session in a tab (focused), the options
        // for sharing are "Stop sharing" and "Stop sharing all".
        workspace.read(&app, |workspace, ctx| {
            let items = workspace.tabs[1].menu_items(1, 3, ctx);
            assert!(items[0]
                .is_approximately_same_item_as(&MenuItemFields::new("Stop sharing").into_item()));
            assert!(items[1].is_approximately_same_item_as(
                &MenuItemFields::new("Stop sharing all").into_item()
            ));
        });

        // Focus the other, non-shared pane in the tab
        workspace.update(&mut app, |workspace, ctx| {
            workspace.activate_tab(1, ctx);
            workspace
                .active_tab_pane_group()
                .update(ctx, |pane_group, ctx| {
                    pane_group.pane_by_index(1).unwrap().focus(ctx);
                });
        });

        // When there's a single shared session in a tab (unfocused), the options
        // for sharing are "Share session" and "Stop sharing all".
        workspace.read(&app, |workspace, ctx| {
            let items = workspace.tabs[1].menu_items(1, 3, ctx);
            assert!(items[0]
                .is_approximately_same_item_as(&MenuItemFields::new("Share session").into_item()));
            assert!(items[1].is_approximately_same_item_as(
                &MenuItemFields::new("Stop sharing all").into_item()
            ));
        });

        // Stop sharing.
        workspace.update(&mut app, |workspace, ctx| {
            let tab = workspace.tabs[1].pane_group.downgrade();
            workspace.stop_sharing_all_panes_in_tab(&tab, ctx);
        });

        // When there's no shared sessions in a tab, the only option is "Share session".
        workspace.read(&app, |workspace, ctx| {
            let items = workspace.tabs[1].menu_items(1, 3, ctx);
            assert!(items[0]
                .is_approximately_same_item_as(&MenuItemFields::new("Share session").into_item()));
            assert!(items[1].is_approximately_same_item_as(&MenuItem::Separator));
        });
    });
}

#[test]
fn test_view_only_session() {
    let _guard = FeatureFlag::ViewingSharedSessions.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        // Trying to open command search
        let workspace = mock_workspace_viewing_shared_session(&mut app);
        workspace.update(&mut app, |workspace: &mut Workspace, ctx| {
            workspace.handle_action(&WorkspaceAction::ShowCommandSearch(Default::default()), ctx);
        });

        // Ensure command search doesn't work for read-only shared sessions
        workspace.read(&app, |workspace, _ctx| {
            assert!(!workspace.current_workspace_state.is_command_search_open);
        });
    });
}

#[test]
// This tests the end-to-end behavior to correctly switch focus among panels.
// (The only panels that can be focused currently are WD, workspace, & AI assistant.)
fn test_switch_focus_panels() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |view, ctx| {
            view.focus_active_tab(ctx);
        });
        workspace.update(&mut app, |view, ctx| {
            assert!(
                view.active_tab_pane_group().is_self_or_child_focused(ctx),
                "Expected terminal to be focused"
            );
        });

        // Shift focus from terminal to left panel when WD is open
        workspace.update(&mut app, |view, ctx| {
            view.current_workspace_state.is_mnemos_drive_open = true;
            view.focus_left_panel(ctx);
        });
        workspace.update(&mut app, |view, ctx| {
            assert!(
                view.left_panel_view.is_self_or_child_focused(ctx),
                "Expected Mnemos Drive panel to be focused"
            );
        });

        // Shift focus from WD to left panel when AI panel is open
        workspace.update(&mut app, |view, ctx| {
            view.current_workspace_state.is_ai_assistant_panel_open = true;
            view.focus_left_panel(ctx);
        });
        workspace.update(&mut app, |view, ctx| {
            assert!(
                view.ai_assistant_panel.is_self_or_child_focused(ctx),
                "Expected AI panel to be focused"
            );
        });

        // Shift focus from AI panel to left panel (terminal)
        workspace.update(&mut app, |view, ctx| {
            view.focus_left_panel(ctx);
        });
        workspace.update(&mut app, |_view, ctx| {
            assert!(
                workspace.is_self_or_child_focused(ctx),
                "Expected terminal to be focused"
            );
        });

        // Shift focus from workspace to right panel when AI assistant is open
        workspace.update(&mut app, |view, ctx| {
            view.current_workspace_state.is_ai_assistant_panel_open = true;
            view.focus_right_panel(ctx);
        });
        workspace.update(&mut app, |view, ctx| {
            assert!(
                view.ai_assistant_panel.is_self_or_child_focused(ctx),
                "Expected AI panel to be focused"
            );
        });

        // Shift focus from WD to right panel (terminal)
        workspace.update(&mut app, |view, ctx| {
            view.focus_right_panel(ctx);
        });
        workspace.update(&mut app, |_view, ctx| {
            assert!(
                workspace.is_self_or_child_focused(ctx),
                "Expected terminal to be focused"
            );
        });
    });
}

#[test]
fn test_focus_notebook() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        let pane_group = workspace.read(&app, |workspace, _ctx| {
            workspace
                .get_pane_group_view(0)
                .expect("should have pane group for tab 0")
                .clone()
        });

        let first_terminal_id = pane_group.read(&app, |panes, _ctx| {
            get_newly_created_pane_id(panes, &[])
                .as_terminal_pane_id()
                .expect("should be a terminal pane")
        });

        let notebook_id = pane_group.update(&mut app, |panes, ctx| {
            // Add a notebook to the left.
            let notebook_view = ctx.add_typed_action_view(NotebookView::new);
            panes.add_pane_with_direction(
                Direction::Left,
                NotebookPane::new(notebook_view, ctx),
                true, /* focus_new_pane */
                ctx,
            );
            get_newly_created_pane_id(panes, &[first_terminal_id.into()])
        });

        // The new pane should be focused, but the terminal is still the active session.
        pane_group.read(&app, |panes, ctx| {
            assert_eq!(panes.focused_pane_id(ctx), notebook_id);
            assert_eq!(panes.active_session_id(ctx), Some(first_terminal_id));
            assert_eq!(
                split_pane_state(panes, first_terminal_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Unfocused)
            );
            assert_eq!(
                active_session_state(panes, first_terminal_id, ctx),
                ActiveSessionState::Active
            );
            assert_eq!(
                split_pane_state(panes, notebook_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Focused)
            );
        });

        // Add a terminal below.
        let second_terminal_id = pane_group.update(&mut app, |panes, ctx| {
            panes.add_terminal_pane(Direction::Down, None, ctx);
            get_newly_created_pane_id(panes, &[first_terminal_id.into(), notebook_id])
                .as_terminal_pane_id()
                .expect("should be a terminal pane")
        });

        // The new terminal should be both focused and the active session.
        pane_group.read(&app, |panes, ctx| {
            assert_eq!(panes.focused_pane_id(ctx), second_terminal_id.into());
            assert_eq!(panes.active_session_id(ctx), Some(second_terminal_id));
            assert_eq!(
                split_pane_state(panes, first_terminal_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Unfocused)
            );
            assert_eq!(
                active_session_state(panes, first_terminal_id, ctx),
                ActiveSessionState::Inactive
            );
            assert_eq!(
                split_pane_state(panes, second_terminal_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Focused)
            );
            assert_eq!(
                active_session_state(panes, second_terminal_id, ctx),
                ActiveSessionState::Active
            );
            assert_eq!(
                split_pane_state(panes, notebook_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Unfocused)
            );
        });

        // Close the new terminal.
        pane_group.update(&mut app, |panes, ctx| {
            panes.close_pane(second_terminal_id.into(), ctx);
        });

        // Focus should switch to the notebook, and the first terminal session
        // will activate.
        pane_group.read(&app, |panes, ctx| {
            assert_eq!(panes.focused_pane_id(ctx), notebook_id);
            assert_eq!(panes.active_session_id(ctx), Some(first_terminal_id));
            assert_eq!(
                split_pane_state(panes, first_terminal_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Unfocused)
            );
            assert_eq!(
                split_pane_state(panes, notebook_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Focused)
            );
            assert_eq!(
                active_session_state(panes, first_terminal_id, ctx),
                ActiveSessionState::Active
            );
        });
    })
}

#[test]
fn test_close_active_session() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        let pane_group = workspace.read(&app, |workspace, _ctx| {
            workspace
                .get_pane_group_view(0)
                .expect("should have pane group for tab 0")
                .clone()
        });

        let first_terminal_id = pane_group.read(&app, |panes, _ctx| {
            get_newly_created_pane_id(panes, &[])
                .as_terminal_pane_id()
                .expect("should be a terminal pane")
        });

        // Add a terminal above.
        let second_terminal_id = pane_group.update(&mut app, |panes, ctx| {
            panes.add_terminal_pane(Direction::Up, None, ctx);
            get_newly_created_pane_id(panes, &[first_terminal_id.into()])
                .as_terminal_pane_id()
                .expect("should be a terminal pane")
        });

        let notebook_id = pane_group.update(&mut app, |panes, ctx| {
            // Add a notebook to the left.
            let notebook_view = ctx.add_typed_action_view(NotebookView::new);
            panes.add_pane_with_direction(
                Direction::Left,
                NotebookPane::new(notebook_view, ctx),
                true, /* focus_new_pane */
                ctx,
            );
            get_newly_created_pane_id(
                panes,
                &[first_terminal_id.into(), second_terminal_id.into()],
            )
        });

        pane_group.read(&app, |panes, ctx| {
            assert_eq!(panes.focused_pane_id(ctx), notebook_id);
            assert_eq!(panes.active_session_id(ctx), Some(second_terminal_id));
        });

        pane_group.update(&mut app, |panes, ctx| {
            // Close the active session, which should leave the notebook focused and activate the
            // remaining session.
            panes.close_pane(second_terminal_id.into(), ctx);
        });

        pane_group.read(&app, |panes, ctx| {
            assert_eq!(panes.focused_pane_id(ctx), notebook_id);
            assert_eq!(panes.active_session_id(ctx), Some(first_terminal_id));
            assert_eq!(
                split_pane_state(panes, first_terminal_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Unfocused)
            );
            assert_eq!(
                active_session_state(panes, first_terminal_id, ctx),
                ActiveSessionState::Active
            );
        });

        pane_group.update(&mut app, |panes, ctx| {
            // Now, focus the remaining session, which should keep it activated.
            panes.focus_pane_by_id(first_terminal_id.into(), ctx);
        });

        pane_group.read(&app, |panes, ctx| {
            assert_eq!(panes.focused_pane_id(ctx), first_terminal_id.into());
            assert_eq!(panes.active_session_id(ctx), Some(first_terminal_id));
            assert_eq!(
                split_pane_state(panes, first_terminal_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Focused)
            );
            assert_eq!(
                split_pane_state(panes, notebook_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Unfocused)
            );
            assert_eq!(
                active_session_state(panes, first_terminal_id, ctx),
                ActiveSessionState::Active
            );
        });
    });
}

fn set_left_panel_visibility_across_tabs(is_enabled: bool, ctx: &mut ViewContext<Workspace>) {
    WindowSettings::handle(ctx).update(ctx, |window_settings, ctx| {
        window_settings
            .left_panel_visibility_across_tabs
            .set_value(is_enabled, ctx)
            .expect("Failed to update left_panel_visibility_across_tabs setting");
    });
}

fn add_get_started_tab(workspace: &mut Workspace, ctx: &mut ViewContext<Workspace>) {
    workspace.add_tab_with_pane_layout(
        PanesLayout::Snapshot(Box::new(PaneNodeSnapshot::Leaf(LeafSnapshot {
            is_focused: true,
            custom_vertical_tabs_title: None,
            contents: LeafContents::GetStarted,
        }))),
        Arc::new(HashMap::<PaneUuid, Vec<SerializedBlockListItem>>::new()),
        None,
        ctx,
    );
}

fn find_terminal_tab_index(workspace: &Workspace, ctx: &AppContext) -> usize {
    workspace
        .tabs
        .iter()
        .position(|tab| tab.pane_group.as_ref(ctx).has_terminal_panes())
        .expect("Expected a terminal tab")
}

fn find_non_following_tab_index(workspace: &Workspace, ctx: &AppContext) -> usize {
    workspace
        .tabs
        .iter()
        .position(|tab| {
            !Workspace::should_enable_file_tree_and_global_search_for_pane_group(
                tab.pane_group.as_ref(ctx),
            )
        })
        .expect("Expected a non-following tab")
}

#[test]
fn test_left_panel_window_scoped_reconciles_between_terminal_tabs_when_enabled() {
    let _conversation_list_guard =
        FeatureFlag::AgentViewConversationListView.override_enabled(false);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            set_left_panel_visibility_across_tabs(true, ctx);

            workspace.add_terminal_tab(false, ctx);

            workspace.activate_tab(0, ctx);
            assert!(
                !workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .left_panel_open
            );
            assert!(!workspace.left_panel_open);

            workspace.open_left_panel(ctx);
            assert!(
                workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .left_panel_open
            );
            assert!(workspace.left_panel_open);

            workspace.activate_tab(1, ctx);
            assert!(
                workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .left_panel_open
            );

            workspace.close_left_panel(ctx);
            assert!(
                !workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .left_panel_open
            );
            assert!(!workspace.left_panel_open);

            workspace.activate_tab(0, ctx);
            assert!(
                !workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .left_panel_open
            );
        });
    });
}

#[test]
fn test_left_panel_window_scoped_non_following_tab_does_not_reconcile_but_updates_window_state() {
    let _conversation_list_guard =
        FeatureFlag::AgentViewConversationListView.override_enabled(false);
    let _get_started_guard = FeatureFlag::GetStartedTab.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            set_left_panel_visibility_across_tabs(true, ctx);

            // Establish window-scoped desired state = open on a terminal tab.
            workspace.open_left_panel(ctx);
            assert!(workspace.left_panel_open);

            // Create a non-following tab (e.g. Get Started), which should not auto-open even though
            // the window state is open.
            add_get_started_tab(workspace, ctx);
            let non_following_tab_index = find_non_following_tab_index(workspace, ctx);
            workspace.activate_tab(non_following_tab_index, ctx);

            assert!(
                !workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .left_panel_open
            );
            assert!(workspace.left_panel_open);

            // User actions in the non-following tab still update window state.
            workspace.open_left_panel(ctx);
            assert!(
                workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .left_panel_open
            );
            assert!(workspace.left_panel_open);

            workspace.close_left_panel(ctx);
            assert!(
                !workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .left_panel_open
            );
            assert!(!workspace.left_panel_open);

            // The window state should reconcile back onto following tabs.
            let terminal_tab_index = find_terminal_tab_index(workspace, ctx);
            workspace.activate_tab(terminal_tab_index, ctx);
            assert!(
                !workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .left_panel_open
            );

            // But toggling the window state from a following tab should not auto-open the
            // non-following tab.
            workspace.open_left_panel(ctx);
            assert!(workspace.left_panel_open);

            workspace.activate_tab(non_following_tab_index, ctx);
            assert!(
                !workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .left_panel_open
            );
            assert!(workspace.left_panel_open);
        });
    });
}

#[test]
fn test_left_panel_window_scoped_disabled_keeps_per_tab_state() {
    let _conversation_list_guard =
        FeatureFlag::AgentViewConversationListView.override_enabled(false);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            set_left_panel_visibility_across_tabs(false, ctx);

            workspace.add_terminal_tab(false, ctx);

            // Open left panel on tab 0.
            workspace.activate_tab(0, ctx);
            workspace.open_left_panel(ctx);
            assert!(
                workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .left_panel_open
            );

            // With window scoping disabled, switching tabs should not reconcile the open state.
            workspace.activate_tab(1, ctx);
            assert!(
                !workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .left_panel_open
            );

            // Each tab can be toggled independently.
            workspace.open_left_panel(ctx);
            assert!(
                workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .left_panel_open
            );

            workspace.activate_tab(0, ctx);
            assert!(
                workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .left_panel_open
            );
        });
    });
}

#[test]
fn test_vertical_tabs_panel_visibility_restores_from_window_snapshot() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        app.update(|ctx| {
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                report_if_error!(settings.use_vertical_tabs.set_value(true, ctx));
            });
        });

        let workspace = mock_workspace(&mut app);

        let closed_snapshot = workspace.update(&mut app, |workspace, ctx| {
            workspace.vertical_tabs_panel_open = false;
            workspace.snapshot(ctx.window_id(), false, ctx)
        });
        let open_snapshot = workspace.update(&mut app, |workspace, ctx| {
            workspace.vertical_tabs_panel_open = true;
            workspace.snapshot(ctx.window_id(), false, ctx)
        });

        let restored_closed = restored_workspace(&mut app, closed_snapshot);
        let restored_open = restored_workspace(&mut app, open_snapshot);

        restored_closed.read(&app, |workspace, _| {
            assert!(!workspace.vertical_tabs_panel_open);
        });
        restored_open.read(&app, |workspace, _| {
            assert!(workspace.vertical_tabs_panel_open);
        });
    });
}

#[test]
fn test_vertical_tabs_panel_defaults_open_for_new_window_when_vertical_tabs_enabled() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        app.update(|ctx| {
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                report_if_error!(settings.use_vertical_tabs.set_value(true, ctx));
            });
        });

        let workspace = mock_workspace(&mut app);

        workspace.read(&app, |workspace, _| {
            assert!(workspace.vertical_tabs_panel_open);
        });
    });
}

#[test]
fn test_vertical_tabs_panel_inherits_transferred_tab_source_window_state() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        app.update(|ctx| {
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                report_if_error!(settings.use_vertical_tabs.set_value(true, ctx));
            });
        });

        let transferred_closed = transferred_tab_workspace(&mut app, false);
        let transferred_open = transferred_tab_workspace(&mut app, true);

        transferred_closed.read(&app, |workspace, _| {
            assert!(!workspace.vertical_tabs_panel_open);
        });
        transferred_open.read(&app, |workspace, _| {
            assert!(workspace.vertical_tabs_panel_open);
        });
    });
}

#[test]
fn test_vertical_tabs_panel_auto_shows_when_setting_enabled() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.read(&app, |workspace, _| {
            assert!(!workspace.vertical_tabs_panel_open);
        });

        // Enabling vertical tabs should auto-open the panel.
        workspace.update(&mut app, |_, ctx| {
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                report_if_error!(settings.use_vertical_tabs.set_value(true, ctx));
            });
        });
        workspace.read(&app, |workspace, _| {
            assert!(workspace.vertical_tabs_panel_open);
        });

        // Disabling vertical tabs should auto-close the panel.
        workspace.update(&mut app, |_, ctx| {
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                report_if_error!(settings.use_vertical_tabs.set_value(false, ctx));
            });
        });
        workspace.read(&app, |workspace, _| {
            assert!(!workspace.vertical_tabs_panel_open);
        });
    });
}

#[test]
fn test_toggle_tab_configs_menu_opens_vertical_tabs_panel_and_menu() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                report_if_error!(settings.use_vertical_tabs.set_value(true, ctx));
            });
            workspace.vertical_tabs_panel_open = true;
        });
        workspace.update(&mut app, |workspace, ctx| {
            workspace.vertical_tabs_panel_open = false;
            workspace.show_new_session_dropdown_menu = None;

            workspace.handle_action(&WorkspaceAction::ToggleTabConfigsMenu, ctx);

            assert!(workspace.vertical_tabs_panel_open);
            assert!(workspace.show_new_session_dropdown_menu.is_some());
        });
    });
}

#[test]
fn test_toggle_tab_configs_menu_keyboard_shortcut_selects_top_item() {
    let _tab_configs_guard = FeatureFlag::TabConfigs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            workspace.show_new_session_dropdown_menu = None;

            workspace.handle_action(&WorkspaceAction::ToggleTabConfigsMenu, ctx);

            assert!(workspace.show_new_session_dropdown_menu.is_some());
            assert_eq!(
                workspace
                    .new_session_dropdown_menu
                    .read(ctx, |menu, _| menu.selected_index()),
                Some(0)
            );
        });
    });
}

#[test]
fn test_pointer_opened_tab_configs_menu_does_not_select_top_item() {
    let _tab_configs_guard = FeatureFlag::TabConfigs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            workspace.toggle_new_session_dropdown_menu(Vector2F::zero(), false, ctx);

            assert!(workspace.show_new_session_dropdown_menu.is_some());
            assert_eq!(
                workspace
                    .new_session_dropdown_menu
                    .read(ctx, |menu, _| menu.selected_index()),
                None
            );
        });
    });
}

#[test]
fn test_open_tab_config_with_params_does_not_use_worktree_branch_as_implicit_title() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        let tab_config = crate::tab_configs::TabConfig {
            name: "Untitled worktree".to_string(),
            title: None,
            color: None,
            panes: vec![TabConfigPaneNode {
                id: "main".to_string(),
                pane_type: Some(TabConfigPaneType::Terminal),
                split: None,
                children: None,
                is_focused: Some(true),
                directory: None,
                commands: Some(vec!["echo {{autogenerated_branch_name}}".to_string()]),
                shell: None,
            }],
            params: HashMap::new(),
            source_path: None,
        };

        workspace.update(&mut app, |workspace, ctx| {
            workspace.open_tab_config_with_params(
                tab_config.clone(),
                HashMap::new(),
                Some("mesa-coyote"),
                ctx,
            );
        });

        workspace.read(&app, |workspace, ctx| {
            assert_eq!(workspace.tab_count(), 2);
            assert_eq!(
                workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .custom_title(ctx),
                None
            );
        });
    });
}

#[test]
fn test_open_tab_config_with_params_uses_explicit_title_template() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        let tab_config = crate::tab_configs::TabConfig {
            name: "Titled worktree".to_string(),
            title: Some("{{autogenerated_branch_name}}".to_string()),
            color: None,
            panes: vec![TabConfigPaneNode {
                id: "main".to_string(),
                pane_type: Some(TabConfigPaneType::Terminal),
                split: None,
                children: None,
                is_focused: Some(true),
                directory: None,
                commands: Some(vec!["echo {{autogenerated_branch_name}}".to_string()]),
                shell: None,
            }],
            params: HashMap::new(),
            source_path: None,
        };

        workspace.update(&mut app, |workspace, ctx| {
            workspace.open_tab_config_with_params(
                tab_config.clone(),
                HashMap::new(),
                Some("mesa-coyote"),
                ctx,
            );
        });

        workspace.read(&app, |workspace, ctx| {
            assert_eq!(workspace.tab_count(), 2);
            assert_eq!(
                workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .custom_title(ctx),
                Some("mesa-coyote".to_string())
            );
        });
    });
}
#[test]
fn test_toggle_tab_configs_menu_does_not_change_vertical_tabs_panel_in_horizontal_mode() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                report_if_error!(settings.use_vertical_tabs.set_value(false, ctx));
            });
            workspace.vertical_tabs_panel_open = true;
            workspace.show_new_session_dropdown_menu = None;

            workspace.handle_action(&WorkspaceAction::ToggleTabConfigsMenu, ctx);

            assert!(workspace.vertical_tabs_panel_open);
            assert!(workspace.show_new_session_dropdown_menu.is_some());
        });
    });
}

#[test]
fn test_unified_new_session_menu_uses_new_worktree_config_label_and_order() {
    let _tab_configs_guard = FeatureFlag::TabConfigs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            let labels = workspace
                .unified_new_session_menu_items(ctx)
                .iter()
                .map(new_session_menu_label)
                .collect::<Vec<_>>();

            assert!(!labels.iter().any(|label| label == "Worktree in"));

            let separator_index = labels
                .iter()
                .position(|label| label == "---")
                .expect("expected a separator in the new-session menu");

            assert_eq!(
                labels.get(separator_index + 1),
                Some(&"New worktree config".to_string())
            );
            assert_eq!(
                labels.get(separator_index + 2),
                Some(&"New tab config".to_string())
            );
        });
    });
}

#[cfg(feature = "local_fs")]
#[test]
fn test_worktree_sidecar_search_editor_proxies_navigation_and_escape() {
    let _tab_configs_guard = FeatureFlag::TabConfigs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        let temp_root = TempDir::new().expect("failed to create temp dir");
        let alpha_repo = temp_root.path().join("alpha-repo");
        let beta_repo = temp_root.path().join("beta-repo");
        std::fs::create_dir_all(&alpha_repo).expect("failed to create alpha repo dir");
        std::fs::create_dir_all(&beta_repo).expect("failed to create beta repo dir");

        workspace.update(&mut app, |_, ctx| {
            PersistedWorkspace::handle(ctx).update(ctx, |persisted, ctx| {
                persisted.user_added_workspace(alpha_repo.clone(), ctx);
                persisted.user_added_workspace(beta_repo.clone(), ctx);
            });
        });

        open_worktree_sidecar(&workspace, &mut app);

        workspace.read(&app, |workspace, ctx| {
            assert!(workspace.show_new_session_sidecar);
            assert!(workspace.worktree_sidecar_search_editor.is_focused(ctx));
            assert_eq!(
                workspace
                    .new_session_sidecar_menu
                    .read(ctx, |menu, _| menu.selected_index()),
                Some(1)
            );
        });

        workspace.update(&mut app, |workspace, ctx| {
            workspace
                .worktree_sidecar_search_editor
                .update(ctx, |_, ctx| {
                    ctx.emit(Event::Navigate(NavigationKey::Down));
                });
        });
        workspace.read(&app, |workspace, ctx| {
            assert_eq!(
                workspace
                    .new_session_sidecar_menu
                    .read(ctx, |menu, _| menu.selected_index()),
                Some(2)
            );
        });

        workspace.update(&mut app, |workspace, ctx| {
            workspace
                .worktree_sidecar_search_editor
                .update(ctx, |_, ctx| {
                    ctx.emit(Event::Navigate(NavigationKey::Up));
                });
        });
        workspace.read(&app, |workspace, ctx| {
            assert_eq!(
                workspace
                    .new_session_sidecar_menu
                    .read(ctx, |menu, _| menu.selected_index()),
                Some(1)
            );
        });

        workspace.update(&mut app, |workspace, ctx| {
            workspace
                .worktree_sidecar_search_editor
                .update(ctx, |editor, ctx| {
                    editor.set_buffer_text("beta", ctx);
                });
        });
        workspace.read(&app, |workspace, ctx| {
            assert_eq!(workspace.worktree_sidecar_search_query, "beta");
            assert_eq!(
                workspace
                    .new_session_sidecar_menu
                    .read(ctx, |menu, _| menu.items_len()),
                2
            );
            assert_eq!(
                workspace
                    .new_session_sidecar_menu
                    .read(ctx, |menu, _| menu.selected_index()),
                Some(1)
            );
        });

        workspace.update(&mut app, |workspace, ctx| {
            workspace
                .worktree_sidecar_search_editor
                .update(ctx, |_, ctx| {
                    ctx.emit(Event::Escape);
                });
        });
        workspace.read(&app, |workspace, ctx| {
            assert!(workspace.show_new_session_dropdown_menu.is_none());
            assert!(!workspace.show_new_session_sidecar);
            assert!(workspace.worktree_sidecar_search_query.is_empty());
            assert!(workspace
                .worktree_sidecar_search_editor
                .as_ref(ctx)
                .buffer_text(ctx)
                .is_empty());
        });
    });
}

#[cfg(feature = "local_fs")]
#[test]
fn test_worktree_sidecar_hides_linked_worktrees_from_repo_list() {
    let _tab_configs_guard = FeatureFlag::TabConfigs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        let temp_root = TempDir::new().expect("failed to create temp dir");
        let main_repo = temp_root.path().join("main-repo");
        let linked_worktree = temp_root.path().join("linked-worktree");
        let external_git_dir = main_repo
            .join(".git")
            .join("worktrees")
            .join("linked-worktree");

        std::fs::create_dir_all(&main_repo).expect("failed to create main repo dir");
        std::fs::create_dir_all(&linked_worktree).expect("failed to create linked worktree dir");
        std::fs::create_dir_all(&external_git_dir).expect("failed to create external git dir");

        workspace.update(&mut app, |_, ctx| {
            PersistedWorkspace::handle(ctx).update(ctx, |persisted, ctx| {
                persisted.user_added_workspace(main_repo.clone(), ctx);
                persisted.user_added_workspace(linked_worktree.clone(), ctx);
            });

            let main_repo_canon =
                CanonicalizedPath::try_from(main_repo.as_path()).expect("canonical main repo");
            let linked_worktree_canon = CanonicalizedPath::try_from(linked_worktree.as_path())
                .expect("canonical linked worktree");
            let external_git_dir_canon = CanonicalizedPath::try_from(external_git_dir.as_path())
                .expect("canonical external git dir");

            let main_repo_std: mnemos_util::standardized_path::StandardizedPath =
                main_repo_canon.into();
            let linked_worktree_std: mnemos_util::standardized_path::StandardizedPath =
                linked_worktree_canon.into();
            let external_git_dir_std: mnemos_util::standardized_path::StandardizedPath =
                external_git_dir_canon.into();

            DetectedRepositories::handle(ctx).update(ctx, |repos, _ctx| {
                repos.insert_test_repo_root(main_repo_std.clone());
                repos.insert_test_repo_root(linked_worktree_std.clone());
            });

            DirectoryWatcher::handle(ctx).update(ctx, |watcher, ctx| {
                watcher
                    .add_directory_with_git_dir(main_repo_std, None, ctx)
                    .expect("register main repo");
                watcher
                    .add_directory_with_git_dir(
                        linked_worktree_std,
                        Some(external_git_dir_std),
                        ctx,
                    )
                    .expect("register linked worktree");
            });
        });

        open_worktree_sidecar(&workspace, &mut app);

        workspace.read(&app, |workspace, ctx| {
            let labels = workspace.new_session_sidecar_menu.read(ctx, |menu, _| {
                menu.items()
                    .iter()
                    .filter_map(|item| match item {
                        MenuItem::Item(fields) => Some(fields.label().to_string()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            });

            let main_repo_label = main_repo.to_string_lossy().to_string();
            let linked_worktree_label = linked_worktree.to_string_lossy().to_string();

            assert!(labels.iter().any(|label| label == "Search repos"));
            assert!(labels.iter().any(|label| label == &main_repo_label));
            assert!(!labels.iter().any(|label| label == &linked_worktree_label));
        });
    });
}

#[test]
fn test_vertical_tabs_context_menu_does_not_show_hover_only_tab_bar() {
    let _full_screen_zen_mode_guard = FeatureFlag::FullScreenZenMode.override_enabled(true);
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                report_if_error!(settings
                    .workspace_decoration_visibility
                    .set_value(WorkspaceDecorationVisibility::OnHover, ctx));
                report_if_error!(settings.use_vertical_tabs.set_value(true, ctx));
            });
            workspace.should_show_ai_assistant_warm_welcome = false;
            workspace.vertical_tabs_panel_open = true;

            workspace.show_tab_right_click_menu =
                Some((0, TabContextMenuAnchor::Pointer(Vector2F::zero())));

            assert_eq!(workspace.tab_bar_mode(ctx), ShowTabBar::Hidden);
        });
    });
}

#[test]
fn test_standard_tab_context_menu_shows_hover_only_tab_bar() {
    let _full_screen_zen_mode_guard = FeatureFlag::FullScreenZenMode.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                report_if_error!(settings
                    .workspace_decoration_visibility
                    .set_value(WorkspaceDecorationVisibility::OnHover, ctx));
            });
            workspace.should_show_ai_assistant_warm_welcome = false;

            workspace.show_tab_right_click_menu =
                Some((0, TabContextMenuAnchor::Pointer(Vector2F::zero())));

            assert_eq!(workspace.tab_bar_mode(ctx), ShowTabBar::Stacked);
        });
    });
}

#[test]
fn test_open_cloud_agent_setup_guide_action_opens_management_view_and_is_idempotent() {
    let _agent_management_guard = FeatureFlag::AgentManagementView.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        // The action is gated on AI being enabled, and this fork ships with
        // `is_any_ai_enabled` defaulting to off.
        crate::settings::AISettings::handle(&app).update(&mut app, |settings, ctx| {
            report_if_error!(settings.is_any_ai_enabled.set_value(true, ctx));
        });

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            assert!(
                !workspace
                    .current_workspace_state
                    .is_agent_management_view_open
            );

            workspace.handle_action(&WorkspaceAction::OpenCloudAgentSetupGuide, ctx);
            assert!(
                workspace
                    .current_workspace_state
                    .is_agent_management_view_open
            );
            assert!(workspace
                .agent_management_view
                .as_ref(ctx)
                .is_showing_setup_guide());

            workspace.handle_action(&WorkspaceAction::OpenCloudAgentSetupGuide, ctx);
            assert!(
                workspace
                    .current_workspace_state
                    .is_agent_management_view_open
            );
            assert!(workspace
                .agent_management_view
                .as_ref(ctx)
                .is_showing_setup_guide());
        });
    });
}

// ---- Mnemos IDE project-tab sync ----------------------------------------------
//
// Jobs are recorded, not run, under `cfg(test)` (see `mnemos_ide_sync::ide_enqueue`),
// so none of these touch the shared redirected project store.

fn ide_take_queued(app: &mut App) -> Vec<crate::workspace::ide_sync::IdeJob> {
    WorkspaceRegistry::handle(app).update(app, |registry, _| registry.ide_take_queued())
}

fn ide_terminal_leaf(cwd: &str, focused: bool) -> crate::app_state::PaneNodeSnapshot {
    use crate::app_state::{LeafContents, LeafSnapshot, PaneNodeSnapshot, TerminalPaneSnapshot};
    PaneNodeSnapshot::Leaf(LeafSnapshot {
        is_focused: focused,
        custom_vertical_tabs_title: None,
        contents: LeafContents::Terminal(TerminalPaneSnapshot {
            uuid: Vec::new(),
            cwd: Some(cwd.to_string()),
            shell_launch_data: None,
            is_active: focused,
            is_read_only: false,
            input_config: None,
            llm_model_override: None,
            active_profile_id: None,
            conversation_ids_to_restore: Vec::new(),
            active_conversation_id: None,
        }),
    })
}

fn ide_set_terminal_cwds(node: &mut crate::app_state::PaneNodeSnapshot, cwd: &str) {
    use crate::app_state::{LeafContents, PaneNodeSnapshot};
    match node {
        PaneNodeSnapshot::Leaf(leaf) => {
            if let LeafContents::Terminal(terminal) = &mut leaf.contents {
                terminal.cwd = Some(cwd.to_string());
            }
        }
        PaneNodeSnapshot::Branch(branch) => {
            for (_, child) in &mut branch.children {
                ide_set_terminal_cwds(child, cwd);
            }
        }
    }
}

#[test]
fn test_ide_snapshot_cwds_put_the_focused_pane_of_the_active_terminal_tab_first() {
    use crate::app_state::{
        BranchSnapshot, PaneFlex, PaneNodeSnapshot, PaneTabSnapshot, SplitDirection,
    };
    let terminal_tab = |root: PaneNodeSnapshot| PaneTabSnapshot {
        id: Vec::new(),
        title: None,
        root,
    };
    let tabs = vec![
        terminal_tab(ide_terminal_leaf("/work/other", true)),
        terminal_tab(PaneNodeSnapshot::Branch(BranchSnapshot {
            direction: SplitDirection::Horizontal,
            children: vec![
                (PaneFlex(1.), ide_terminal_leaf("/work/logs", false)),
                (PaneFlex(1.), ide_terminal_leaf("/work/app", true)),
                (PaneFlex(1.), ide_terminal_leaf("/work/app", false)),
            ],
        })),
    ];
    assert_eq!(
        ide_tab_sync::snapshot_cwds(&tabs, 1),
        vec![
            PathBuf::from("/work/app"),
            PathBuf::from("/work/logs"),
            PathBuf::from("/work/other"),
        ]
    );
}

#[test]
fn test_ide_restored_tabs_report_their_link_title_and_snapshot_cwd() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let checkout = tempfile::tempdir().unwrap();
        let checkout_s = checkout.path().to_string_lossy().into_owned();

        let workspace = mock_workspace(&mut app);
        let mut snapshot = workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);
            workspace.snapshot(ctx.window_id(), false, ctx)
        });
        assert_eq!(snapshot.tabs.len(), 2);
        snapshot.tabs[0].tcb_project_id = Some("proj-a".to_string());
        snapshot.tabs[0].custom_title = Some("Alpha".to_string());
        for terminal_tab in &mut snapshot.tabs[0].terminal_tabs {
            ide_set_terminal_cwds(&mut terminal_tab.root, &checkout_s);
        }
        ide_take_queued(&mut app);

        let restored = restored_workspace(&mut app, snapshot);
        app.update(|ctx| {
            let infos = Workspace::ide_tab_infos(&restored, ctx);
            assert_eq!(infos.len(), 2);
            assert_eq!(infos[0].project_id.as_deref(), Some("proj-a"));
            assert_eq!(infos[0].title.as_deref(), Some("Alpha"));
            assert!(
                infos[0].cwds.contains(&checkout.path().to_path_buf()),
                "a pane that has not bootstrapped yet reports its restored cwd: {:?}",
                infos[0].cwds
            );
            assert!(infos[0].safe_to_close);
            assert_eq!(infos[1].project_id, None);
        });
        // A restored window asks for a reconcile to match its tabs up.
        assert_eq!(
            ide_take_queued(&mut app),
            vec![crate::workspace::ide_sync::IdeJob::Reconcile]
        );
    });
}

#[test]
fn test_ide_closing_tabs_closes_only_the_tabs_own_project() {
    use crate::workspace::ide_sync::IdeJob;
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);
            workspace.tabs[1].tcb_project_id = Some("p1".to_string());
            workspace.tabs[3].tcb_project_id = Some("p1".to_string());
            // A plain tab's close must never act on the active project.
            let mut active = mnemos_ide_projects::Project::new("active", PathBuf::from("/x"));
            active.id = "p-active".to_string();
            workspace.ide_project_tabs =
                mnemos_ide_projects::ProjectTabs::new(vec![active], Some("p-active".to_string()));
        });
        ide_take_queued(&mut app);

        // A plain tab closes like any tab: no project action at all.
        workspace.update(&mut app, |workspace, ctx| {
            workspace.close_tab(2, true, true, ctx)
        });
        assert_eq!(ide_take_queued(&mut app), Vec::<IdeJob>::new());

        // A project tab records the close; the queue decides (across every
        // window) whether another tab still shows the project.
        workspace.update(&mut app, |workspace, ctx| {
            assert_eq!(workspace.tabs[1].tcb_project_id.as_deref(), Some("p1"));
            workspace.close_tab(1, true, true, ctx);
        });
        assert_eq!(
            ide_take_queued(&mut app),
            vec![IdeJob::TabClosed {
                project_id: "p1".to_string()
            }]
        );
        workspace.read(&app, |workspace, _| assert_eq!(workspace.tabs.len(), 2));
    });
}

#[test]
fn test_ide_torn_off_tab_takes_its_project_link_along() {
    use crate::workspace::ide_sync::IdeJob;
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let source = mock_workspace(&mut app);
        let target = mock_workspace(&mut app);
        let moved = source.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);
            workspace.tabs[1].tcb_project_id = Some("p1".to_string());
            workspace.tabs[1].pane_group.clone()
        });
        // What tab tear-off does: the new window adopts the pane group in an
        // unlinked tab, then the source tab is removed.
        target.update(&mut app, |workspace, _| {
            workspace.tabs.push(TabData::new(moved.clone()));
        });
        ide_take_queued(&mut app);
        source.update(&mut app, |workspace, ctx| {
            workspace.remove_tab_without_undo(1, ctx)
        });

        assert_eq!(
            ide_take_queued(&mut app),
            Vec::<IdeJob>::new(),
            "moving a tab closes nothing"
        );
        target.read(&app, |workspace, _| {
            let tab = workspace
                .tabs
                .iter()
                .find(|t| t.pane_group.id() == moved.id())
                .unwrap();
            assert_eq!(tab.tcb_project_id.as_deref(), Some("p1"));
        });
    });
}

#[test]
fn test_ide_update_relinks_closes_and_opens_tabs_in_its_own_window() {
    use crate::workspace::ide_sync::{
        IdeJob, IdeTabOpen, IdeTabRef, IdeUpdate, WorkspaceRegistryEvent,
    };
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let checkout = tempfile::tempdir().unwrap();
        let workspace = mock_workspace(&mut app);
        let (window_id, pane_groups) = workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);
            workspace.tabs[0].tcb_project_id = Some("keep".to_string());
            workspace.tabs[1].tcb_project_id = Some("stale".to_string());
            workspace.tabs[2].tcb_project_id = Some("closed".to_string());
            workspace.activate_tab(0, ctx);
            let pane_groups: Vec<EntityId> =
                workspace.tabs.iter().map(|t| t.pane_group.id()).collect();
            (ctx.window_id(), pane_groups)
        });
        ide_take_queued(&mut app);
        let other_window = mnemos_ui::WindowId::from_usize(usize::MAX);
        let tab = |pane_group: EntityId, id: &str| IdeTabRef {
            window_id,
            pane_group_id: pane_group,
            project_id: Some(id.to_string()),
        };
        let mut opened = mnemos_ide_projects::Project::new("Opened", checkout.path().to_path_buf());
        opened.id = "opened".to_string();
        let update = IdeUpdate {
            open_projects: Some(vec![opened]),
            relink: vec![
                (tab(pane_groups[1], "stale"), Some("folded".to_string())),
                // The tab no longer carries the id the job saw: left alone.
                (tab(pane_groups[0], "not-its-id"), None),
            ],
            close: vec![tab(pane_groups[2], "closed")],
            open: vec![
                IdeTabOpen {
                    window_id,
                    project_id: "opened".to_string(),
                    title: "Opened".to_string(),
                    cwd: Some(checkout.path().to_path_buf()),
                    focus: false,
                },
                IdeTabOpen {
                    window_id: other_window,
                    project_id: "elsewhere".to_string(),
                    title: "Elsewhere".to_string(),
                    cwd: None,
                    focus: false,
                },
            ],
            focus: None,
        };
        workspace.update(&mut app, |workspace, ctx| {
            workspace.handle_workspace_registry_event(
                &WorkspaceRegistryEvent::Ide(Arc::new(update)),
                ctx,
            );
        });

        workspace.read(&app, |workspace, ctx| {
            let tabs: Vec<(EntityId, Option<&str>)> = workspace
                .tabs
                .iter()
                .map(|t| (t.pane_group.id(), t.tcb_project_id.as_deref()))
                .collect();
            assert_eq!(tabs.len(), 3, "{tabs:?}");
            assert_eq!(tabs[0], (pane_groups[0], Some("keep")));
            assert_eq!(tabs[1], (pane_groups[1], Some("folded")));
            assert_eq!(tabs[2].1, Some("opened"));
            assert_eq!(
                workspace.tabs[2]
                    .pane_group
                    .as_ref(ctx)
                    .custom_title(ctx)
                    .as_deref(),
                Some("Opened")
            );
            assert_eq!(
                workspace.tabs[workspace.active_tab_index].pane_group.id(),
                pane_groups[0],
                "a reconcile open never takes focus"
            );
            assert_eq!(
                workspace
                    .ide_project_tabs
                    .projects()
                    .iter()
                    .map(|p| p.id.as_str())
                    .collect::<Vec<_>>(),
                vec!["opened"]
            );
        });
        // Closing a closed project's tab is not the user closing a project.
        assert_eq!(ide_take_queued(&mut app), Vec::<IdeJob>::new());
    });
}

#[test]
fn test_ide_closing_the_last_tab_for_a_closed_project_keeps_the_window() {
    use crate::workspace::ide_sync::{IdeTabRef, IdeUpdate, WorkspaceRegistryEvent};
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);
        let (window_id, pane_group) = workspace.update(&mut app, |workspace, ctx| {
            assert_eq!(workspace.tabs.len(), 1);
            workspace.tabs[0].tcb_project_id = Some("closed".to_string());
            (ctx.window_id(), workspace.tabs[0].pane_group.id())
        });
        let update = IdeUpdate {
            close: vec![IdeTabRef {
                window_id,
                pane_group_id: pane_group,
                project_id: Some("closed".to_string()),
            }],
            ..Default::default()
        };
        workspace.update(&mut app, |workspace, ctx| {
            workspace.handle_workspace_registry_event(
                &WorkspaceRegistryEvent::Ide(Arc::new(update)),
                ctx,
            );
        });
        workspace.read(&app, |workspace, _| {
            assert_eq!(workspace.tabs.len(), 1, "the window's last tab stays");
            assert_eq!(workspace.tabs[0].tcb_project_id, None, "as a plain tab");
        });
    });
}

#[test]
fn test_ide_closing_a_window_closes_its_projects_and_undo_reopens_them() {
    use crate::workspace::ide_sync::IdeJob;
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let primary = mock_workspace(&mut app);
        let second = mock_workspace(&mut app);
        let moved = second.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);
            workspace.tabs[0].tcb_project_id = Some("p1".to_string());
            workspace.tabs[2].tcb_project_id = Some("p2".to_string());
            workspace.tabs[2].pane_group.clone()
        });
        // `p2`'s pane group was torn off into the primary window already.
        primary.update(&mut app, |workspace, _| {
            workspace.tabs.push(TabData::new(moved.clone()));
        });
        ide_take_queued(&mut app);

        // What `on_window_closed` does.
        second.update(&mut app, |workspace, ctx| {
            let window_id = ctx.window_id();
            WorkspaceRegistry::handle(ctx).update(ctx, |registry, _| registry.unregister(window_id));
            workspace.ide_window_closed(ctx);
        });
        assert_eq!(
            ide_take_queued(&mut app),
            vec![IdeJob::TabClosed {
                project_id: "p1".to_string()
            }],
            "its project tabs close like closed tabs; the moved one moves its link"
        );
        primary.read(&app, |workspace, _| {
            let tab = workspace.tabs.iter().find(|t| t.pane_group.id() == moved.id()).unwrap();
            assert_eq!(tab.tcb_project_id.as_deref(), Some("p2"));
        });

        // Undo close window: registered again, its projects open again.
        let window_id = second.update(&mut app, |workspace, ctx| {
            workspace.ide_window_reopened(ctx);
            ctx.window_id()
        });
        assert_eq!(
            ide_take_queued(&mut app),
            vec![
                IdeJob::TabReopened {
                    project_id: "p1".to_string()
                },
                IdeJob::TabReopened {
                    project_id: "p2".to_string()
                },
                IdeJob::Reconcile,
            ]
        );
        app.read(|ctx| {
            assert!(WorkspaceRegistry::as_ref(ctx).get(window_id, ctx).is_some());
        });
    });
}

#[test]
fn test_ide_activating_a_project_tab_records_it_once() {
    use crate::workspace::ide_sync::IdeJob;
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);
            workspace.tabs[1].tcb_project_id = Some("p1".to_string());
            workspace.tabs[2].tcb_project_id = Some("p1".to_string());
        });
        ide_take_queued(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            workspace.activate_tab(1, ctx);
            workspace.activate_tab(0, ctx);
            workspace.activate_tab(2, ctx);
        });
        assert_eq!(
            ide_take_queued(&mut app),
            vec![IdeJob::Activated {
                project_id: "p1".to_string()
            }],
            "a plain tab and a second tab of the same project record nothing"
        );
    });
}

/// Agent restore asks for the other machines' live agents while the first
/// window is still being built, before the registry's polling has started
/// (and under `cfg(test)` it never starts). The answer must come from that
/// request, not from the first scheduled tick: signed out, it is "not read"
/// at once, so restore decides without waiting.
#[test]
fn live_agents_wanted_answers_before_polling_starts() {
    use crate::workspace::live_agents::LiveAgentsAnswer;
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let _workspace = mock_workspace(&mut app);
        WorkspaceRegistry::handle(&app).read(&app, |registry, _| {
            assert!(matches!(registry.live_agents_answer(), LiveAgentsAnswer::Pending));
        });
        WorkspaceRegistry::handle(&app)
            .update(&mut app, |registry, ctx| registry.live_agents_wanted(ctx));
        WorkspaceRegistry::handle(&app).read(&app, |registry, _| {
            assert!(matches!(registry.live_agents_answer(), LiveAgentsAnswer::NotRead));
        });
    });
}

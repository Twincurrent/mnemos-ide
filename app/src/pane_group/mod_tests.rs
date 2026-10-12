use crate::terminal::cli_agent_sessions::CLIAgentSessionsModel;
use crate::{
    ai::{
        active_agent_views_model::ActiveAgentViewsModel,
        agent_conversations_model::AgentConversationsModel,
        ambient_agents::github_auth_notifier::GitHubAuthNotifier,
        blocklist::BlocklistAIHistoryModel,
        document::ai_document_model::AIDocumentModel,
        execution_profiles::profiles::AIExecutionProfilesModel,
        llms::LLMPreferences,
        mcp::{
            templatable_manager::TemplatableMCPServerManager, FileBasedMCPManager, FileMCPWatcher,
        },
        outline::RepoOutlines,
        persisted_workspace::PersistedWorkspace,
        restored_conversations::RestoredAgentConversations,
        skills::SkillManager,
        AIRequestUsageModel,
    },
    auth::auth_manager::AuthManager,
    cloud_object::model::persistence::CloudModel,
    context_chips::prompt::Prompt,
    network::NetworkStatus,
    notebooks::{
        editor::keys::NotebookKeybindings, manager::NotebookManager, notebook::NotebookView,
    },
    resource_center::TipsCompleted,
    search::files::model::FileSearchModel,
    server::{
        cloud_objects::{listener::Listener, update_manager::UpdateManager},
        server_api::ServerApiProvider,
        sync_queue::SyncQueue,
        telemetry::context_provider::AppTelemetryContextProvider,
    },
    settings::PrivacySettings,
    settings_view::keybindings::KeybindingChangedNotifier,
    suggestions::ignored_suggestions_model::IgnoredSuggestionsModel,
    system::SystemStats,
    terminal::history::History,
    terminal::{
        alt_screen_reporting::AltScreenReporting,
        keys::TerminalKeybindings,
        local_tty::{spawner::PtySpawner, TerminalManager},
        shared_session::{
            SharedSessionActionSource, SharedSessionScrollbackType, SharedSessionStatus,
        },
    },
    test_util::settings::initialize_settings_for_tests,
    undo_close::UndoCloseStack,
    mnemos_managed_paths_watcher::MnemosManagedPathsWatcher,
    workflows::local_workflows::LocalWorkflows,
    workspace::{
        sync_inputs::SyncedInputState, ActiveSession, OneTimeModalModel, WorkspaceRegistry,
    },
    workspaces::{
        team_tester::TeamTesterStatus, update_manager::TeamUpdateManager,
        user_profiles::UserProfiles, user_workspaces::UserWorkspaces,
    },
    AgentNotificationsModel, GlobalResourceHandles, GlobalResourceHandlesProvider,
};
#[cfg(feature = "local_fs")]
use repo_metadata::RepoMetadataModel;
use repo_metadata::{repositories::DetectedRepositories, watcher::DirectoryWatcher};
use std::collections::HashMap;
use watcher::HomeDirectoryWatcher;

use super::*;
use crate::terminal::resizable_data::ResizableData;
use ai::{
    index::full_source_code_embedding::manager::CodebaseIndexManager,
    project_context::model::ProjectContextModel,
};
use pathfinder_geometry::rect::RectF;
use shared_session::permissions_manager::SessionPermissionsManager;
use mnemos_ui::windowing::{state::ApplicationStage, WindowManager};
use mnemos_ui::{
    platform::{WindowBounds, WindowStyle},
    App, ModelHandle,
};

fn initialize_app(app: &mut App) {
    initialize_settings_for_tests(app);

    app.add_singleton_model(|_ctx| ServerApiProvider::new_for_test());
    app.add_singleton_model(|_| AuthStateProvider::new_for_test());
    app.add_singleton_model(AppTelemetryContextProvider::new_context_provider);
    app.add_singleton_model(AuthManager::new_for_test);
    app.add_singleton_model(|_ctx| PtySpawner::new_for_test());
    // Terminal panes call `CLIAgentEventFileIngestor::handle(ctx)` during
    // construction (terminal_pane.rs / terminal_manager.rs), so the test harness
    // must register it or every terminal-constructing test panics with
    // "singleton ... never registered".
    app.add_singleton_model(
        crate::terminal::cli_agent_sessions::file_ingest::CLIAgentEventFileIngestor::new_for_test,
    );
    app.add_singleton_model(|_| NetworkStatus::new());
    app.add_singleton_model(|_| SystemStats::new());
    app.add_singleton_model(SyncQueue::mock);
    app.add_singleton_model(CloudModel::mock);
    app.add_singleton_model(UserWorkspaces::default_mock);
    app.add_singleton_model(TeamTesterStatus::mock);
    app.add_singleton_model(TeamUpdateManager::mock);
    app.add_singleton_model(Listener::mock);
    app.add_singleton_model(UpdateManager::mock);

    // Initialize file-based MCP dependencies.
    app.add_singleton_model(|_| DetectedRepositories::default());
    app.add_singleton_model(HomeDirectoryWatcher::new_for_test);
    app.add_singleton_model(DirectoryWatcher::new);
    app.add_singleton_model(MnemosManagedPathsWatcher::new_for_testing);
    app.add_singleton_model(FileMCPWatcher::new);
    app.add_singleton_model(|_| FileBasedMCPManager::default());

    app.add_singleton_model(|_| TemplatableMCPServerManager::default());
    app.add_singleton_model(|_ctx| UserProfiles::new(Vec::new()));
    app.add_singleton_model(|_| Appearance::mock());
    app.add_singleton_model(PrivacySettings::mock);
    app.add_singleton_model(|_ctx| SyncedInputState::mock());
    app.add_singleton_model(LocalWorkflows::new);
    app.add_singleton_model(|_| Prompt::mock());
    app.add_singleton_model(|_| ResizableData::default());
    app.add_singleton_model(NotebookManager::mock);
    app.add_singleton_model(shared_session::manager::Manager::new);
    app.add_singleton_model(|_| ActiveSession::default());
    let global_resources = GlobalResourceHandles::mock(app);
    app.add_singleton_model(|_| GlobalResourceHandlesProvider::new(global_resources.clone()));
    app.add_singleton_model(|_| KeybindingChangedNotifier::new());
    app.add_singleton_model(NotebookKeybindings::new);
    app.add_singleton_model(TerminalKeybindings::new);
    app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
    app.add_singleton_model(|_| CLIAgentSessionsModel::new());
    app.add_singleton_model(|_| ActiveAgentViewsModel::new());
    app.add_singleton_model(AgentNotificationsModel::new);
    app.add_singleton_model(|ctx| {
        AIExecutionProfilesModel::new(&crate::LaunchMode::new_for_unit_test(), ctx)
    });
    app.add_singleton_model(|ctx| {
        AIRequestUsageModel::new_for_test(ServerApiProvider::as_ref(ctx).get_ai_client(), ctx)
    });
    app.add_singleton_model(SessionPermissionsManager::new);
    app.add_singleton_model(LLMPreferences::new);
    #[cfg(feature = "local_fs")]
    app.add_singleton_model(RepoMetadataModel::new);
    app.add_singleton_model(SkillManager::new);
    app.add_singleton_model(FileSearchModel::new);
    app.add_singleton_model(|_| crate::code_review::git_status_update::GitStatusUpdateModel::new());
    app.add_singleton_model(RepoOutlines::new_for_test);
    crate::terminal::available_shells::register(app);
    AltScreenReporting::register(app);
    app.add_singleton_model(|ctx| {
        CodebaseIndexManager::new_for_test(ServerApiProvider::as_ref(ctx).get(), ctx)
    });
    app.add_singleton_model(|ctx| PersistedWorkspace::new(vec![], HashMap::new(), None, ctx));
    app.add_singleton_model(|_| ProjectContextModel::default());
    app.add_singleton_model(|_| RestoredAgentConversations::new(vec![]));
    app.add_singleton_model(OneTimeModalModel::new);
    app.add_singleton_model(|_| WorkspaceRegistry::new());
    app.add_singleton_model(UndoCloseStack::new);
    app.add_singleton_model(|_| IgnoredSuggestionsModel::new(vec![]));
    app.add_singleton_model(AIDocumentModel::new);
    app.add_singleton_model(|_| History::new(vec![]));
    app.add_singleton_model(|_| GitHubAuthNotifier::new());
    app.add_singleton_model(AgentConversationsModel::new);
    app.add_singleton_model(remote_server::manager::RemoteServerManager::new);
}

struct MockOptions {
    layout: PanesLayout,
    window_bounds: WindowBounds,
}

impl Default for MockOptions {
    fn default() -> Self {
        Self {
            layout: Default::default(),
            window_bounds: WindowBounds::ExactPosition(RectF::new(
                Vector2F::zero(),
                Vector2F::new(1024., 768.),
            )),
        }
    }
}

fn mock_pane_group(app: &mut App, options: MockOptions) -> ViewHandle<PaneGroup> {
    let tips_model = app.add_model(|_| TipsCompleted::default());
    let (_, pane_group) =
        app.add_window_with_bounds(WindowStyle::NotStealFocus, options.window_bounds, |ctx| {
            let user_default_shell_changed_banner_dismissal_model_handle =
                ctx.add_model(|_| BannerState::default());
            let block_lists = Arc::new(HashMap::new());
            PaneGroup::new_with_panes_layout(
                tips_model,
                user_default_shell_changed_banner_dismissal_model_handle,
                ServerApiProvider::as_ref(ctx).get(),
                options.layout,
                block_lists,
                None,
                ctx,
            )
        });
    pane_group
}

fn get_newly_created_pane_id(panes: &PaneGroup, existing_ids: &[PaneId]) -> PaneId {
    panes
        .pane_ids()
        .find(|id| !existing_ids.contains(id))
        .unwrap()
}

fn split_pane_state(panes: &PaneGroup, pane_id: PaneId, ctx: &AppContext) -> SplitPaneState {
    panes
        .focus_state_handle()
        .as_ref(ctx)
        .split_pane_state_for(pane_id)
}

fn is_active_session(panes: &PaneGroup, pane_id: PaneId, ctx: &AppContext) -> bool {
    panes.active_session_id(ctx).map(Into::into) == Some(pane_id)
}

fn new_notebook(ctx: &mut ViewContext<PaneGroup>) -> ViewHandle<NotebookView> {
    ctx.add_typed_action_view(NotebookView::new)
}

struct PreAttachReturnsFalsePane {
    pane_id: PaneId,
    pane_configuration: ModelHandle<PaneConfiguration>,
}

impl PreAttachReturnsFalsePane {
    fn new(ctx: &mut ViewContext<PaneGroup>) -> Self {
        Self {
            pane_id: PaneId::dummy_pane_id(),
            pane_configuration: ctx.add_model(|_ctx| PaneConfiguration::new("")),
        }
    }
}

impl pane::PaneContent for PreAttachReturnsFalsePane {
    fn id(&self) -> PaneId {
        self.pane_id
    }

    fn pre_attach(&self, _group: &PaneGroup, _ctx: &mut ViewContext<PaneGroup>) -> bool {
        false
    }

    fn attach(
        &self,
        _group: &PaneGroup,
        _focus_handle: focus_state::PaneFocusHandle,
        _ctx: &mut ViewContext<PaneGroup>,
    ) {
    }

    fn detach(
        &self,
        _group: &PaneGroup,
        _detach_type: pane::DetachType,
        _ctx: &mut ViewContext<PaneGroup>,
    ) {
    }

    fn snapshot(&self, _app: &AppContext) -> LeafContents {
        LeafContents::GetStarted
    }

    fn has_application_focus(&self, _ctx: &mut ViewContext<PaneGroup>) -> bool {
        false
    }

    fn focus(&self, _ctx: &mut ViewContext<PaneGroup>) {}

    fn shareable_link(
        &self,
        _ctx: &mut ViewContext<PaneGroup>,
    ) -> Result<pane::ShareableLink, pane::ShareableLinkError> {
        Ok(pane::ShareableLink::Base)
    }

    fn pane_configuration(&self) -> ModelHandle<PaneConfiguration> {
        self.pane_configuration.clone()
    }

    fn is_pane_being_dragged(&self, _ctx: &AppContext) -> bool {
        false
    }
}

// TODO: This test is commented out for now until we can fix it. It is flaky and sometimes hangs, causing the CI to cancel.
// #[test]
// #[allow(clippy::clone_on_copy)]
// fn test_pane_history() {
//     App::test((), |mut app| async move {
//         let pane_group = mock_pane_group(&mut app, platform);

//         pane_group.update(&mut app, |panes, ctx| {
//             let mut entity_ids: Vec<EntityId> =
//                 panes.view_id_to_session_data.keys().cloned().collect();

//             let first_entity_id = entity_ids.get(0).unwrap().clone();

//             // Add pane Left.
//             panes.add_pane(Direction::Left, ctx);
//             entity_ids = panes.view_id_to_session_data.keys().cloned().collect();
//             entity_ids.retain(|x| *x != first_entity_id);
//             let second_entity_id = entity_ids.get(0).unwrap().clone();
//             // Add pane Up.
//             panes.add_pane(Direction::Up, ctx);
//             entity_ids = panes.view_id_to_session_data.keys().cloned().collect();
//             entity_ids.retain(|x| *x != first_entity_id && *x != second_entity_id);
//             let third_entity_id = entity_ids.get(0).unwrap().clone();

//             assert!(panes.prev_session_id(third_entity_id).unwrap() == second_entity_id);
//         })
//     });
// }

#[test]
#[allow(clippy::clone_on_copy)]
fn test_pane_focus_on_close() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            let first_pane_id = get_newly_created_pane_id(panes, &[]);

            // Add pane Left.
            panes.add_terminal_pane(Direction::Left, None, ctx);
            let second_pane_id = get_newly_created_pane_id(panes, &[first_pane_id]);

            assert!(panes.prev_pane_id(second_pane_id).unwrap() == first_pane_id);

            // Add pane Up.
            panes.add_terminal_pane(Direction::Up, None, ctx);
            let third_pane_id = get_newly_created_pane_id(panes, &[first_pane_id, second_pane_id]);

            // Close the third pane and check that the second pane opened is now focused.
            panes.close_pane(third_pane_id, ctx);
            assert_eq!(second_pane_id, panes.focused_pane_id(ctx));
        })
    });
}

// --- Terminal-tab behavior (browser-style root-level tabs) --------------------

#[test]
fn test_add_terminal_tab_stacks_into_focused_panes_group() {
    let _flag = FeatureFlag::PaneLeafGroups.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            // Starts as a single terminal tab with a solo pane.
            assert_eq!(panes.terminal_tab_count(), 1);
            let first_pane = panes.focused_pane_id(ctx);

            // Ctrl+T lands the new terminal in the FOCUSED pane's leaf group —
            // NOT as a workspace-level tab ("Ctrl+T adds to the project, not the
            // focused pane" bug).
            let new_pane: PaneId = panes
                .add_terminal_tab(ctx)
                .expect("add_terminal_tab should create a terminal pane")
                .into();
            assert_eq!(
                panes.terminal_tab_count(),
                1,
                "no workspace-level tab may be created"
            );
            let members = panes
                .panes
                .leaf_group_members(first_pane)
                .expect("focused pane must be in a leaf group");
            assert_eq!(members, &[first_pane, new_pane]);
            // The new member is the group's active tab and holds focus.
            assert_eq!(panes.panes.leaf_group_active_pane(first_pane), Some(new_pane));
            assert_eq!(panes.focused_pane_id(ctx), new_pane);
        });
    });
}

#[test]
fn test_add_pane_as_tab_stacks_file_pane_into_focused_panes_group() {
    let _flag = FeatureFlag::PaneLeafGroups.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            assert_eq!(panes.terminal_tab_count(), 1);
            let first_pane = panes.focused_pane_id(ctx);

            // `EditorLayout::NewTab` for a file: the pane joins the FOCUSED
            // pane's leaf group as a browser-style tab, exactly like Ctrl+T does
            // for a terminal. Regression: it used to become a workspace-level
            // tab, which in Mnemos IDE reads as a brand-new *project*.
            let new_pane = panes
                .add_pane_as_tab(
                    Box::new(NotebookPane::new(new_notebook(ctx), ctx)),
                    true, /* focus_new_pane */
                    ctx,
                )
                .expect("add_pane_as_tab should attach the pane");
            assert_eq!(
                panes.terminal_tab_count(),
                1,
                "no root-level tab may be created"
            );
            let members = panes
                .panes
                .leaf_group_members(first_pane)
                .expect("focused pane must be in a leaf group");
            assert_eq!(members, &[first_pane, new_pane]);
            assert_eq!(panes.panes.leaf_group_active_pane(first_pane), Some(new_pane));
            assert_eq!(panes.focused_pane_id(ctx), new_pane);
        });
    });
}

#[test]
fn test_add_pane_as_tab_without_focus_keeps_focus_on_original_pane() {
    let _flag = FeatureFlag::PaneLeafGroups.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            let first_pane = panes.focused_pane_id(ctx);

            // Preview opens (focus_new_pane = false) must not steal focus.
            let new_pane = panes
                .add_pane_as_tab(
                    Box::new(NotebookPane::new(new_notebook(ctx), ctx)),
                    false, /* focus_new_pane */
                    ctx,
                )
                .expect("add_pane_as_tab should attach the pane");
            assert_eq!(panes.terminal_tab_count(), 1);
            assert_ne!(new_pane, first_pane);
            assert_eq!(panes.focused_pane_id(ctx), first_pane);
        });
    });
}

#[test]
fn test_activate_terminal_tab_switches_and_focuses_its_pane() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            let first_tab = panes.active_terminal_tab_id();
            let first_pane = panes.focused_pane_id(ctx);

            // Open a second tab (now active + focused on its own pane).
            let second_pane: PaneId = panes
                .add_terminal_workspace_tab(ctx)
                .expect("second tab")
                .into();
            let second_tab = panes.active_terminal_tab_id();
            assert_eq!(panes.focused_pane_id(ctx), second_pane);

            // Switching back to the first tab must change the active tab AND move
            // focus to that tab's pane (the non-negotiable focus-follows-tab rule).
            panes.activate_terminal_tab(first_tab, ctx);
            assert_eq!(panes.active_terminal_tab_id(), first_tab);
            assert_eq!(
                panes.focused_pane_id(ctx),
                first_pane,
                "activating a tab must focus that tab's pane"
            );

            // And forward again to the second tab.
            panes.activate_terminal_tab(second_tab, ctx);
            assert_eq!(panes.active_terminal_tab_id(), second_tab);
            assert_eq!(panes.focused_pane_id(ctx), second_pane);
        });
    });
}

#[test]
fn test_close_terminal_tab_via_action_removes_tab() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            let first_tab = panes.active_terminal_tab_id();
            let _second_pane = panes.add_terminal_workspace_tab(ctx).expect("second tab");
            let second_tab = panes.active_terminal_tab_id();
            assert_eq!(panes.terminal_tab_count(), 2);

            // Exercise the REAL close path through the action handler (the × button
            // dispatches exactly this action), not just a struct method. This is the
            // routing that the v1 swallowed-click bug broke.
            panes.handle_action(&PaneGroupAction::CloseTerminalTab(second_tab), ctx);

            assert_eq!(panes.terminal_tab_count(), 1, "closed tab must be removed");
            assert_eq!(
                panes.active_terminal_tab_id(),
                first_tab,
                "closing the active tab falls back to the remaining tab"
            );
            // The remaining tab's pane is focused and still live.
            let remaining_pane = panes.focused_pane_id(ctx);
            assert!(panes.has_pane_id(remaining_pane));
        });
    });
}

#[test]
fn test_close_last_terminal_tab_is_guarded() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            let only_tab = panes.active_terminal_tab_id();
            let only_pane = panes.focused_pane_id(ctx);
            assert_eq!(panes.terminal_tab_count(), 1);

            // Closing the final tab must NOT destroy it / leave an empty state.
            panes.handle_action(&PaneGroupAction::CloseTerminalTab(only_tab), ctx);

            assert_eq!(
                panes.terminal_tab_count(),
                1,
                "the last tab must never be removed"
            );
            assert_eq!(panes.active_terminal_tab_id(), only_tab);
            assert_eq!(panes.focused_pane_id(ctx), only_pane);
            assert!(panes.has_pane_id(only_pane));
        });
    });
}

#[test]
fn test_reorder_terminal_tab_via_action_moves_and_follows_active() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            // Build three tabs: [t0, t1, t2], active = t2 (the last added).
            let t0 = panes.active_terminal_tab_id();
            let _p1 = panes.add_terminal_workspace_tab(ctx).expect("tab 1");
            let t1 = panes.active_terminal_tab_id();
            let _p2 = panes.add_terminal_workspace_tab(ctx).expect("tab 2");
            let t2 = panes.active_terminal_tab_id();
            assert_eq!(panes.terminal_tab_ids(), vec![t0, t1, t2]);

            // Drag the first tab (t0) to the last slot via the real action path.
            panes.handle_action(
                &PaneGroupAction::ReorderTerminalTab {
                    id: t0,
                    new_index: 2,
                },
                ctx,
            );

            // Order changed and the moved tab is now active (focus follows it).
            assert_eq!(panes.terminal_tab_ids(), vec![t1, t2, t0]);
            assert_eq!(
                panes.active_terminal_tab_id(),
                t0,
                "the reordered tab must become active"
            );

            // Drag it back to the front.
            panes.handle_action(
                &PaneGroupAction::ReorderTerminalTab {
                    id: t0,
                    new_index: 0,
                },
                ctx,
            );
            assert_eq!(panes.terminal_tab_ids(), vec![t0, t1, t2]);
            assert_eq!(panes.active_terminal_tab_id(), t0);

            // Out-of-range index clamps to the last slot rather than panicking.
            panes.handle_action(
                &PaneGroupAction::ReorderTerminalTab {
                    id: t0,
                    new_index: 99,
                },
                ctx,
            );
            assert_eq!(panes.terminal_tab_ids(), vec![t1, t2, t0]);
        });
    });
}

#[test]
fn test_split_terminal_tab_into_pane_via_action_merges_and_removes_source() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            // Tab 0 holds the target pane; tab 1 is the one we drag onto it.
            let target_tab = panes.active_terminal_tab_id();
            let target_pane = panes.focused_pane_id(ctx);

            let source_pane: PaneId = panes.add_terminal_workspace_tab(ctx).expect("source tab").into();
            let source_tab = panes.active_terminal_tab_id();
            assert_eq!(panes.terminal_tab_count(), 2);

            // Drag the source tab onto the target pane's right edge → side-by-side split.
            panes.handle_action(
                &PaneGroupAction::SplitTerminalTabIntoPane {
                    tab_id: source_tab,
                    target_pane_id: target_pane,
                    direction: Some(Direction::Right),
                },
                ctx,
            );

            // Source tab is gone; only the target tab remains.
            assert_eq!(
                panes.terminal_tab_count(),
                1,
                "the source tab must be removed after merging into a split"
            );
            assert_eq!(panes.active_terminal_tab_id(), target_tab);

            // Both panes now live in the single remaining tab as a split.
            assert!(panes.has_pane_id(target_pane));
            assert!(
                panes.has_pane_id(source_pane),
                "the dragged tab's pane must survive, re-homed into the split"
            );
            // The moved pane is focused in its new home.
            assert_eq!(panes.focused_pane_id(ctx), source_pane);
        });
    });
}

#[test]
fn test_split_terminal_tab_onto_its_own_pane_redirects_to_neighbour_tab() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            // Tab 0 is the neighbour that stays; tab 1 is dragged out. Starting the
            // drag activates the dragged tab, so the pane under the drop point is the
            // dragged tab's OWN pane — the handler must redirect the split to the
            // neighbour tab instead of no-opping (the "drag tab back out" bug).
            let neighbour_pane = panes.focused_pane_id(ctx);

            let source_pane: PaneId = panes.add_terminal_workspace_tab(ctx).expect("source tab").into();
            let source_tab = panes.active_terminal_tab_id();
            assert_eq!(panes.terminal_tab_count(), 2);

            panes.handle_action(
                &PaneGroupAction::SplitTerminalTabIntoPane {
                    tab_id: source_tab,
                    target_pane_id: source_pane,
                    direction: Some(Direction::Right),
                },
                ctx,
            );

            assert_eq!(
                panes.terminal_tab_count(),
                1,
                "the dragged tab must merge into the neighbour tab, not no-op"
            );
            assert!(panes.has_pane_id(neighbour_pane));
            assert!(
                panes.has_pane_id(source_pane),
                "the dragged tab's pane must survive, re-homed into the split"
            );
            assert_eq!(panes.focused_pane_id(ctx), source_pane);
        });
    });
}

#[test]
fn test_split_terminal_tab_center_drop_merges_into_target_leaf_group() {
    let _flag = FeatureFlag::PaneLeafGroups.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            // Tab 0 holds the target pane; tab 1 is the one we drag onto it.
            let target_tab = panes.active_terminal_tab_id();
            let target_pane = panes.focused_pane_id(ctx);

            let source_pane: PaneId = panes.add_terminal_workspace_tab(ctx).expect("source tab").into();
            let source_tab = panes.active_terminal_tab_id();
            assert_eq!(panes.terminal_tab_count(), 2);

            // Drop the source tab on the target pane's CENTER zone → the tab's
            // pane becomes a member of the target's leaf group (per-pane tab),
            // exactly what the drag preview's center overlay promised.
            panes.handle_action(
                &PaneGroupAction::SplitTerminalTabIntoPane {
                    tab_id: source_tab,
                    target_pane_id: target_pane,
                    direction: None,
                },
                ctx,
            );

            assert_eq!(
                panes.terminal_tab_count(),
                1,
                "the source tab must be removed after merging into the group"
            );
            assert_eq!(panes.active_terminal_tab_id(), target_tab);
            assert_eq!(
                panes.panes.leaf_group_members(target_pane),
                Some([target_pane, source_pane].as_slice()),
                "the dragged tab's pane must land as a tab of the target's group"
            );
            assert_eq!(
                panes.panes.visible_pane_ids(),
                vec![source_pane],
                "the merged pane becomes the group's active (visible) member"
            );
            assert_eq!(panes.focused_pane_id(ctx), source_pane);
        });
    });
}

#[test]
fn test_split_terminal_tab_center_drop_on_own_pane_merges_into_neighbour_group() {
    let _flag = FeatureFlag::PaneLeafGroups.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            // As with the edge case above: starting the drag activates the
            // dragged tab, so a center drop usually lands on the dragged tab's
            // OWN pane. The handler must redirect to the neighbour tab's pane
            // and merge there instead of no-opping.
            let neighbour_pane = panes.focused_pane_id(ctx);

            let source_pane: PaneId = panes.add_terminal_workspace_tab(ctx).expect("source tab").into();
            let source_tab = panes.active_terminal_tab_id();
            assert_eq!(panes.terminal_tab_count(), 2);

            panes.handle_action(
                &PaneGroupAction::SplitTerminalTabIntoPane {
                    tab_id: source_tab,
                    target_pane_id: source_pane,
                    direction: None,
                },
                ctx,
            );

            assert_eq!(
                panes.terminal_tab_count(),
                1,
                "the dragged tab must merge into the neighbour tab, not no-op"
            );
            assert_eq!(
                panes.panes.leaf_group_members(neighbour_pane),
                Some([neighbour_pane, source_pane].as_slice()),
                "the redirect must merge the pane into the neighbour's group"
            );
            assert_eq!(panes.focused_pane_id(ctx), source_pane);
        });
    });
}

#[test]
fn test_toggle_maximize_pane_is_disabled() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            // Two panes so the old guard (`pane_count() > 1`) would have allowed
            // maximizing — the action must still be a no-op: a fullscreen pane
            // hides every other pane's geometry and breaks drag drop-zones.
            panes.add_terminal_workspace_tab(ctx).expect("second pane");
            assert!(panes.pane_count() > 1);

            panes.handle_action(&PaneGroupAction::ToggleMaximizePane, ctx);
            assert!(
                !panes.is_focused_pane_maximized(ctx),
                "pane maximize is disabled — a pane can only ever be a pane"
            );
        });
    });
}

#[test]
fn test_split_last_terminal_tab_is_guarded() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            let only_tab = panes.active_terminal_tab_id();
            let only_pane = panes.focused_pane_id(ctx);
            assert_eq!(panes.terminal_tab_count(), 1);

            // Dragging the only tab into a split would empty the container — no-op.
            panes.handle_action(
                &PaneGroupAction::SplitTerminalTabIntoPane {
                    tab_id: only_tab,
                    target_pane_id: only_pane,
                    direction: Some(Direction::Right),
                },
                ctx,
            );

            assert_eq!(panes.terminal_tab_count(), 1);
            assert_eq!(panes.active_terminal_tab_id(), only_tab);
            assert!(panes.has_pane_id(only_pane));
        });
    });
}

#[test]
fn test_detach_last_terminal_tab_is_guarded() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            // Detaching the only tab would empty the container — the handler must
            // guard and return before touching panes or the window path. (The
            // multi-tab detach happy-path creates a real OS window, so it's covered
            // by integration review, not this headless unit test.)
            let only_tab = panes.active_terminal_tab_id();
            let only_pane = panes.focused_pane_id(ctx);
            assert_eq!(panes.terminal_tab_count(), 1);

            panes.handle_action(
                &PaneGroupAction::DetachTerminalTabToWindow {
                    tab_id: only_tab,
                    screen_position: pathfinder_geometry::vector::Vector2F::zero(),
                },
                ctx,
            );

            assert_eq!(panes.terminal_tab_count(), 1, "the last tab must never detach");
            assert_eq!(panes.active_terminal_tab_id(), only_tab);
            assert!(panes.has_pane_id(only_pane));
        });
    });
}

// --- Leaf-group (per-pane tab) member actions ----------------------------------

/// Two terminals split side-by-side, then center-drop merged into one leaf
/// group [first, second] with `second` the active member. Returns
/// `(first, second)`.
fn build_two_member_group(
    panes: &mut PaneGroup,
    ctx: &mut ViewContext<PaneGroup>,
) -> (PaneId, PaneId) {
    let first = get_newly_created_pane_id(panes, &[]);
    panes.add_terminal_pane(Direction::Right, None, ctx);
    let second = get_newly_created_pane_id(panes, &[first]);
    // The center-drop merge (the strip's raison d'être).
    panes.merge_pane_into_tab_group(second, first, ctx);
    assert_eq!(
        panes.panes.visible_pane_ids(),
        vec![second],
        "merged pane becomes the active (visible) member"
    );
    assert_eq!(
        panes.focused_pane_id(ctx),
        second,
        "merged pane takes focus"
    );
    (first, second)
}

#[test]
fn test_activate_leaf_group_member_action_switches_active_and_focus() {
    let _flag = FeatureFlag::PaneLeafGroups.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            let (first, second) = build_two_member_group(panes, ctx);

            // Exercise the REAL strip-click path through the action handler.
            panes.handle_action(
                &PaneGroupAction::ActivateLeafGroupMember { pane_id: first },
                ctx,
            );
            assert_eq!(
                panes.panes.visible_pane_ids(),
                vec![first],
                "activation must swap the visible member"
            );
            assert_eq!(
                panes.focused_pane_id(ctx),
                first,
                "activation must move focus to the raised member"
            );
            // The inactive member stays alive (its content is retained).
            assert!(panes.has_pane_id(second));

            // Both members qualify for the header ×-predicate (closable even
            // though the group is the only visible pane).
            let focus_state = panes.focus_state_handle().as_ref(ctx);
            assert!(focus_state.is_in_multi_member_group(first));
            assert!(focus_state.is_in_multi_member_group(second));
        });
    });
}

#[test]
fn test_close_leaf_group_member_action_removes_member_and_activates_survivor() {
    let _flag = FeatureFlag::PaneLeafGroups.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            let (first, second) = build_two_member_group(panes, ctx);

            // Close the ACTIVE member via the strip's × action. Must NOT take
            // the "last visible pane → exit" path even though the group is the
            // only visible pane.
            panes.handle_action(
                &PaneGroupAction::CloseLeafGroupMember { pane_id: second },
                ctx,
            );

            assert_eq!(
                panes.panes.visible_pane_ids(),
                vec![first],
                "the surviving member is revealed"
            );
            assert_eq!(
                panes.focused_pane_id(ctx),
                first,
                "focus falls back to the survivor"
            );
            assert!(!panes.has_pane_id(second), "closed member is torn down");
            assert_eq!(
                panes.panes.leaf_group_members(first),
                Some([first].as_slice()),
                "the group is back to a single member"
            );
        });
    });
}

#[test]
fn test_close_leaf_group_member_with_undo_hides_member_and_reveals_survivor() {
    let _flag = FeatureFlag::PaneLeafGroups.override_enabled(true);
    let _undo = FeatureFlag::UndoClosedPanes.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            let (first, second) = build_two_member_group(panes, ctx);

            panes.handle_action(
                &PaneGroupAction::CloseLeafGroupMember { pane_id: second },
                ctx,
            );

            // Undo-close: the member is hidden (still a group member, still
            // alive) while activation hands off to the survivor.
            assert_eq!(panes.panes.visible_pane_ids(), vec![first]);
            assert_eq!(panes.focused_pane_id(ctx), first);
            assert!(
                panes.has_pane_id(second),
                "undo-close keeps the pane alive during the grace period"
            );
            assert!(panes.panes.is_pane_hidden(&second));
            assert_eq!(
                panes.panes.leaf_group_members(first),
                Some([first, second].as_slice()),
                "membership survives until the grace period expires"
            );

            // Grace-period expiry strips the membership for good.
            assert!(panes.cleanup_closed_pane(second, ctx));
            assert!(!panes.has_pane_id(second));
            assert_eq!(
                panes.panes.leaf_group_members(first),
                Some([first].as_slice())
            );
            assert_eq!(panes.panes.visible_pane_ids(), vec![first]);
        });
    });
}

/// Build a leaf group `[first, second]` (active `second`) plus a separate
/// plain pane `outside`, laid out `[group(first, second) | outside]`. Returns
/// `(first, second, outside)`.
fn build_group_plus_outside_pane(
    panes: &mut PaneGroup,
    ctx: &mut ViewContext<PaneGroup>,
) -> (PaneId, PaneId, PaneId) {
    let first = get_newly_created_pane_id(panes, &[]);
    panes.add_terminal_pane(Direction::Right, None, ctx);
    let second = get_newly_created_pane_id(panes, &[first]);
    panes.add_terminal_pane(Direction::Right, None, ctx);
    let outside = get_newly_created_pane_id(panes, &[first, second]);
    panes.merge_pane_into_tab_group(second, first, ctx);
    assert_eq!(
        panes.panes.leaf_group_members(first),
        Some([first, second].as_slice()),
        "first + second form the leaf group"
    );
    assert_eq!(
        panes.panes.leaf_group_members(outside),
        Some([outside].as_slice()),
        "outside stays a plain single-member leaf"
    );
    (first, second, outside)
}

#[test]
fn test_move_leaf_group_member_edge_drop_outside_splits_member_out() {
    let _flag = FeatureFlag::PaneLeafGroups.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            let (first, second, outside) = build_group_plus_outside_pane(panes, ctx);

            // Edge-drop the active member `second` onto a pane OUTSIDE its group.
            panes.handle_action(
                &PaneGroupAction::MoveLeafGroupMember {
                    member: second,
                    target_pane_id: outside,
                    direction: Some(Direction::Right),
                },
                ctx,
            );

            // The survivor keeps its frame as a single-member leaf.
            assert_eq!(
                panes.panes.leaf_group_members(first),
                Some([first].as_slice()),
                "the survivor is left intact"
            );
            // The dragged member left the group and is now its own pane.
            assert_eq!(
                panes.panes.leaf_group_members(second),
                Some([second].as_slice()),
                "the member popped out of the group"
            );
            assert!(panes.has_pane_id(second));
            assert!(
                panes.panes.visible_pane_ids().contains(&second),
                "the member is now visible beside the target"
            );
            assert_eq!(
                panes.focused_pane_id(ctx),
                second,
                "focus follows the moved member"
            );
        });
    });
}

#[test]
fn test_move_leaf_group_member_center_drop_merges_into_other_group() {
    let _flag = FeatureFlag::PaneLeafGroups.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            let (first, second, outside) = build_group_plus_outside_pane(panes, ctx);

            // Center-drop the active member `second` onto the separate pane.
            panes.handle_action(
                &PaneGroupAction::MoveLeafGroupMember {
                    member: second,
                    target_pane_id: outside,
                    direction: None,
                },
                ctx,
            );

            // `second` leaves the first group and becomes a tab in `outside`'s.
            assert_eq!(
                panes.panes.leaf_group_members(outside),
                Some([outside, second].as_slice()),
                "the member merged in as a tab of the target group"
            );
            assert_eq!(
                panes.panes.leaf_group_members(first),
                Some([first].as_slice()),
                "the source group is back to a single member"
            );
            assert_eq!(
                panes.focused_pane_id(ctx),
                second,
                "the merged member takes focus"
            );
        });
    });
}

#[test]
fn test_move_leaf_group_member_edge_drop_self_splits_out_beside_group() {
    let _flag = FeatureFlag::PaneLeafGroups.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            let (first, second) = build_two_member_group(panes, ctx);

            // Edge-drop the member onto its OWN pane (the common self-case:
            // pressing a strip tab activates it, so the pane under the drop is
            // the member's own). It must retarget to the sibling and split out.
            panes.handle_action(
                &PaneGroupAction::MoveLeafGroupMember {
                    member: second,
                    target_pane_id: second,
                    direction: Some(Direction::Right),
                },
                ctx,
            );

            assert_eq!(
                panes.panes.leaf_group_members(first),
                Some([first].as_slice()),
                "the group collapses to its survivor"
            );
            assert_eq!(
                panes.panes.leaf_group_members(second),
                Some([second].as_slice()),
                "the member split out into its own pane"
            );
            assert!(panes.has_pane_id(second));
            let visible = panes.panes.visible_pane_ids();
            assert!(
                visible.contains(&first) && visible.contains(&second),
                "both the survivor and the split-out member are visible now"
            );
            assert_eq!(
                panes.focused_pane_id(ctx),
                second,
                "focus follows the split-out member"
            );
        });
    });
}

#[test]
fn test_move_leaf_group_member_center_drop_on_own_group_is_noop() {
    let _flag = FeatureFlag::PaneLeafGroups.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            let (first, second) = build_two_member_group(panes, ctx);

            // Center-drop onto a sibling in the member's OWN group → no-op.
            panes.handle_action(
                &PaneGroupAction::MoveLeafGroupMember {
                    member: second,
                    target_pane_id: first,
                    direction: None,
                },
                ctx,
            );

            assert_eq!(
                panes.panes.leaf_group_members(second),
                Some([first, second].as_slice()),
                "membership is unchanged"
            );
            assert_eq!(
                panes.panes.visible_pane_ids(),
                vec![second],
                "the active member is unchanged"
            );
            assert_eq!(
                panes.focused_pane_id(ctx),
                second,
                "focus is unchanged"
            );
        });
    });
}

#[test]
fn test_insert_hidden_child_agent_pane_keeps_focus_and_active_session() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            let parent_pane_id = get_newly_created_pane_id(panes, &[]);
            let initial_pane_count = panes.pane_count();
            let initial_visible_count = panes.visible_pane_count();
            let initial_active_session = panes.active_session_id(ctx);

            let child_pane_id = panes.insert_terminal_pane_hidden_for_child_agent(
                parent_pane_id,
                HashMap::new(),
                ctx,
            );

            assert_eq!(panes.pane_count(), initial_pane_count + 1);
            assert_eq!(panes.visible_pane_count(), initial_visible_count);
            assert!(panes.has_pane_id(child_pane_id.into()));

            // The new child pane should remain hidden and not affect visible ordering.
            assert_eq!(panes.pane_id_by_index(0), Some(parent_pane_id));
            assert_eq!(panes.pane_id_by_index(1), None);

            // Creating a hidden child pane should not steal focus or active session.
            assert_eq!(panes.focused_pane_id(ctx), parent_pane_id);
            assert_eq!(panes.active_session_id(ctx), initial_active_session);
        });
    });
}

#[test]
fn test_active_session_id_reset_on_last_pane_close() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            let terminal_id = get_newly_created_pane_id(panes, &[]);
            assert_eq!(
                panes.active_session_id(ctx),
                terminal_id.as_terminal_pane_id()
            );

            // Add a non-terminal pane (Notebook) so the pane group remains alive when terminal is closed.
            panes.add_pane_with_direction(
                Direction::Right,
                NotebookPane::new(new_notebook(ctx), ctx),
                false, /* focus_new_pane */
                ctx,
            );

            // Close the terminal.
            panes.close_pane(terminal_id, ctx);

            // active_session_id should be None after closing the last pane.
            assert_eq!(
                panes.active_session_id(ctx),
                None,
                "active_session_id should be None after closing the last pane"
            );
        });
    });
}

#[test]
fn test_add_pane_aborts_cleanly_when_pre_attach_returns_false() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            let before_snapshot = panes.snapshot(ctx);
            let before_count = panes.pane_count();

            panes.add_pane_with_direction(
                Direction::Right,
                PreAttachReturnsFalsePane::new(ctx),
                true, /* focus_new_pane */
                ctx,
            );

            assert_eq!(panes.pane_count(), before_count);
            assert_eq!(panes.snapshot(ctx), before_snapshot);
        });
    });
}

#[test]
fn test_focus_notebook() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            let first_terminal_id = get_newly_created_pane_id(panes, &[]);

            // Add a notebook to the left.
            panes.add_pane_with_direction(
                Direction::Left,
                NotebookPane::new(new_notebook(ctx), ctx),
                true, /* focus_new_pane */
                ctx,
            );
            let notebook_id = get_newly_created_pane_id(panes, &[first_terminal_id]);

            // The new pane should be focused, but the terminal is still the active session.
            assert_eq!(panes.focused_pane_id(ctx), notebook_id);
            assert_eq!(
                panes.active_session_id(ctx).map(Into::into),
                Some(first_terminal_id)
            );
            assert_eq!(
                split_pane_state(panes, first_terminal_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Unfocused)
            );
            assert!(is_active_session(panes, first_terminal_id, ctx));
            assert_eq!(
                split_pane_state(panes, notebook_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Focused)
            );

            // Add a terminal below.
            panes.add_terminal_pane(Direction::Down, None, ctx);
            let second_terminal_id =
                get_newly_created_pane_id(panes, &[first_terminal_id, notebook_id]);

            // The new terminal should be both focused and the active session.
            assert_eq!(panes.focused_pane_id(ctx), second_terminal_id);
            assert_eq!(
                panes.active_session_id(ctx).map(Into::into),
                Some(second_terminal_id)
            );
            assert_eq!(
                split_pane_state(panes, first_terminal_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Unfocused)
            );
            assert!(!is_active_session(panes, first_terminal_id, ctx));
            assert_eq!(
                split_pane_state(panes, second_terminal_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Focused)
            );
            assert!(is_active_session(panes, second_terminal_id, ctx));
            assert_eq!(
                split_pane_state(panes, notebook_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Unfocused)
            );

            // Close the new terminal. Focus should switch to the notebook, and the first terminal
            // session will activate.
            panes.close_pane(second_terminal_id, ctx);
            assert_eq!(panes.focused_pane_id(ctx), notebook_id);
            assert_eq!(
                panes.active_session_id(ctx).map(Into::into),
                Some(first_terminal_id)
            );
            assert_eq!(
                split_pane_state(panes, first_terminal_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Unfocused)
            );
            assert_eq!(
                split_pane_state(panes, notebook_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Focused)
            );
            assert!(is_active_session(panes, first_terminal_id, ctx));
        })
    });
}

#[test]
fn test_group_without_terminals() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            let terminal_id = get_newly_created_pane_id(panes, &[]);

            // Add a notebook to the left.
            panes.add_pane_with_direction(
                Direction::Left,
                NotebookPane::new(new_notebook(ctx), ctx),
                true, /* focus_new_pane */
                ctx,
            );
            let notebook_id = get_newly_created_pane_id(panes, &[terminal_id]);

            // Close the terminal, which should leave the group without an active session.
            panes.close_pane(terminal_id, ctx);
            assert_eq!(panes.focused_pane_id(ctx), notebook_id);
            assert_eq!(panes.active_session_id(ctx), None);
            assert_eq!(
                split_pane_state(panes, notebook_id, ctx),
                SplitPaneState::NotInSplitPane
            );
        });
    });
}

#[test]
fn test_close_active_session() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            // Add two terminal sessions.
            let first_terminal_id = get_newly_created_pane_id(panes, &[]);
            panes.add_terminal_pane(Direction::Up, None, ctx);
            let second_terminal_id = get_newly_created_pane_id(panes, &[first_terminal_id]);

            // Add a notebook to the left.
            panes.add_pane_with_direction(
                Direction::Left,
                NotebookPane::new(new_notebook(ctx), ctx),
                true, /* focus_new_pane */
                ctx,
            );
            let notebook_id =
                get_newly_created_pane_id(panes, &[first_terminal_id, second_terminal_id]);
            assert_eq!(panes.focused_pane_id(ctx), notebook_id);
            assert_eq!(
                panes.active_session_id(ctx).map(Into::into),
                Some(second_terminal_id)
            );

            // Close the active session, which should leave the notebook focused and activate the
            // remaining session.
            panes.close_pane(second_terminal_id, ctx);
            assert_eq!(panes.focused_pane_id(ctx), notebook_id);
            assert_eq!(
                panes.active_session_id(ctx).map(Into::into),
                Some(first_terminal_id)
            );
            assert_eq!(
                split_pane_state(panes, first_terminal_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Unfocused)
            );
            assert!(is_active_session(panes, first_terminal_id, ctx));

            // Now, focus the remaining session, which should keep it activated.
            panes.focus_pane_by_id(first_terminal_id, ctx);
            assert_eq!(panes.focused_pane_id(ctx), first_terminal_id);
            assert_eq!(
                panes.active_session_id(ctx).map(Into::into),
                Some(first_terminal_id)
            );
            assert_eq!(
                split_pane_state(panes, first_terminal_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Focused)
            );
            assert_eq!(
                split_pane_state(panes, notebook_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Unfocused)
            );
            assert!(is_active_session(panes, first_terminal_id, ctx));
        });
    });
}

#[test]
fn test_update_session_visibility() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let pane_group = mock_pane_group(&mut app, Default::default());
        pane_group.update(&mut app, |panes, ctx| {
            // Assert that there is no active window.
            WindowManager::handle(ctx).read(ctx, |state, _| {
                assert_eq!(state.stage(), ApplicationStage::Starting);
                assert!(state.active_window().is_none());
            });

            fn visibility_matches(panes: &PaneGroup, expected: bool, ctx: &ViewContext<PaneGroup>) {
                for data in panes.panes_of::<TerminalPane>() {
                    let view = data.terminal_view(ctx).as_ref(ctx);
                    assert_eq!(
                        view.was_ever_visible(),
                        expected,
                        "View {} visibility was {}, expected {}",
                        data.terminal_view(ctx).id(),
                        view.was_ever_visible(),
                        expected
                    );
                }
            }

            // Add pane Left.
            panes.add_terminal_pane(Direction::Left, None, ctx);

            // Assert that neither of the panes are marked as visible (due
            // to the fact that the window is not active).
            visibility_matches(panes, false, ctx);

            let window_id = ctx.window_id();
            WindowManager::handle(ctx).update(ctx, |state, ctx| {
                state.overwrite_for_test(ApplicationStage::Active, Some(window_id));
                ctx.notify();
            });

            // Assert that both of the panes are still not marked as
            // visible, given the fact that the pane group is not focused.
            visibility_matches(panes, false, ctx);

            panes.focus(ctx);

            // Assert that both of the panes are now visible.
            visibility_matches(panes, true, ctx);
        })
    });
}

#[test]
fn test_initial_widths_are_computed_correctly() {
    use launch_config::PaneTemplateType::*;

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        // Define a simple macro to help us create new leaf panes.
        macro_rules! leaf_pane {
            () => {
                PaneTemplate {
                    is_focused: None,
                    cwd: "".into(),
                    commands: vec![],
                    pane_mode: PaneMode::Terminal,
                    shell: None,
                }
            };
        }

        // Pick an arbitrary initial window that isn't the same as the
        // fallback value.
        let window_width = 864.;
        let window_height = 636.;
        assert_ne!(window_width, FALLBACK_INITIAL_WINDOW_SIZE.x());
        assert_ne!(window_height, FALLBACK_INITIAL_WINDOW_SIZE.y());

        // Create a template that looks like the following, with each pane
        // numbered by its index in the pane group:
        //
        //  ---------------------
        //  |         0         |
        //  | __________________|
        //  |     1   |____2____|
        //  | ________|____3____|
        //  |   4  |   5  |  6  |
        //  |      |      |     |
        //  ---------------------
        let template = PaneBranchTemplate {
            split_direction: launch_config::SplitDirection::Vertical,
            panes: vec![
                leaf_pane!(),
                PaneBranchTemplate {
                    split_direction: launch_config::SplitDirection::Horizontal,
                    panes: vec![
                        leaf_pane!(),
                        PaneBranchTemplate {
                            split_direction: launch_config::SplitDirection::Vertical,
                            panes: vec![leaf_pane!(), leaf_pane!()],
                        },
                    ],
                },
                PaneBranchTemplate {
                    split_direction: launch_config::SplitDirection::Horizontal,
                    panes: vec![leaf_pane!(), leaf_pane!(), leaf_pane!()],
                },
            ],
        };

        let window_size = Vector2F::new(window_width, window_height);
        let pane_group = mock_pane_group(
            &mut app,
            MockOptions {
                layout: PanesLayout::Template(template),
                window_bounds: WindowBounds::ExactPosition(RectF::new(
                    Vector2F::zero(),
                    window_size,
                )),
            },
        );

        // Assert that the window created by the call to `mock_pane_group`
        // has the expected bounds.
        let window_id = app.read(|ctx| pane_group.window_id(ctx));
        app.update(|ctx| {
            assert_eq!(
                Some(window_size),
                ctx.window_bounds(&window_id).map(|rect| rect.size())
            );
        });

        let pane_group_width = window_width - 2.0 * workspace::WORKSPACE_PADDING;
        let pane_group_height =
            window_height - workspace::TOTAL_TAB_BAR_HEIGHT - 2.0 * workspace::WORKSPACE_PADDING;

        pane_group.read(&app, |pane_group, ctx| {
            // Make assertions about the expected widths of the various
            // panes.
            assert_eq!(
                pane_group
                    .terminal_view_at_pane_index(0, ctx)
                    .unwrap()
                    .as_ref(ctx)
                    .size_info()
                    .pane_width_px()
                    .as_f32(),
                pane_group_width,
                "Pane with index 0 had unexpected width!"
            );
            let half_width = (pane_group_width - tree::get_divider_thickness()) / 2.;
            for i in 1..=3 {
                assert_eq!(
                    pane_group
                        .terminal_view_at_pane_index(i, ctx)
                        .unwrap()
                        .as_ref(ctx)
                        .size_info()
                        .pane_width_px()
                        .as_f32(),
                    half_width,
                    "Pane with index {i} had unexpected width!"
                );
            }
            let one_third_width = (pane_group_width - (2. * tree::get_divider_thickness())) / 3.;
            for i in 4..=6 {
                assert_eq!(
                    pane_group
                        .terminal_view_at_pane_index(i, ctx)
                        .unwrap()
                        .as_ref(ctx)
                        .size_info()
                        .pane_width_px()
                        .as_f32(),
                    one_third_width,
                    "Pane with index {i} had unexpected width!"
                );
            }

            // Make assertions about the expected heights of the various
            // panes.
            let one_third_height = (pane_group_height - (2. * tree::get_divider_thickness())) / 3.;
            for i in (0..=1).chain(4..=6) {
                assert_eq!(
                    pane_group
                        .terminal_view_at_pane_index(i, ctx)
                        .unwrap()
                        .as_ref(ctx)
                        .size_info()
                        .pane_height_px()
                        .as_f32(),
                    one_third_height,
                    "Pane with index {i} had unexpected height!"
                );
            }
            let one_sixth_height = (pane_group_height - (5. * tree::get_divider_thickness())) / 6.;
            for i in 2..=3 {
                assert_eq!(
                    pane_group
                        .terminal_view_at_pane_index(i, ctx)
                        .unwrap()
                        .as_ref(ctx)
                        .size_info()
                        .pane_height_px()
                        .as_f32(),
                    one_sixth_height,
                    "Pane with index {i} had unexpected height!"
                );
            }
        });
    });
}

#[test]
fn test_is_terminal_pane_being_shared() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let pane_group = mock_pane_group(&mut app, Default::default());
        pane_group.update(&mut app, |panes, ctx| {
            assert!(!panes.is_terminal_pane_being_shared(ctx));

            // Add another pane; the pane group should still be "unshared".
            panes.add_terminal_pane(Direction::Left, None, ctx);
            assert!(!panes.is_terminal_pane_being_shared(ctx));

            // Make one of the terminal panes shared. There is now at least one terminal pane being shared.
            panes
                .terminal_session_by_pane_index(0)
                .expect("terminal pane exists")
                .terminal_manager(ctx)
                .as_ref(ctx)
                .model()
                .lock()
                .set_shared_session_status(SharedSessionStatus::ActiveSharer);
            assert!(panes.is_terminal_pane_being_shared(ctx));
        });
    });
}

#[test]
fn test_number_of_shared_panes() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            // We have two terminal sessions. Neither is shared
            let first_pane_id = get_newly_created_pane_id(panes, &[]);
            panes.add_terminal_pane(Direction::Up, None, ctx);
            assert_eq!(panes.number_of_shared_sessions(ctx), 0);

            // Make one pane shared
            panes
                .terminal_manager(0, ctx)
                .unwrap()
                .as_ref(ctx)
                .model()
                .lock()
                .set_shared_session_status(SharedSessionStatus::ActiveSharer);
            assert_eq!(panes.number_of_shared_sessions(ctx), 1);

            // Make both panes shared
            panes
                .terminal_manager(1, ctx)
                .unwrap()
                .as_ref(ctx)
                .model()
                .lock()
                .set_shared_session_status(SharedSessionStatus::ActiveSharer);
            assert_eq!(panes.number_of_shared_sessions(ctx), 2);

            // Close a pane
            panes.close_pane(first_pane_id, ctx);
            assert_eq!(panes.number_of_shared_sessions(ctx), 1);
        });
    });
}

#[test]
fn test_start_shared_session_from_modal() {
    let _guard = FeatureFlag::CreatingSharedSessions.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |pane_group, ctx| {
            let terminal_pane = pane_group.terminal_session_by_pane_index(0).unwrap();
            let terminal_pane_id = terminal_pane.terminal_pane_id();
            let terminal_model = terminal_pane.terminal_manager(ctx).as_ref(ctx).model();

            assert!(matches!(
                terminal_model.lock().shared_session_status(),
                SharedSessionStatus::NotShared
            ));

            pane_group.open_share_session_modal(
                terminal_pane_id,
                SharedSessionActionSource::PaneHeader,
                ctx,
            );
            assert!(pane_group.terminal_with_open_share_session_modal.is_some());
            assert_eq!(
                pane_group
                    .share_session_modal
                    .as_ref(ctx)
                    .terminal_pane_id(),
                Some(terminal_pane_id)
            );

            pane_group.handle_share_session_modal_event(
                &ShareSessionModalEvent::StartSharing {
                    terminal_pane_id,
                    scrollback_type: SharedSessionScrollbackType::None,
                    source: SharedSessionActionSource::PaneHeader,
                },
                ctx,
            );
            assert!(pane_group.terminal_with_open_share_session_modal.is_none());
            assert!(matches!(
                terminal_model.lock().shared_session_status(),
                SharedSessionStatus::SharePending
            ));
        });

        // Wait for one tick of the event loop for the share to be started.
        pane_group.read(&app, |pane_group, ctx| {
            let terminal_view = pane_group
                .terminal_view_at_pane_index(0, ctx)
                .unwrap()
                .to_owned();
            let model = terminal_view.as_ref(ctx).model.lock();
            assert!(matches!(
                model.shared_session_status(),
                SharedSessionStatus::ActiveSharer
            ));

            let manager = shared_session::manager::Manager::as_ref(ctx);
            let shared_views = manager.shared_views(ctx).collect_vec();
            assert_eq!(shared_views.len(), 1);
            assert_eq!(shared_views[0].id(), terminal_view.id());

            let terminal_pane = pane_group.terminal_session_by_pane_index(0).unwrap();
            assert!(terminal_pane
                .pane_view()
                .as_ref(ctx)
                .header()
                .as_ref(ctx)
                .has_shareable_object(ctx));
        });
    });
}

/// TODO: look into moving this test somewhere more suitable.
/// Currently, the pane group is responsible for creating and owning
/// the terminal manager, which in turn owns the Network model for the share.
#[test]
fn test_stop_shared_session() {
    let _guard = FeatureFlag::CreatingSharedSessions.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        // Start the shared session.
        pane_group.update(&mut app, |pane_group, ctx| {
            let terminal_pane = pane_group.terminal_session_by_pane_index(0).unwrap();
            terminal_pane
                .terminal_manager(ctx)
                .update(ctx, |terminal_manager, ctx| {
                    let terminal_view = terminal_manager.view();
                    terminal_view.update(ctx, |terminal_view, ctx| {
                        terminal_view.attempt_to_share_session(
                            SharedSessionScrollbackType::None,
                            None,
                            SessionSourceType::default(),
                            false,
                            ctx,
                        );
                    });
                })
        });

        // Wait for one tick of the event loop for the share to be started.
        pane_group.read(&app, |pane_group, ctx| {
            let terminal_model = pane_group
                .terminal_session_by_pane_index(0)
                .unwrap()
                .to_owned()
                .terminal_manager(ctx)
                .as_ref(ctx)
                .model();
            assert!(matches!(
                terminal_model.lock().shared_session_status(),
                SharedSessionStatus::ActiveSharer
            ));
        });

        // Stop the shared session.
        pane_group.update(&mut app, |pane_group, ctx| {
            let terminal_pane = pane_group.terminal_session_by_pane_index(0).unwrap();
            terminal_pane
                .terminal_manager(ctx)
                .update(ctx, |terminal_manager, ctx| {
                    let terminal_view = terminal_manager.view();
                    terminal_view.update(ctx, |terminal_view, ctx| {
                        terminal_view
                            .stop_sharing_session(SharedSessionActionSource::PaneHeader, ctx);
                    });
                });
        });

        // Ensure the state is correct after stopping.
        pane_group.update(&mut app, |pane_group, ctx| {
            let terminal_pane = pane_group.terminal_session_by_pane_index(0).unwrap();
            let terminal_manager = terminal_pane
                .terminal_manager(ctx)
                .as_ref(ctx)
                .as_any()
                .downcast_ref::<TerminalManager>()
                .unwrap();
            let terminal_model = terminal_pane.terminal_manager(ctx).as_ref(ctx).model();

            assert!(terminal_manager.session_sharer().borrow().is_none());
            assert!(matches!(
                terminal_model.lock().shared_session_status(),
                SharedSessionStatus::NotShared
            ));

            let manager = shared_session::manager::Manager::as_ref(ctx);
            let shared_views = manager.shared_views(ctx).collect_vec();
            assert!(shared_views.is_empty());

            assert!(!terminal_pane
                .pane_view()
                .as_ref(ctx)
                .header()
                .as_ref(ctx)
                .has_shareable_object(ctx));
        });
    });
}

#[test]
fn test_navigation_skips_hidden_closed_panes() {
    let _guard = FeatureFlag::UndoClosedPanes.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        pane_group.update(&mut app, |panes, ctx| {
            // Add second terminal to the right to create a horizontal pair
            panes.add_terminal_pane(Direction::Right, None, ctx);

            // Add third terminal; place it to the right of current focus
            panes.add_terminal_pane(Direction::Right, None, ctx);

            // Determine ordered visible panes by index 0..2
            let a = panes.pane_id_by_index(0).expect("pane 0 exists");
            let b = panes.pane_id_by_index(1).expect("pane 1 exists");
            let c = panes.pane_id_by_index(2).expect("pane 2 exists");

            // Focus C and confirm prev would be B when all are visible
            panes.focus_pane_by_id(c, ctx);
            assert_eq!(panes.prev_pane_id_navigation(c), Some(b));

            // Close B (it will be hidden for undo and excluded from visible navigation)
            panes.close_pane(b, ctx);

            // Now prev from C should skip B and go to A
            assert_eq!(panes.prev_pane_id_navigation(c), Some(a));

            // And next from A should skip B and go to C
            assert_eq!(panes.next_pane_id(a), Some(c));
        })
    });
}

// Ensures that we always show the pane header for terminal panes, regardless of split state.
#[test]
fn test_terminal_pane_headers() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let pane_group = mock_pane_group(&mut app, Default::default());

        // There should be a single terminal pane to start and the pane header should not be shown.
        pane_group.read(&app, |pane_group, ctx| {
            assert_eq!(pane_group.pane_contents.len(), 1);

            let terminal_panes = pane_group.panes_of::<TerminalPane>().collect_vec();
            assert_eq!(terminal_panes.len(), 1);

            let pane_view = terminal_panes[0].pane_view();
            let header_visible = pane_view
                .as_ref(ctx)
                .header()
                .as_ref(ctx)
                .is_visible_in_pane_group();
            assert!(header_visible);
        });

        // Create a terminal split pane.
        pane_group.update(&mut app, |pane_group, ctx| {
            pane_group.add_terminal_pane(Direction::Left, None, ctx);
        });

        // There should be two terminal panes and they should both have the pane header.
        pane_group.read(&app, |pane_group, ctx| {
            assert_eq!(pane_group.pane_contents.len(), 2);

            let terminal_panes = pane_group.panes_of::<TerminalPane>().collect_vec();
            assert_eq!(terminal_panes.len(), 2);

            for terminal_pane in terminal_panes {
                let pane_view = terminal_pane.pane_view();
                assert!(pane_view
                    .as_ref(ctx)
                    .header()
                    .as_ref(ctx)
                    .is_visible_in_pane_group());
            }
        });

        // Close one of the panes; the remaining pane should still have a header.
        pane_group.update(&mut app, |pane_group, ctx| {
            pane_group.close_pane(pane_group.focused_pane_id(ctx), ctx);
        });

        pane_group.read(&app, |pane_group, ctx| {
            assert_eq!(pane_group.pane_contents.len(), 1);

            let terminal_panes = pane_group.panes_of::<TerminalPane>().collect_vec();
            assert_eq!(terminal_panes.len(), 1);

            let pane_view = terminal_panes[0].pane_view();
            assert!(pane_view
                .as_ref(ctx)
                .header()
                .as_ref(ctx)
                .is_visible_in_pane_group());
        });

        // Create a non-terminal split pane. Terminal pane header remains visible.
        pane_group.update(&mut app, |pane_group, ctx| {
            pane_group.add_pane_with_direction(
                Direction::Left,
                NotebookPane::new(new_notebook(ctx), ctx),
                true, /* focus_new_pane */
                ctx,
            );
        });

        pane_group.read(&app, |pane_group, ctx| {
            assert_eq!(pane_group.pane_contents.len(), 2);

            let terminal_panes = pane_group.panes_of::<TerminalPane>().collect_vec();
            assert_eq!(terminal_panes.len(), 1);

            let pane_view = terminal_panes[0].pane_view();
            assert!(pane_view
                .as_ref(ctx)
                .header()
                .as_ref(ctx)
                .is_visible_in_pane_group());
        });
    });
}

/// Tests that focusing two different panes in quick succession does not cause
/// an infinite loop of focus changes, as outlined in this PR's description:
/// https://github.com/warpdotdev/warp-internal/pull/8990
#[cfg_attr(windows, ignore = "TODO(CORE-3626)")]
#[test]
fn test_pane_focus_does_not_have_an_infinite_event_loop() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        // Create a pane group with two terminal panes that will fight for
        // focus.
        let mock_options = MockOptions {
            layout: PanesLayout::Template(PaneTemplateType::PaneBranchTemplate {
                split_direction: crate::launch_configs::launch_config::SplitDirection::Horizontal,
                panes: vec![
                    PaneTemplateType::PaneTemplate {
                        is_focused: Some(true),
                        cwd: "/".into(),
                        commands: vec![],
                        pane_mode: PaneMode::Terminal,
                        shell: None,
                    },
                    PaneTemplateType::PaneTemplate {
                        is_focused: None,
                        cwd: "/".into(),
                        commands: vec![],
                        pane_mode: PaneMode::Terminal,
                        shell: None,
                    },
                ],
            }),
            ..Default::default()
        };
        let pane_group = mock_pane_group(&mut app, mock_options);

        // The cycle requires that we are constantly trying to focus the input.
        // An active and long-running block causes focus to move to the
        // terminal instead of the input, so we need to wait until we've
        // finished bootstrapping to ensure no such block will exist.
        loop {
            let mut all_terminals_bootstrapped = true;
            pane_group.update(&mut app, |pane_group, ctx| {
                pane_group.for_all_terminal_panes(|terminal_view, _ctx| {
                    let model = terminal_view.model.lock();
                    let active_block = model.block_list().active_block();
                    if active_block.bootstrap_stage() != crate::terminal::model::bootstrap::BootstrapStage::PostBootstrapPrecmd ||
                        active_block.is_active_and_long_running() {
                        all_terminals_bootstrapped = false;
                    }
                }, ctx);
            });
            if all_terminals_bootstrapped {
                break;
            }
            // Return control back to the executor briefly so we can make
            // progress.
            futures_lite::future::yield_now().await;
        }

        pane_group.update(&mut app, |pane_group, ctx| {
            // Switch panes twice in quick succession.  We want to make
            // sure the test terminates and doesn't get into an infinite
            // loop.
            pane_group.navigate_next_pane(ctx);
            pane_group.navigate_next_pane(ctx);
        });
    });
}

/// A view to help us react to focus changes and know that they were processed
/// synchronously, not asynchronously (via an Effect::Event).
struct FocusDetectionView {
    pane_group: ViewHandle<PaneGroup>,
    new_focused_pane_id: Option<PaneId>,
}

impl FocusDetectionView {
    fn new(pane_group: ViewHandle<PaneGroup>, ctx: &mut ViewContext<Self>) -> Self {
        ctx.subscribe_to_view(&pane_group, |me, pane_group, event, ctx| {
            let Event::OpenPromptEditor = event else {
                return;
            };
            // This event is enqueued by us after the `Focus` effect, and so
            // by the time we receive it, application focus will have been
            // moved to the second pane, and (crucially) the pane group should
            // have updated its internal state accordingly (which is what we're
            // asserting here).

            let new_focused_pane_id = me
                .new_focused_pane_id
                .expect("should have set this already");
            pane_group.read(ctx, |pane_group, ctx| {
                assert_eq!(pane_group.focused_pane_id(ctx), new_focused_pane_id);
                assert_eq!(
                    pane_group.active_session_id(ctx),
                    new_focused_pane_id.as_terminal_pane_id()
                );
            });
        });
        Self {
            pane_group,
            new_focused_pane_id: None,
        }
    }
}

impl Entity for FocusDetectionView {
    type Event = ();
}

impl View for FocusDetectionView {
    fn ui_name() -> &'static str {
        "FocusDetectionView"
    }

    fn render(&self, _app: &AppContext) -> Box<dyn Element> {
        ChildView::new(&self.pane_group).finish()
    }
}

impl TypedActionView for FocusDetectionView {
    type Action = ();
}

/// This test ensures that a change in application focus causes the pane group
/// focused pane to update synchronously, without needing to wait for effect
/// flushing to occur.
///
/// The goal is to avoid situations where a delayed response to application
/// focus changes leads to an infinite loop of focusing and re-focusing two
/// different panes.
#[test]
fn test_focused_pane_is_synchronized_with_application_focus() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        // Create a pane group with two terminal panes, so that we can move
        // focus and observe the effects.
        let panes_layout = PanesLayout::Template(PaneTemplateType::PaneBranchTemplate {
            split_direction: crate::launch_configs::launch_config::SplitDirection::Horizontal,
            panes: vec![
                PaneTemplateType::PaneTemplate {
                    is_focused: Some(true),
                    cwd: "/".into(),
                    commands: vec![],
                    pane_mode: PaneMode::Terminal,
                    shell: None,
                },
                PaneTemplateType::PaneTemplate {
                    is_focused: None,
                    cwd: "/".into(),
                    commands: vec![],
                    pane_mode: PaneMode::Terminal,
                    shell: None,
                },
            ],
        });

        let tips_model = app.add_model(|_| TipsCompleted::default());
        let (_, root_view) =
            app.add_window_with_bounds(WindowStyle::NotStealFocus, WindowBounds::Default, |ctx| {
                let user_default_shell_changed_banner_dismissal_model_handle =
                    ctx.add_model(|_| BannerState::default());
                let block_lists = Arc::new(HashMap::new());
                let pane_group = ctx.add_typed_action_view(|ctx| {
                    PaneGroup::new_with_panes_layout(
                        tips_model,
                        user_default_shell_changed_banner_dismissal_model_handle,
                        ServerApiProvider::as_ref(ctx).get(),
                        panes_layout,
                        block_lists,
                        None,
                        ctx,
                    )
                });

                FocusDetectionView::new(pane_group, ctx)
            });
        let pane_group = root_view.read(&app, |root_view, _ctx| root_view.pane_group.clone());

        let (focused_pane_id, active_session_id) = pane_group.read(&app, |pane_group, ctx| {
            (
                pane_group.focused_pane_id(ctx),
                pane_group.active_session_id(ctx),
            )
        });

        let second_pane_id = pane_group.read(&app, |pane_group, _ctx| {
            pane_group
                .pane_ids()
                .find(|pane_id| *pane_id != focused_pane_id)
                .expect("should have more than one pane")
        });

        // Verify that the "second" pane is not focused or active.
        assert_ne!(focused_pane_id, second_pane_id);
        assert_ne!(active_session_id, second_pane_id.as_terminal_pane_id());

        root_view.update(&mut app, |root_view, _ctx| {
            root_view.new_focused_pane_id = Some(second_pane_id);
        });

        pane_group.update(&mut app, |pane_group, ctx| {
            // First, request a change of application focus to the second
            // pane's terminal view.
            pane_group
                .terminal_view_from_pane_id(second_pane_id, ctx)
                .expect("second pane is a terminal pane")
                .update(ctx, |_terminal_view, ctx| {
                    ctx.focus_self();
                });

            // Second, emit an event on the pane group to trigger assertion
            // logic in the FocusDetectionView.  This event effect is enqueued after
            // the focus effect but before the focus effect is processed, meaning
            // it will observe any changes that occured synchronously as part
            // of the focus effect but will _not_ observe any changes that result
            // from events dispatched during focus handling.
            //
            // We use `OpenPromptEditor` because we can be confident that
            // nothing else above may have emitted this event.
            //
            // IMPORTANT: This MUST be emitted in the same pane group update
            // during which we focus the terminal view, to ensure that the
            // effect queue doesn't get processed or further modified before we
            // enqueue this event on the effect queue.
            ctx.emit(Event::OpenPromptEditor);
        });
    });
}

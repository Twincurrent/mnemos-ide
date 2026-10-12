//! Mnemos IDE: the workspace's half of project-tab sync (see `workspace::ide_sync`
//! for the queue that drives it).
//!
//! Here: what the reconcile needs to know about this window's tabs, applying
//! a reconcile's result to them, the first window's project tabs at launch,
//! and turning the user's tab actions into registry jobs.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use mnemos_ui::windowing::state::ApplicationStage;
use mnemos_ui::{AppContext, EntityId, SingletonEntity, ViewContext, ViewHandle};
use mnemos_ide_projects::{Project, ProjectStore};

use crate::app_state::{LeafContents, PaneNodeSnapshot, PaneTabSnapshot};
use crate::pane_group::{NewTerminalOptions, PaneGroup, PanesLayout};
use crate::quit_warning::UnsavedStateSummary;
use crate::tab::TabData;
use crate::terminal::cli_agent_sessions::CLIAgentSessionsModel;
use crate::workspace::ide_sync::{
    close_unless_linked, IdeJob, IdeTabInfo, IdeUpdate, WorkspaceRegistryEvent,
};
use crate::workspace::WorkspaceRegistry;

use super::Workspace;

impl Workspace {
    /// What the reconcile needs to know about each tab in `workspace`.
    ///
    /// Takes the handle rather than `&self`: whether a tab is safe to close
    /// comes from the same summary Warp's own close confirmation uses, and
    /// that needs the app mutably.
    pub(crate) fn ide_tab_infos(
        workspace: &ViewHandle<Self>,
        ctx: &mut AppContext,
    ) -> Vec<IdeTabInfo> {
        let (mut infos, pane_groups) = {
            let ws = workspace.as_ref(ctx);
            let pane_groups: Vec<ViewHandle<PaneGroup>> = ws.tab_views().cloned().collect();
            (ws.ide_tab_facts(ctx), pane_groups)
        };
        for (info, pane_group) in infos.iter_mut().zip(&pane_groups) {
            info.safe_to_close = Self::ide_tab_is_idle(pane_group, ctx);
        }
        infos
    }

    /// [`Self::ide_tab_infos`] without `safe_to_close` (left `false`).
    pub(super) fn ide_tab_facts(&self, ctx: &AppContext) -> Vec<IdeTabInfo> {
        self.tabs
            .iter()
            .map(|tab| {
                let pane_group = tab.pane_group.as_ref(ctx);
                let mut views = pane_group.terminal_views(ctx);
                // The pane the user is in says most about where the tab is.
                views.sort_by_key(|view| !view.as_ref(ctx).is_active_session(ctx));
                let mut cwds: Vec<PathBuf> = views
                    .iter()
                    .filter_map(|view| view.as_ref(ctx).pwd_if_local(ctx))
                    .map(PathBuf::from)
                    .collect();
                // Until a restored or freshly opened pane's shell reports
                // its directory, the one it was opened in is the evidence.
                if let Some(initial) = self.ide_initial_cwds.get(&tab.pane_group.id()) {
                    for cwd in initial {
                        if !cwds.contains(cwd) {
                            cwds.push(cwd.clone());
                        }
                    }
                }
                IdeTabInfo {
                    pane_group_id: tab.pane_group.id(),
                    project_id: tab.tcb_project_id.clone(),
                    title: pane_group.custom_title(ctx),
                    cwds,
                    safe_to_close: false,
                }
            })
            .collect()
    }

    /// Every project id a tab in this window carries.
    pub(crate) fn ide_linked_project_ids(&self) -> Vec<String> {
        self.tabs
            .iter()
            .filter_map(|tab| tab.tcb_project_id.clone())
            .collect()
    }

    /// Nothing in the tab would be lost by closing it: no CLI agent session
    /// (an idle `claude` prompt still counts), and nothing Warp's own tab
    /// close confirmation warns about (a running command, a shared session,
    /// unsaved editor changes).
    fn ide_tab_is_idle(pane_group: &ViewHandle<PaneGroup>, ctx: &mut AppContext) -> bool {
        let agents = CLIAgentSessionsModel::as_ref(ctx);
        let agent_running = pane_group
            .as_ref(ctx)
            .terminal_views(ctx)
            .iter()
            .any(|view| agents.session(view.id()).is_some());
        !agent_running
            && !UnsavedStateSummary::for_tabs(vec![pane_group.downgrade()], ctx).has_unsaved_state()
    }

    /// Queue a project-tab job (see `workspace::ide_sync`).
    pub(super) fn ide_request(job: IdeJob, ctx: &mut ViewContext<Self>) {
        WorkspaceRegistry::handle(ctx).update(ctx, |registry, ctx| registry.ide_enqueue(job, ctx));
    }

    /// Hand the registry's last live-agents list to this (new) window, as if
    /// it had just arrived.
    pub(super) fn ide_seed_live_agents(&mut self, ctx: &mut ViewContext<Self>) {
        if let Some(machines) = WorkspaceRegistry::as_ref(ctx).live_agents_last() {
            self.handle_workspace_registry_event(
                &WorkspaceRegistryEvent::LiveAgents(machines),
                ctx,
            );
        }
    }

    pub(super) fn handle_workspace_registry_event(
        &mut self,
        event: &WorkspaceRegistryEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        match event {
            WorkspaceRegistryEvent::Ide(update) => self.ide_apply_update(update, ctx),
            WorkspaceRegistryEvent::LiveAgents(machines) => {
                let remote = crate::workspace::live_agents::remote_machines(machines);
                self.left_panel_view.update(ctx, |panel, vctx| {
                    panel.set_agents_remote_machines(remote, vctx);
                });
                // A session another machine runs is not resumed in place here.
                self.ide_claude_sessions_machines(machines.clone(), ctx);
            }
        }
    }

    /// Apply the parts of a project-tab job's result addressed to this
    /// window. Every action re-checks its tab, found by pane group and the id
    /// it had when the job started: the job ran in the background, and the
    /// user may have closed or changed the tab meanwhile.
    fn ide_apply_update(&mut self, update: &IdeUpdate, ctx: &mut ViewContext<Self>) {
        let window_id = ctx.window_id();
        let mut tabs_changed = false;

        for (tab, new_id) in update
            .relink
            .iter()
            .filter(|(t, _)| t.window_id == window_id)
        {
            if let Some(data) = self.tabs.iter_mut().find(|d| {
                d.pane_group.id() == tab.pane_group_id && d.tcb_project_id == tab.project_id
            }) {
                data.tcb_project_id = new_id.clone();
                tabs_changed = true;
            }
        }

        for tab in update.close.iter().filter(|t| t.window_id == window_id) {
            let Some(index) = self.tabs.iter().position(|d| {
                d.pane_group.id() == tab.pane_group_id && d.tcb_project_id == tab.project_id
            }) else {
                continue;
            };
            // Idle when the job started is not idle now: a command started
            // since then is left alone, like a tab that was busy from the
            // start (spared, so no later reconcile closes it either). Startup
            // too: agent-restore may have resumed a Claude session in a
            // restored pane after the snapshot was taken.
            let pane_group = self.tabs[index].pane_group.clone();
            if !Self::ide_tab_is_idle(&pane_group, ctx) {
                if let Some(id) = tab.project_id.clone() {
                    let pane_group_id = tab.pane_group_id;
                    WorkspaceRegistry::handle(ctx)
                        .update(ctx, |registry, _| registry.ide_spare(pane_group_id, id));
                }
                continue;
            }
            self.ide_close_tab_of_closed_project(index, ctx);
            tabs_changed = true;
        }

        let opens: Vec<_> = update
            .open
            .iter()
            .filter(|o| o.window_id == window_id)
            .collect();
        if !opens.is_empty() {
            // Building and titling a pane group focuses its new pane; give
            // the keyboard back to the tab the user is in if it had it.
            let active_had_focus = self
                .tabs
                .get(self.active_tab_index)
                .map(|t| t.pane_group.clone())
                .is_some_and(|pane_group| pane_group.is_self_or_child_focused(ctx));
            let mut focus = None;
            for open in opens {
                let cwd = open
                    .cwd
                    .clone()
                    .filter(|p| p.is_dir())
                    .or_else(dirs::home_dir);
                let pane_group_id =
                    self.ide_add_project_tab(&open.project_id, &open.title, cwd, ctx);
                if open.focus {
                    focus = Some(pane_group_id);
                }
            }
            if let Some(index) = focus.and_then(|pg| self.ide_tab_index(pg)) {
                self.activate_tab(index, ctx);
                ctx.windows().show_window_and_focus_app(window_id);
            } else if active_had_focus {
                self.focus_active_tab(ctx);
            }
            self.ide_apply_horizontal_tab_mode(ctx);
            tabs_changed = true;
        }

        if let Some((focus_window, pane_group_id)) = update.focus {
            if focus_window == window_id {
                if let Some(index) = self.ide_tab_index(pane_group_id) {
                    self.activate_tab(index, ctx);
                    ctx.windows().show_window_and_focus_app(window_id);
                }
            }
        }

        if let Some(open_projects) = &update.open_projects {
            self.ide_sync_project_tabs(open_projects.clone());
        }
        // A relink may have changed the active tab's project.
        self.ide_note_active_project(ctx);
        if tabs_changed {
            // Tab ids are persisted with the window snapshot (SQLite).
            ctx.dispatch_global_action("workspace:save_app", ());
            ctx.notify();
        }
    }

    pub(super) fn ide_tab_index(&self, pane_group_id: EntityId) -> Option<usize> {
        self.tabs
            .iter()
            .position(|tab| tab.pane_group.id() == pane_group_id)
    }

    /// Close a tab whose project was closed (on another machine, in another
    /// window, or before this launch).
    fn ide_close_tab_of_closed_project(&mut self, index: usize, ctx: &mut ViewContext<Self>) {
        // Unlinked first: the project is already closed, and `remove_tab`
        // would otherwise record this as the user closing it here.
        self.tabs[index].tcb_project_id = None;
        // The last tab would take the window with it (and on Windows the
        // app); it stays as a plain terminal tab instead.
        if self.tabs.len() == 1 {
            return;
        }
        // Indices shift below; a rename in progress would land on the wrong
        // tab.
        self.cancel_tab_rename(ctx);
        self.remove_tab(index, true, true, ctx);
    }

    /// Add a terminal tab for a project at the end of the strip, without
    /// activating it: `add_tab_with_pane_layout` always activates (and closes
    /// the palette), and a project opened on another machine must not pull
    /// the user off the tab they are in. Returns the tab's pane group id.
    pub(super) fn ide_add_project_tab(
        &mut self,
        project_id: &str,
        title: &str,
        cwd: Option<PathBuf>,
        ctx: &mut ViewContext<Self>,
    ) -> EntityId {
        let layout = PanesLayout::SingleTerminal(Box::new(NewTerminalOptions {
            initial_directory: cwd.clone(),
            ..Default::default()
        }));
        let pane_group = ctx.add_typed_action_view(|ctx| {
            let mut pane_group = PaneGroup::new_with_panes_layout(
                self.tips_completed.clone(),
                self.user_default_shell_unsupported_banner_model_handle
                    .clone(),
                self.server_api.clone(),
                layout,
                Arc::new(HashMap::new()),
                self.model_event_sender.clone(),
                ctx,
            );
            if !title.is_empty() {
                pane_group.set_title(title, ctx);
            }
            pane_group
        });
        ctx.subscribe_to_view(&pane_group, move |me, pane_group, event, ctx| {
            me.handle_file_tree_event(pane_group, event, ctx)
        });
        let pane_group_id = pane_group.id();
        let mut tab = TabData::new(pane_group);
        tab.tcb_project_id = Some(project_id.to_string());
        self.tabs.push(tab);
        if let Some(cwd) = cwd {
            self.ide_initial_cwds.insert(pane_group_id, vec![cwd]);
        }
        ctx.notify();
        pane_group_id
    }

    /// The first window of a launch that restored no tabs gets its project
    /// tabs right away, one per open project whose checkout here is known.
    /// Open projects without a known checkout are left to the startup
    /// reconcile, which runs discovery (a disk walk) off the UI thread and
    /// opens their tabs at the checkout it finds.
    ///
    /// `None`: not the first window (a Cmd+N window is a plain window), or no
    /// open projects. `Some(n)`: `n` tabs were opened.
    pub(super) fn ide_seed_project_tabs(&mut self, ctx: &mut ViewContext<Self>) -> Option<usize> {
        if WorkspaceRegistry::as_ref(ctx).ide_has_other_windows() {
            return None;
        }
        let store = match ProjectStore::load_blocking() {
            Ok(store) => store,
            Err(e) => {
                log::warn!(target: "mnemos_ide", "ProjectStore::load_blocking failed: {e}");
                return None;
            }
        };
        let open = store.open_projects_in_order();
        if open.is_empty() {
            return None;
        }
        let mut created = 0;
        for project in &open {
            let dir = project.local_path().filter(|p| p.is_dir());
            // A record from before the synced flag (only this machine's
            // legacy open list) opens even without a checkout: the reconcile
            // only opens flagged records, so skipping it would drop it.
            if dir.is_none() && project.is_open() == Some(true) {
                continue;
            }
            self.ide_add_project_tab(&project.id, &project.name, dir.or_else(dirs::home_dir), ctx);
            created += 1;
        }
        if created > 0 {
            let active = store
                .tab_state
                .as_ref()
                .and_then(|s| s.active_project_id.as_deref())
                .and_then(|id| store.resolve_id(id))
                .and_then(|id| {
                    self.tabs
                        .iter()
                        .position(|t| t.tcb_project_id.as_deref() == Some(id.as_str()))
                })
                .unwrap_or(0);
            self.activate_tab_internal(active, ctx);
        }
        self.ide_sync_project_tabs(open);
        log::info!(target: "mnemos_ide", "mnemos-ide: seeded {created} project tab(s)");
        Some(created)
    }

    /// Whether this window runs in Mnemos IDE's project-tab layout: a tab is linked
    /// to a project, or the registry has open projects.
    pub(super) fn ide_projects_in_use(&self) -> bool {
        self.tabs.iter().any(|t| t.tcb_project_id.is_some())
            || ProjectStore::load_blocking()
                .map(|store| !store.open_projects_in_order().is_empty())
                .unwrap_or(false)
    }

    /// A tab linked to `project_id` is gone from this window.
    ///
    /// When its pane group now lives in another window (tab tear-off hands
    /// the group over before the source tab is removed, and the new window's
    /// tab starts unlinked) the link moves with it. Otherwise the queue closes
    /// the project, unless another tab in any window still shows it. Never a
    /// delete: the record stays in the registry.
    pub(super) fn ide_project_tab_removed(
        &mut self,
        pane_group_id: EntityId,
        project_id: String,
        from_shell_exit: bool,
        ctx: &mut ViewContext<Self>,
    ) {
        // This workspace is mid-update, so `all_workspaces` leaves it out.
        let moved_to = WorkspaceRegistry::as_ref(ctx)
            .all_workspaces(ctx)
            .into_iter()
            .find(|(_, ws)| {
                ws.as_ref(ctx)
                    .tabs
                    .iter()
                    .any(|t| t.pane_group.id() == pane_group_id)
            })
            .map(|(_, ws)| ws);
        if let Some(workspace) = moved_to {
            workspace.update(ctx, |ws, _| {
                if let Some(tab) = ws
                    .tabs
                    .iter_mut()
                    .find(|t| t.pane_group.id() == pane_group_id && t.tcb_project_id.is_none())
                {
                    tab.tcb_project_id = Some(project_id);
                }
            });
            return;
        }
        if from_shell_exit {
            WorkspaceRegistry::handle(ctx)
                .update(ctx, |registry, ctx| registry.ide_tab_exited(project_id, ctx));
            return;
        }
        Self::ide_request(IdeJob::TabClosed { project_id }, ctx);
    }

    /// Record the active tab's project as this machine's active project, when
    /// it changed (`workspace_state.json`, read when the next launch seeds
    /// its project tabs); nothing else writes it since tabs stopped being
    /// edited through the store.
    pub(super) fn ide_note_active_project(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(id) = self
            .tabs
            .get(self.active_tab_index)
            .and_then(|t| t.tcb_project_id.clone())
        else {
            return;
        };
        if self.ide_noted_active.as_deref() == Some(id.as_str()) {
            return;
        }
        self.ide_noted_active = Some(id.clone());
        Self::ide_request(IdeJob::Activated { project_id: id }, ctx);
    }

    /// The last tab of this window is being closed by the user (Cmd+W): the
    /// window closes with it. Its project is closed once the window is
    /// actually gone (see [`Self::ide_window_closed`]): until then the tab
    /// still counts as showing it, and the window close can be cancelled
    /// (the quit warning).
    pub(super) fn ide_closing_last_tab(
        &mut self,
        pane_group_id: EntityId,
        project_id: String,
        from_shell_exit: bool,
    ) {
        self.ide_closing_last_tab = Some((pane_group_id, project_id, from_shell_exit));
    }

    /// This window closed; it is already unregistered, so no queue job sees
    /// its tabs any more.
    ///
    /// - Quitting the app closes no project: that is not the user closing
    ///   anything, and the projects must be open on the next launch.
    /// - Closing a window while others stay open closes its project tabs,
    ///   like closing each tab (unless another window still shows one, or a
    ///   tab moved to another window). Left open, they would pop back up in
    ///   the primary window at whatever later moment a reconcile runs.
    /// - Closing the last window is how Windows quits the app, so only an
    ///   explicit close of its last tab (Cmd+W) closes that one project.
    ///   This is done right here, not queued: the process may exit before a
    ///   queued job runs.
    pub(super) fn ide_window_closed(&mut self, ctx: &mut ViewContext<Self>) {
        let last_tab = self
            .ide_closing_last_tab
            .take()
            .filter(|(pane_group_id, id, _)| {
                self.tabs.len() == 1
                    && self.tabs[0].pane_group.id() == *pane_group_id
                    && self.tabs[0].tcb_project_id.as_deref() == Some(id.as_str())
            });
        // The window closed because its last shell exited: a shutdown, crash
        // or `exit`, not the user closing the window. Same deferred path as a
        // tab whose shell exited (see `WorkspaceRegistry::ide_tab_exited`).
        let from_shell_exit = last_tab.as_ref().is_some_and(|(_, _, s)| *s);
        if ctx.windows().stage() == ApplicationStage::Terminating {
            return;
        }
        if WorkspaceRegistry::as_ref(ctx).ide_has_other_windows() {
            let linked: Vec<(EntityId, String)> = self
                .tabs
                .iter()
                .filter_map(|t| Some((t.pane_group.id(), t.tcb_project_id.clone()?)))
                .collect();
            for (pane_group_id, project_id) in linked {
                self.ide_project_tab_removed(pane_group_id, project_id, from_shell_exit, ctx);
            }
            return;
        }
        let Some((_, project_id, _)) = last_tab else {
            return;
        };
        if from_shell_exit {
            // Usually lost when the process exits, which is the point: a
            // shutdown closes no project.
            WorkspaceRegistry::handle(ctx)
                .update(ctx, |registry, ctx| registry.ide_tab_exited(project_id, ctx));
            return;
        }
        if let Err(e) = ProjectStore::update_blocking(|store| {
            close_unless_linked(store, &project_id, &[], Utc::now())
        }) {
            log::warn!(target: "mnemos_ide", "mnemos-ide: closing the last tab's project failed: {e}");
        }
    }

    /// This window came back (undo close window). Closing it closed its
    /// projects; they are open again, and the window rejoins the registry so
    /// the reconcile sees its tabs (and can make it the primary window).
    pub(super) fn ide_window_reopened(&mut self, ctx: &mut ViewContext<Self>) {
        let window_id = ctx.window_id();
        let handle = ctx.handle();
        WorkspaceRegistry::handle(ctx).update(ctx, |registry, _| {
            registry.register(window_id, handle);
        });
        let ids: Vec<String> = self
            .tabs
            .iter()
            .filter_map(|t| t.tcb_project_id.clone())
            .collect();
        for project_id in ids {
            Self::ide_request(IdeJob::TabReopened { project_id }, ctx);
        }
        Self::ide_request(IdeJob::Reconcile, ctx);
    }

    /// Mirror the open set into `ProjectTabs`, with the active tab's project
    /// as its active one.
    pub(super) fn ide_sync_project_tabs(&mut self, open_projects: Vec<Project>) {
        self.ide_project_tabs.set_projects(open_projects);
        if let Some(id) = self
            .tabs
            .get(self.active_tab_index)
            .and_then(|t| t.tcb_project_id.clone())
        {
            self.ide_project_tabs.set_active(&id);
        }
    }
}

/// Working directories a restored tab's terminal panes had when the app last
/// saved them, most relevant first: the active terminal tab's focused pane,
/// then the rest of that tab, then the other terminal tabs.
pub(super) fn snapshot_cwds(tabs: &[PaneTabSnapshot], active: usize) -> Vec<PathBuf> {
    let ordered = tabs.get(active).into_iter().chain(
        tabs.iter()
            .enumerate()
            .filter(|(i, _)| *i != active)
            .map(|(_, tab)| tab),
    );
    let mut out: Vec<PathBuf> = Vec::new();
    for tab in ordered {
        let mut leaves: Vec<(bool, PathBuf)> = Vec::new();
        collect_leaf_cwds(&tab.root, &mut leaves);
        leaves.sort_by_key(|(focused, _)| !focused);
        for (_, cwd) in leaves {
            if !out.contains(&cwd) {
                out.push(cwd);
            }
        }
    }
    out
}

fn collect_leaf_cwds(node: &PaneNodeSnapshot, out: &mut Vec<(bool, PathBuf)>) {
    match node {
        PaneNodeSnapshot::Leaf(leaf) => {
            if let LeafContents::Terminal(terminal) = &leaf.contents {
                if let Some(cwd) = terminal.cwd.as_deref().filter(|c| !c.trim().is_empty()) {
                    out.push((leaf.is_focused, PathBuf::from(cwd)));
                }
            }
        }
        PaneNodeSnapshot::Branch(branch) => {
            for (_, child) in &branch.children {
                collect_leaf_cwds(child, out);
            }
        }
    }
}

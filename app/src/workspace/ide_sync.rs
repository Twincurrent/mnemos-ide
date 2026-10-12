//! Mnemos IDE: keeps every window's project tabs in step with the synced project
//! registry (`projects.json`).
//!
//! The registry's `open` flags are the tab set every machine shows. This
//! module runs `mnemos_ide_projects::reconcile::plan` against a snapshot of the tabs
//! in *all* windows, and records the user's own tab actions (close a project
//! tab, reopen one, add a project) as registry mutations.
//!
//! Everything goes through one queue on [`WorkspaceRegistry`], one job at a
//! time, so a reconcile never races another reconcile or a close: a job's tab
//! snapshot is always taken after the previous job's result was applied. The
//! store work (and the first discovery scan, which walks the disk) runs on the
//! background executor under `ProjectStore`'s process-wide lock; the result
//! comes back as a [`WorkspaceRegistryEvent`] that each workspace applies to
//! its own tabs. Applying it from the event, rather than from the job's
//! callback, matters: the callback runs while the registry model is being
//! updated, and closing a tab or a window re-enters the registry.
//!
//! Live changes (a pull from another machine, a close in another window) are
//! picked up by polling the file's `(mtime, len)` every two seconds.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Utc};
use mnemos_ui::r#async::Timer;
use mnemos_ui::windowing::state::ApplicationStage;
use mnemos_ui::{AppContext, EntityId, ModelContext, SingletonEntity, WindowId};
use mnemos_ide_projects::{
    FsEnv, LocalResolver, Project, ProjectStore, RealFs, ReconcileMode, ReconcilePlan,
};

use super::{Workspace, WorkspaceRegistry};

/// How often `projects.json` is checked for changes made by someone else
/// (mnemos-ide-sync applying a pull, another Mnemos process).
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// How often the first reconcile checks whether mnemos-ide-sync's startup pull is
/// in, and how many times before it goes ahead without it (15 s).
const FIRST_PULL_POLL: Duration = Duration::from_millis(250);
const FIRST_PULL_MAX_WAITS: u32 = 60;

/// Work for the project-tab queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IdeJob {
    /// Match every window's tabs to the registry (open, close, relink).
    Reconcile,
    /// A tab linked to `project_id` was closed by the user. The project is
    /// closed everywhere unless another tab (in any window) still shows it.
    TabClosed { project_id: String },
    /// A closed project tab came back (undo close): its project is open again.
    TabReopened { project_id: String },
    /// "Add project" picked `root`; open or focus its tab in `window_id`.
    Add { root: PathBuf, window_id: WindowId },
    /// A tab linked to `project_id` became the active tab. Recorded as this
    /// machine's active project (`workspace_state.json`), the tab the next
    /// launch activates when it seeds its project tabs.
    Activated { project_id: String },
}

/// A tab as it was when a job started. Tabs are addressed by pane group, not
/// by index: indices shift while the job runs in the background.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdeTabRef {
    pub window_id: WindowId,
    pub pane_group_id: EntityId,
    /// The id the tab must still carry for an action to apply to it.
    pub project_id: Option<String>,
}

/// What a workspace reports about one of its tabs.
#[derive(Debug, Clone)]
pub struct IdeTabInfo {
    pub pane_group_id: EntityId,
    pub project_id: Option<String>,
    pub title: Option<String>,
    pub cwds: Vec<PathBuf>,
    pub safe_to_close: bool,
}

/// A tab to open for a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdeTabOpen {
    pub window_id: WindowId,
    pub project_id: String,
    pub title: String,
    pub cwd: Option<PathBuf>,
    /// Activate the new tab (and its window). Reconcile opens never do: a
    /// project opened on another machine must not steal focus here.
    pub focus: bool,
}

/// The result of one job, for every workspace to apply to its own tabs.
#[derive(Debug, Clone, Default)]
pub struct IdeUpdate {
    /// The open projects in tab order, for `ProjectTabs`. `None` when the
    /// store could not be read.
    pub open_projects: Option<Vec<Project>>,
    /// Tabs whose project id changes (`None` unlinks).
    pub relink: Vec<(IdeTabRef, Option<String>)>,
    /// Tabs to close. `project_id` is the id the tab carries after `relink`.
    /// Each is re-checked when applied: a tab that became busy since the
    /// job's snapshot is spared, whatever the mode.
    pub close: Vec<IdeTabRef>,
    pub open: Vec<IdeTabOpen>,
    /// Activate this existing tab (and its window).
    pub focus: Option<(WindowId, EntityId)>,
}

pub enum WorkspaceRegistryEvent {
    /// Mnemos IDE: a project-tab job finished. Each workspace applies the parts
    /// addressed to its window.
    Ide(Arc<IdeUpdate>),
    /// Mnemos IDE: the other machines' running agents (see `workspace::live_agents`).
    LiveAgents(Arc<Vec<mnemos_ide_sync::live_agents::MachineAgents>>),
}

type FileSig = Option<(Option<SystemTime>, u64)>;

/// Queue state; lives on [`WorkspaceRegistry`], which every workspace (and
/// every test harness that builds one) already has.
#[derive(Default)]
pub(crate) struct IdeProjectSync {
    queue: VecDeque<IdeJob>,
    /// A job is scheduled or in flight.
    running: bool,
    /// Holds discovery's scan cache between reconciles. Lent to the job
    /// while it runs; a failed job loses it and the next one rescans.
    resolver: Option<LocalResolver>,
    /// The first reconcile of the process has run (successfully or not). It
    /// runs as `Startup` and waits for mnemos-ide-sync's startup pull (see
    /// `ide_first_pull_pending`); everything after it is `Live`. Neither mode
    /// closes a busy tab.
    startup_done: bool,
    /// Times the first reconcile has waited for mnemos-ide-sync's startup pull.
    first_pull_waits: u32,
    /// Tabs left open (busy) when their project was closed elsewhere, by pane
    /// group, with the id they carried. A remote close is acted on once: a
    /// later reconcile, run for some unrelated change, must not close the
    /// tab under the user once whatever ran in it has finished. Forgotten
    /// when the project opens again or the tab goes away.
    spared: HashMap<EntityId, String>,
    polling: bool,
    /// Last `(mtime, len)` of `projects.json`; `None` until the first poll.
    last_sig: Option<FileSig>,
    /// The file changed and has not been reconciled yet.
    changed: bool,
    /// Unit tests only: jobs recorded instead of run (see `ide_enqueue`).
    test_queued: Vec<IdeJob>,
    /// Project tabs that went away because their shell exited, waiting out
    /// [`SHELL_EXIT_SETTLE`] before they count as the user closing the project
    /// (see [`WorkspaceRegistry::ide_tab_exited`]).
    exited_pending: Vec<(String, Instant)>,
    /// Two shell exits landed within [`SHELL_EXIT_BURST`] of each other.
    exit_burst: bool,
}

/// How long a shell-exit close waits before it closes the project. Long
/// enough for an OS shutdown to kill the process first, short enough that
/// typing `exit` still closes the project before anyone notices.
const SHELL_EXIT_SETTLE: Duration = Duration::from_secs(5);
/// Shell exits closer together than this are one event: shutdown, logoff, a
/// crash or a kill, never a user closing projects one by one.
const SHELL_EXIT_BURST: Duration = Duration::from_secs(2);

impl WorkspaceRegistry {
    /// Queue `job` (see [`queue_job`] for the order).
    pub(crate) fn ide_enqueue(&mut self, job: IdeJob, ctx: &mut ModelContext<Self>) {
        // The first job runs at launch; pin the session start before anything
        // can be reopened against it.
        ide_session_start();
        // Unit tests share one redirected Mnemos IDE store per process
        // (`view_test.rs`); a job run by one test (a reconcile registering a
        // tab's temp folder, say) would write to it under every other test.
        // They assert on what was queued instead.
        if cfg!(test) {
            self.ide.test_queued.push(job);
            return;
        }
        queue_job(&mut self.ide.queue, job);
        self.ide_pump(ctx);
    }

    /// A project tab went away because its shell exited, not because the
    /// user closed it. On Windows, shutting down or logging off kills every
    /// shell first; Mnemos then removed each tab and closed its project on
    /// every machine (2026-10-02: 11 projects closed in two seconds). So such
    /// a close waits [`SHELL_EXIT_SETTLE`], and is dropped if the app is
    /// terminating by then or if exits came in a burst.
    pub(crate) fn ide_tab_exited(&mut self, project_id: String, ctx: &mut ModelContext<Self>) {
        let now = Instant::now();
        if self
            .ide
            .exited_pending
            .iter()
            .any(|(_, at)| now.duration_since(*at) < SHELL_EXIT_BURST)
        {
            self.ide.exit_burst = true;
        }
        let first = self.ide.exited_pending.is_empty();
        self.ide.exited_pending.push((project_id, now));
        if first {
            ctx.spawn(
                async { Timer::after(SHELL_EXIT_SETTLE).await },
                |registry, _, ctx| registry.ide_flush_exited(ctx),
            );
        }
    }

    fn ide_flush_exited(&mut self, ctx: &mut ModelContext<Self>) {
        let pending = std::mem::take(&mut self.ide.exited_pending);
        let burst = std::mem::take(&mut self.ide.exit_burst);
        let terminating = ctx.windows().stage() == ApplicationStage::Terminating;
        if burst || terminating {
            log::info!(
                target: "mnemos_ide",
                "mnemos-ide: {} shell exit(s) look like a shutdown or crash (burst={burst}, terminating={terminating}); projects stay open",
                pending.len(),
            );
            return;
        }
        for (project_id, _) in pending {
            self.ide_enqueue(IdeJob::TabClosed { project_id }, ctx);
        }
    }

    /// Unit tests: the jobs queued since the last call.
    #[cfg(test)]
    pub(crate) fn ide_take_queued(&mut self) -> Vec<IdeJob> {
        std::mem::take(&mut self.ide.test_queued)
    }

    /// Start watching `projects.json` for changes made elsewhere. Idempotent.
    pub(crate) fn ide_start_polling(&mut self, ctx: &mut ModelContext<Self>) {
        // Unit tests run no jobs (see `ide_enqueue`), and a poll loop per
        // test app would only add timers that never settle.
        if self.ide.polling || cfg!(test) {
            return;
        }
        self.ide.polling = true;
        self.live_agents_start(ctx);
        // The baseline is taken now, before anything could write the file:
        // mnemos-ide-sync's startup pull usually lands within the first poll
        // interval, and a baseline taken after it would swallow that change.
        self.ide.last_sig = Some(projects_file_sig());
        self.ide_schedule_poll(ctx);
        // The first window of the process reconciles once even when it was
        // opened for something else (a URL, a launch config) rather than
        // restored; restored and empty windows have queued one already, and
        // a waiting reconcile absorbs this one.
        self.ide_enqueue(IdeJob::Reconcile, ctx);
    }

    /// Whether a normal window other than the quake window is registered.
    /// Reads only the registry's own map: the caller may be a window under
    /// construction while another workspace is mid-update (Cmd+N), and a
    /// workspace being updated cannot be read or even upgraded.
    pub(crate) fn ide_has_other_windows(&self) -> bool {
        let quake = crate::root_view::quake_mode_window_id();
        self.window_ids().any(|id| Some(id) != quake)
    }

    /// The window that opens tabs for projects that have none anywhere: the
    /// oldest normal window. Only one window may do it, or every window would
    /// open its own copy.
    pub(crate) fn ide_primary_window(&self, ctx: &AppContext) -> Option<WindowId> {
        let quake = crate::root_view::quake_mode_window_id();
        self.all_workspaces(ctx)
            .into_iter()
            .filter(|(id, ws)| Some(*id) != quake && !ws.as_ref(ctx).is_drag_preview_workspace())
            .map(|(id, _)| id)
            .min()
    }

    fn ide_pump(&mut self, ctx: &mut ModelContext<Self>) {
        if self.ide.running || self.ide.queue.is_empty() {
            return;
        }
        self.ide.running = true;
        // Start on a later turn of the event loop, never inside the caller:
        // the job reads every workspace, and the caller is usually one of
        // them (mid-update) or a window still being built. At launch this
        // also lets every restored window exist before the first snapshot.
        ctx.spawn(async {}, |registry, _, ctx| registry.ide_start_next(ctx));
    }

    fn ide_finish(&mut self, update: Option<IdeUpdate>, ctx: &mut ModelContext<Self>) {
        if let Some(update) = update {
            ctx.emit(WorkspaceRegistryEvent::Ide(Arc::new(update)));
        }
        self.ide.running = false;
        // The next job starts on a later turn, after every workspace has
        // applied this one, so its snapshot sees the result.
        self.ide_pump(ctx);
    }

    fn ide_start_next(&mut self, ctx: &mut ModelContext<Self>) {
        let Some(job) = self.ide.queue.pop_front() else {
            self.ide.running = false;
            return;
        };
        match job {
            IdeJob::Reconcile if self.ide_first_pull_pending(ctx) => {
                // Held, not dropped: back at the front, looked at again
                // shortly. User actions queued meanwhile go ahead of it.
                self.ide.queue.push_front(IdeJob::Reconcile);
                ctx.spawn(
                    async { Timer::after(FIRST_PULL_POLL).await },
                    |registry, _, ctx| registry.ide_start_next(ctx),
                );
            }
            IdeJob::Reconcile if !self.ide.exited_pending.is_empty() => {
                // A shell exit is still settling: its project is open in the
                // store but its tab is gone, so a reconcile now would reopen
                // the tab the user just exited. Look again shortly.
                self.ide.queue.push_front(IdeJob::Reconcile);
                ctx.spawn(
                    async { Timer::after(FIRST_PULL_POLL).await },
                    |registry, _, ctx| registry.ide_start_next(ctx),
                );
            }
            IdeJob::Reconcile => self.ide_start_reconcile(ctx),
            IdeJob::TabClosed { project_id } => {
                let linked = self.ide_linked_ids(ctx);
                self.ide_run_store_job(ctx, move |store, now| {
                    close_unless_linked(store, &project_id, &linked, now);
                });
            }
            IdeJob::TabReopened { project_id } => {
                self.ide_run_store_job(ctx, move |store, now| {
                    reopen(store, &project_id, now);
                });
            }
            IdeJob::Add { root, window_id } => self.ide_start_add(root, window_id, ctx),
            IdeJob::Activated { project_id } => {
                // A tab the user switches to shows a project they are using. If
                // that project was closed before this session started (a past
                // shutdown closed eleven at once on 2026-10-02), open it again.
                // A close made while this session runs is a decision taken on
                // the other machine and is left alone, and nothing reopens
                // before the first reconcile, which may still close this tab.
                let session_start = (self.ide.startup_done).then(ide_session_start);
                self.ide_run_store_job(ctx, move |store, now| {
                    set_active_project(store, &project_id);
                    if let Some(session_start) = session_start {
                        reopen_if_closed_before(store, &project_id, session_start, now);
                    }
                });
            }
        }
    }

    /// Whether the first reconcile should wait for mnemos-ide-sync's startup pull.
    ///
    /// It would otherwise run on the `projects.json` the last session left
    /// behind: a project another machine closed meanwhile would have its
    /// restored tab closed and then reopened by the pull (losing its panes),
    /// and whatever the reconcile stamps would carry a fresh clock that beats
    /// the other machine's newer choice once merged. Bounded, so an
    /// unreachable server delays the first reconcile by seconds, not forever.
    fn ide_first_pull_pending(&mut self, ctx: &AppContext) -> bool {
        if self.ide.startup_done || self.ide.first_pull_waits >= FIRST_PULL_MAX_WAITS {
            return false;
        }
        let Some(manager) = crate::auth::auth_manager::AuthManager::as_ref(ctx).ide_sync_manager()
        else {
            return false;
        };
        let pending = !manager.status().borrow().first_pull_done;
        if pending {
            self.ide.first_pull_waits += 1;
            if self.ide.first_pull_waits == FIRST_PULL_MAX_WAITS {
                log::info!(target: "mnemos_ide", "mnemos-ide: startup pull still running; reconciling without it");
            }
        }
        pending
    }

    /// Every tab in every window, in window order.
    fn ide_collect_tabs(&self, ctx: &mut AppContext) -> Vec<(WindowId, IdeTabInfo)> {
        let mut workspaces = self.all_workspaces(ctx);
        workspaces.sort_by_key(|(id, _)| *id);
        let mut out = Vec::new();
        for (window_id, ws) in workspaces {
            for info in Workspace::ide_tab_infos(&ws, ctx) {
                out.push((window_id, info));
            }
        }
        out
    }

    fn ide_linked_ids(&self, ctx: &AppContext) -> Vec<String> {
        self.all_workspaces(ctx)
            .into_iter()
            .flat_map(|(_, ws)| ws.as_ref(ctx).ide_linked_project_ids())
            .collect()
    }

    fn ide_start_reconcile(&mut self, ctx: &mut ModelContext<Self>) {
        let mode = if self.ide.startup_done {
            ReconcileMode::Live
        } else {
            ReconcileMode::Startup
        };
        let tabs = self.ide_collect_tabs(ctx);
        // A spared tab whose tab went away, or that now carries another id,
        // is forgotten.
        self.ide.spared.retain(|pane_group_id, id| {
            tabs.iter().any(|(_, info)| {
                info.pane_group_id == *pane_group_id && info.project_id.as_deref() == Some(id)
            })
        });
        let spared = self.ide.spared.clone();
        let refs: Vec<IdeTabRef> = tabs
            .iter()
            .map(|(window_id, info)| IdeTabRef {
                window_id: *window_id,
                pane_group_id: info.pane_group_id,
                project_id: info.project_id.clone(),
            })
            .collect();
        let snapshots: Vec<mnemos_ide_projects::TabSnapshot> = tabs
            .into_iter()
            .enumerate()
            .map(|(key, (_, info))| {
                let spared = spared
                    .get(&info.pane_group_id)
                    .is_some_and(|id| info.project_id.as_deref() == Some(id.as_str()));
                mnemos_ide_projects::TabSnapshot {
                    key,
                    project_id: info.project_id,
                    title: info.title,
                    cwds: info.cwds,
                    safe_to_close: info.safe_to_close && !spared,
                }
            })
            .collect();
        let primary = self.ide_primary_window(ctx);
        let mut resolver = self.ide.resolver.take().unwrap_or_default();
        ctx.spawn(
            async move {
                ProjectStore::update(move |store| {
                    let plan =
                        mnemos_ide_projects::plan(store, &snapshots, mode, &RealFs, &mut resolver, Utc::now());
                    (plan, store.open_projects_in_order(), resolver)
                })
                .await
            },
            move |registry, result, ctx| {
                // Set whatever the outcome: a retry after a failed first
                // attempt runs minutes later for some remote change, and must
                // not hold for the startup pull again.
                registry.ide.startup_done = true;
                let update = match result {
                    Ok((plan, open_projects, resolver)) => {
                        registry.ide.resolver = Some(resolver);
                        registry.ide_note_spared(&plan, &refs, &open_projects);
                        if !plan.is_empty() {
                            log::info!(
                                target: "mnemos_ide",
                                "mnemos-ide: reconcile ({mode:?}): relink {}, open {}, close {}, store changed {}",
                                plan.relink.len(),
                                plan.open.len(),
                                plan.close.len(),
                                plan.store_changed,
                            );
                        }
                        Some(update_from_plan(&plan, &refs, primary, open_projects))
                    }
                    Err(e) => {
                        log::warn!(target: "mnemos_ide", "mnemos-ide: reconcile failed: {e}");
                        None
                    }
                };
                registry.ide_finish(update, ctx);
            },
        );
    }

    /// A close for this tab was skipped because it had become busy by the
    /// time the result reached its window.
    pub(crate) fn ide_spare(&mut self, pane_group_id: EntityId, project_id: String) {
        self.ide.spared.insert(pane_group_id, project_id);
    }

    /// Remember the tabs this plan kept (busy, project closed elsewhere), and
    /// forget spared tabs whose project is open again.
    fn ide_note_spared(&mut self, plan: &ReconcilePlan, refs: &[IdeTabRef], open: &[Project]) {
        for key in &plan.kept {
            let Some(tab) = refs.get(*key) else { continue };
            // The id the tab carries once this plan's relinks are applied.
            let id = plan
                .relink
                .iter()
                .find(|(k, _)| k == key)
                .map_or(tab.project_id.clone(), |(_, id)| id.clone());
            if let Some(id) = id {
                self.ide.spared.insert(tab.pane_group_id, id);
            }
        }
        self.ide
            .spared
            .retain(|_, id| !open.iter().any(|p| p.id == *id));
    }

    /// A store mutation whose only visible effect is the open set.
    fn ide_run_store_job(
        &mut self,
        ctx: &mut ModelContext<Self>,
        mutate: impl FnOnce(&mut ProjectStore, DateTime<Utc>) + Send + 'static,
    ) {
        ctx.spawn(
            async move {
                ProjectStore::update(move |store| {
                    mutate(store, Utc::now());
                    store.open_projects_in_order()
                })
                .await
            },
            |registry, result, ctx| {
                let update = match result {
                    Ok(open_projects) => Some(IdeUpdate {
                        open_projects: Some(open_projects),
                        ..Default::default()
                    }),
                    Err(e) => {
                        log::warn!(target: "mnemos_ide", "mnemos-ide: project store update failed: {e}");
                        None
                    }
                };
                registry.ide_finish(update, ctx);
            },
        );
    }

    fn ide_start_add(&mut self, root: PathBuf, window_id: WindowId, ctx: &mut ModelContext<Self>) {
        let tabs: Vec<IdeTabRef> = self
            .ide_collect_tabs(ctx)
            .into_iter()
            .map(|(window_id, info)| IdeTabRef {
                window_id,
                pane_group_id: info.pane_group_id,
                project_id: info.project_id,
            })
            .collect();
        let primary = self.ide_primary_window(ctx);
        let target = if self.get(window_id, ctx).is_some() {
            Some(window_id)
        } else {
            primary
        };
        let cwd = root.clone();
        ctx.spawn(
            async move {
                ProjectStore::update(move |store| {
                    let added = add_project(store, &root, &RealFs, Utc::now());
                    // An id held by a tab from before a fold still means this
                    // project.
                    let existing = tabs
                        .iter()
                        .find(|t| {
                            t.project_id
                                .as_deref()
                                .and_then(|id| store.resolve_id(id))
                                .as_deref()
                                == Some(added.id.as_str())
                        })
                        .cloned();
                    (added, existing, store.open_projects_in_order())
                })
                .await
            },
            move |registry, result, ctx| {
                let update = match result {
                    Ok((added, existing, open_projects)) => {
                        log::info!(
                            target: "mnemos_ide",
                            "mnemos-ide: added project {:?} at {:?} (id={})",
                            added.title, cwd, added.id,
                        );
                        // A checkout in a new place: the next discovery must
                        // not answer from a scan that predates it.
                        if let Some(resolver) = registry.ide.resolver.as_mut() {
                            resolver.invalidate();
                        }
                        let mut update = IdeUpdate {
                            open_projects: Some(open_projects),
                            ..Default::default()
                        };
                        match (existing, target) {
                            (Some(tab), _) => {
                                update.focus = Some((tab.window_id, tab.pane_group_id))
                            }
                            (None, Some(window_id)) => update.open.push(IdeTabOpen {
                                window_id,
                                project_id: added.id,
                                title: added.title,
                                cwd: Some(cwd),
                                focus: true,
                            }),
                            (None, None) => {}
                        }
                        Some(update)
                    }
                    Err(e) => {
                        log::warn!(target: "mnemos_ide", "mnemos-ide: add project failed: {e}");
                        None
                    }
                };
                registry.ide_finish(update, ctx);
            },
        );
    }

    fn ide_schedule_poll(&mut self, ctx: &mut ModelContext<Self>) {
        ctx.spawn(
            async {
                Timer::after(POLL_INTERVAL).await;
                projects_file_sig()
            },
            |registry, sig, ctx| {
                registry.ide_on_poll(sig, ctx);
                registry.ide_schedule_poll(ctx);
            },
        );
    }

    /// Reconcile once the file has changed and then stayed put for a whole
    /// poll interval, so a burst of writes (a pull, then its echo, then the
    /// path learning) costs one reconcile rather than three.
    fn ide_on_poll(&mut self, sig: FileSig, ctx: &mut ModelContext<Self>) {
        match &self.ide.last_sig {
            // First look: a baseline. The startup reconcile covers the
            // state the app came up with.
            None => self.ide.last_sig = Some(sig),
            Some(prev) if *prev != sig => {
                self.ide.last_sig = Some(sig);
                self.ide.changed = true;
            }
            Some(_) if self.ide.changed => {
                self.ide.changed = false;
                self.ide_enqueue(IdeJob::Reconcile, ctx);
            }
            Some(_) => {}
        }
    }
}

/// Add `job` to the jobs not started yet. A reconcile already waiting absorbs
/// another one (it snapshots the tabs when it starts, so it covers both). A
/// user action goes ahead of a waiting reconcile: that
/// reconcile must see the registry with the action applied. Run after it, a
/// close would come too late (the reconcile finds the project open with no
/// tab and reopens it).
fn queue_job(queue: &mut VecDeque<IdeJob>, job: IdeJob) {
    // Only the latest active tab matters.
    if matches!(job, IdeJob::Activated { .. }) {
        queue.retain(|j| !matches!(j, IdeJob::Activated { .. }));
    }
    let waiting_reconcile = queue.iter().position(|j| *j == IdeJob::Reconcile);
    match (job, waiting_reconcile) {
        (IdeJob::Reconcile, Some(_)) => {}
        (IdeJob::Reconcile, None) => queue.push_back(IdeJob::Reconcile),
        (job, Some(at)) => queue.insert(at, job),
        (job, None) => queue.push_back(job),
    }
}

fn projects_file_sig() -> FileSig {
    let path = mnemos_ide_core::paths::ide_projects_file().ok()?;
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok(), meta.len()))
}

/// Map a plan's tab keys (indices into `refs`) back to tabs.
fn update_from_plan(
    plan: &ReconcilePlan,
    refs: &[IdeTabRef],
    primary: Option<WindowId>,
    open_projects: Vec<Project>,
) -> IdeUpdate {
    let relink: Vec<(IdeTabRef, Option<String>)> = plan
        .relink
        .iter()
        .filter_map(|(key, id)| Some((refs.get(*key)?.clone(), id.clone())))
        .collect();
    let close = plan
        .close
        .iter()
        .filter_map(|key| {
            let mut tab = refs.get(*key)?.clone();
            // Relinks are applied first, so the tab must carry its new id.
            if let Some((_, id)) = relink.iter().find(|(r, _)| *r == tab) {
                tab.project_id = id.clone();
            }
            Some(tab)
        })
        .collect();
    let open = match primary {
        Some(window_id) => plan
            .open
            .iter()
            .map(|o| IdeTabOpen {
                window_id,
                project_id: o.project_id.clone(),
                title: o.title.clone(),
                cwd: o.cwd.clone(),
                focus: false,
            })
            .collect(),
        None => {
            if !plan.open.is_empty() {
                log::info!(
                    target: "mnemos_ide",
                    "mnemos-ide: no window to open {} project tab(s) in; next reconcile will",
                    plan.open.len()
                );
            }
            Vec::new()
        }
    };
    IdeUpdate {
        open_projects: Some(open_projects),
        relink,
        close,
        open,
        focus: None,
    }
}

/// Close `id` everywhere unless one of `linked` (every tab's project id, in
/// every window) still shows it. Never removes the record: closing a tab is
/// not deleting a project. An id the registry does not know (an orphan from
/// an older build) is a no-op. Returns whether the flag changed.
pub(crate) fn close_unless_linked(
    store: &mut ProjectStore,
    id: &str,
    linked: &[String],
    now: DateTime<Utc>,
) -> bool {
    let Some(live) = store.resolve_id(id) else {
        return false;
    };
    let still_shown = linked
        .iter()
        .any(|tab_id| store.resolve_id(tab_id).as_deref() == Some(live.as_str()));
    // Already closed: re-stamping the clock would only cause a sync push.
    if still_shown || store.get(&live).and_then(Project::is_open) == Some(false) {
        return false;
    }
    store.set_open(&live, false, now)
}

/// Record `id` (following `replacedBy`) as this machine's active project.
/// An id the registry does not know is ignored. Local tab state only; the
/// synced registry is untouched.
pub(crate) fn set_active_project(store: &mut ProjectStore, id: &str) -> bool {
    let Some(live) = store.resolve_id(id) else {
        return false;
    };
    let state = store.tab_state.get_or_insert_with(Default::default);
    if state.active_project_id.as_deref() == Some(live.as_str()) {
        return false;
    }
    state.active_project_id = Some(live);
    true
}

/// When this process started using the project registry (pinned by the
/// first queued job).
pub(crate) fn ide_session_start() -> DateTime<Utc> {
    static START: std::sync::OnceLock<DateTime<Utc>> = std::sync::OnceLock::new();
    *START.get_or_init(Utc::now)
}

/// Mark `id` open again if it was closed before `session_start`. A close with
/// no recorded time counts as before. Returns whether the flag changed.
pub(crate) fn reopen_if_closed_before(
    store: &mut ProjectStore,
    id: &str,
    session_start: DateTime<Utc>,
    now: DateTime<Utc>,
) -> bool {
    let Some(live) = store.resolve_id(id) else {
        return false;
    };
    let Some(project) = store.get(&live) else {
        return false;
    };
    if project.is_open() != Some(false) {
        return false;
    }
    if project.open_changed_at().is_some_and(|at| at >= session_start) {
        return false;
    }
    store.set_open(&live, true, now)
}

/// Mark `id` open again (a closed tab was restored). Returns whether the flag
/// changed.
pub(crate) fn reopen(store: &mut ProjectStore, id: &str, now: DateTime<Utc>) -> bool {
    let Some(live) = store.resolve_id(id) else {
        return false;
    };
    if store.get(&live).and_then(Project::is_open) == Some(true) {
        return false;
    }
    store.set_open(&live, true, now)
}

/// A project added through the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AddedProject {
    pub id: String,
    /// The record's name; a claimed record keeps the name it already had.
    pub title: String,
}

/// The "Add project" picker: reuse the record checked out at `root` here,
/// else claim a record of the same repo that has no checkout here yet, else
/// register a new one. It ends up open, with this machine's path set.
pub(crate) fn add_project(
    store: &mut ProjectStore,
    root: &Path,
    fs: &dyn FsEnv,
    now: DateTime<Utc>,
) -> AddedProject {
    let platform = fs.platform();
    // A verbatim `\\?\C:\x` would be stored as a checkout no other code (nor
    // Claude Code's project slug) spells that way.
    let root = if platform == "win32" {
        PathBuf::from(mnemos_ide_projects::registry::strip_verbatim(
            &root.to_string_lossy(),
        ))
    } else {
        root.to_path_buf()
    };
    let name = root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "New Project".to_string());
    let remote = fs.git_remote(&root);
    // `register_or_claim` stamps `open = true` with its clock itself.
    // `_on(platform)` is `register_or_claim` for the platform `fs` acts out
    // (this machine's, for `RealFs`).
    let id = store.register_or_claim_on(platform, &root, &name, remote.as_deref(), now);
    let title = store.get(&id).map(|p| p.name.clone()).unwrap_or(name);
    AddedProject { id, title }
}

#[cfg(test)]
#[path = "ide_sync_tests.rs"]
mod tests;

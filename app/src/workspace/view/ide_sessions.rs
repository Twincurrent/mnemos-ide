//! Mnemos IDE: the past Claude Code sessions in the Agents panel, and resuming
//! one inside its project.
//!
//! **Listing.** Each project with a checkout on this machine gets its
//! sessions from `~/.claude/projects` (`mnemos_ide_sessions::SessionStore::list_for_root`,
//! sessions started in subfolders included), plus the folder the active
//! terminal is in when it belongs to no project. Sessions from the other
//! machine are on disk already: transcript sync (`mnemos_ide_sync::transcripts`)
//! pulls them into the slug folder of this machine's checkout and rewrites
//! their `cwd`, so they list and resume like local ones. A project with no
//! checkout here has nothing on disk; while one of its sessions runs, it
//! shows under the other machine's name in the panel instead.
//!
//! The scan runs off the UI thread, at most every few seconds, when the
//! Agents panel comes into view, on its refresh chip, every
//! [`TICK`] while it is visible, and when a local agent starts or ends while
//! it is visible. Parsed heads are cached by `(mtime, len)` in one store
//! shared by every window, so a rescan of unchanged transcripts is a stat per
//! file.
//!
//! **State.** A row's state is worked out when it is pushed, from live data:
//! running here (its id is a `CLIAgentSessionsModel` session's id), running
//! on another machine (its id is in that machine's `agents-live` snapshot),
//! folder missing (the mapped `cwd` is not a directory here), pulled
//! recently (its newest turns came from another machine within
//! `RECENT_ELSEWHERE` and nothing ran here since: that machine may still run
//! it unseen, so its clicks fork), else resumable. "from Windows" marks a
//! session whose newest turns were pulled in and that has not run here since.
//! It names the other machine's platform, not the machine: the pull record
//! keeps only the platform, so two machines of one platform look alike there
//! (the guards do not need more: any pull that left the file as it is counts).
//!
//! **Resuming** never opens a new project tab for a project that already has
//! one: the session runs in a new terminal tab inside the tab that shows the
//! project (matched by project id, else by a terminal under the project
//! root), in this window or another one, which is raised. Only a project
//! with no tab anywhere gets one, opened through the same path the reconcile
//! uses and marked open in the registry. A session outside every project
//! opens inside the active tab.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mnemos_ide_agent_restore::{
    resume_mode, AgentsLive, NewestTurn, ResumeMode, RECENT_ELSEWHERE,
};
use mnemos_ide_agents::{SessionGroup, SessionRow, SessionState};
use mnemos_ide_sessions::{is_within, SessionMeta, SessionStore};
use mnemos_ide_sync::live_agents::MachineAgents;
use mnemos_ide_sync::transcripts::PulledOrigin;
use mnemos_ui::r#async::Timer;
use mnemos_ui::{AppContext, SingletonEntity, ViewContext, ViewHandle, WeakViewHandle};

use crate::terminal::cli_agent_sessions::CLIAgentSessionsModel;
use crate::terminal::view::TerminalView;
use crate::workspace::ide_sync::IdeJob;
use crate::workspace::WorkspaceRegistry;

use super::Workspace;

/// How often the list is rescanned while the Agents panel is visible. A
/// transcript pulled from the other machine shows up within this.
const TICK: Duration = Duration::from_secs(30);
/// A scan that was not asked for explicitly is skipped this soon after the
/// last one.
const MIN_GAP: Duration = Duration::from_secs(5);
/// Sessions read per project. The panel shows five until "more".
const PER_PROJECT: usize = 20;

/// The workspace's view of the sessions list.
#[derive(Default)]
pub(super) struct IdeClaudeSessions {
    scan: Arc<SessionScan>,
    /// The other machines' live agents, from the last `LiveAgents` event
    /// (seeded from the registry when the window opens). `None` until one
    /// arrives: nothing is known, so no session is resumed in place.
    machines: Option<Arc<Vec<MachineAgents>>>,
    /// Sessions this window launched, by id, with the terminal running
    /// `claude --resume`. Until the agent hook reports the session (which a
    /// cold start can delay well past the panel's click guard), this is what
    /// keeps a second click from starting a second `claude` on it.
    launched: HashMap<String, WeakViewHandle<TerminalView>>,
    busy: bool,
    /// A scan was asked for while one ran: `Some(clear_cache)`.
    again: Option<bool>,
    last_scan: Option<Instant>,
    ticking: bool,
}

/// A folder whose sessions are listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ScanRoot {
    pub project_id: Option<String>,
    pub name: String,
    pub root: PathBuf,
}

/// One session as read from disk.
#[derive(Debug, Clone)]
pub(super) struct ScannedSession {
    pub meta: SessionMeta,
    /// Where it ran (its recorded `cwd`, else the root).
    pub cwd: PathBuf,
    pub cwd_exists: bool,
}

#[derive(Debug, Clone)]
pub(super) struct ScannedGroup {
    pub root: ScanRoot,
    pub sessions: Vec<ScannedSession>,
}

/// What a scan found.
#[derive(Debug, Clone, Default)]
pub(super) struct SessionScan {
    pub groups: Vec<ScannedGroup>,
    pub origins: HashMap<String, PulledOrigin>,
}

/// The parse cache, shared by every window.
fn store() -> &'static Mutex<SessionStore> {
    static STORE: OnceLock<Mutex<SessionStore>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(SessionStore::new()))
}

/// Read every root's sessions. Blocking; runs on a background thread.
///
/// Nested roots (a project inside another) each keep only the sessions
/// deepest in them, so a session is listed once. `extra` is the active
/// terminal's folder: when no root holds it, the nearest folder above it
/// that Claude Code has sessions for is listed too.
fn scan_sessions(
    roots: Vec<ScanRoot>,
    extra: Option<PathBuf>,
    store_dir: Option<PathBuf>,
    clear_cache: bool,
) -> SessionScan {
    let mut store = store().lock().unwrap_or_else(|e| e.into_inner());
    if clear_cache {
        store.clear_cache();
    }
    let groups = scan_with(&mut store, roots, extra);
    drop(store);
    let origins = store_dir
        .map(|dir| mnemos_ide_sync::transcripts::pulled_origins(&dir))
        .unwrap_or_default();
    SessionScan { groups, origins }
}

/// The groups of [`scan_sessions`], read through `store`.
fn scan_with(
    store: &mut SessionStore,
    roots: Vec<ScanRoot>,
    extra: Option<PathBuf>,
) -> Vec<ScannedGroup> {
    let mut roots: Vec<ScanRoot> = roots.into_iter().filter(|r| r.root.is_dir()).collect();
    // Deepest first, so a nested project claims its sessions before the
    // project around it sees them.
    roots.sort_by_key(|r| std::cmp::Reverse(r.root.components().count()));
    let mut seen: std::collections::HashSet<String> = Default::default();
    let mut groups = Vec::new();
    for root in &roots {
        // Uncapped: a nested project's sessions are in this list too, and
        // dropping them only after a cap could leave none of this root's own.
        // Every head is parsed either way; the cap only shortens the list.
        let sessions = match store.list_for_root(&root.root, usize::MAX) {
            Ok(sessions) => sessions,
            Err(e) => {
                log::debug!(target: "mnemos_ide", "sessions: {}: {e}", root.root.display());
                continue;
            }
        };
        // Every one is claimed, listed or not, so a nested project's
        // sessions past its cap do not turn up under the project around it.
        let mut sessions: Vec<SessionMeta> = sessions
            .into_iter()
            .filter(|meta| seen.insert(meta.id.clone()))
            .collect();
        sessions.truncate(PER_PROJECT);
        let sessions: Vec<ScannedSession> = sessions
            .into_iter()
            .map(|meta| scanned(meta, &root.root))
            .collect();
        if !sessions.is_empty() {
            groups.push(ScannedGroup {
                root: root.clone(),
                sessions,
            });
        }
    }
    if let Some(extra) = extra.filter(|p| !roots.iter().any(|r| is_within(p, &r.root))) {
        if let Ok((root, sessions)) = store.list_for_cwd_or_ancestor(&extra) {
            let sessions: Vec<ScannedSession> = sessions
                .into_iter()
                .filter(|meta| seen.insert(meta.id.clone()))
                .take(PER_PROJECT)
                .map(|meta| scanned(meta, &root))
                .collect();
            if !sessions.is_empty() {
                let name = root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| root.to_string_lossy().into_owned());
                groups.push(ScannedGroup {
                    root: ScanRoot {
                        project_id: None,
                        name,
                        root,
                    },
                    sessions,
                });
            }
        }
    }
    groups
}

fn scanned(meta: SessionMeta, root: &Path) -> ScannedSession {
    let cwd = meta.cwd.clone().unwrap_or_else(|| root.to_path_buf());
    // The cwd lies under a checkout this machine has open, so this stat
    // touches nothing a project tab would not.
    let cwd_exists = cwd.is_dir();
    ScannedSession {
        meta,
        cwd,
        cwd_exists,
    }
}

/// The panel rows for a scan, each with its state. `live_here` maps a
/// session id to the terminal view running it; `elsewhere` to the machine
/// running it and that session's Remote Control URL. A session neither
/// lists whose newest turns were pulled from another machine less than
/// [`RECENT_ELSEWHERE`] before `now` (by when that pull ran here,
/// `PulledOrigin::at`; the transcript's mtime only for a pull an older
/// build recorded) is `PulledRecently`: its clicks fork.
pub(super) fn session_groups(
    scan: &SessionScan,
    live_here: &HashMap<String, u64>,
    elsewhere: &HashMap<String, (String, Option<String>)>,
    now: SystemTime,
) -> Vec<SessionGroup> {
    let since = now.checked_sub(RECENT_ELSEWHERE).unwrap_or(UNIX_EPOCH);
    scan.groups
        .iter()
        .map(|group| SessionGroup {
            project_id: group.root.project_id.clone(),
            project: group.root.name.clone(),
            root: group.root.root.clone(),
            rows: group
                .sessions
                .iter()
                .map(|s| {
                    let id = &s.meta.id;
                    let state = if let Some(bits) = live_here.get(id) {
                        SessionState::LiveHere {
                            terminal_view_id_bits: *bits,
                        }
                    } else if let Some((machine, url)) = elsewhere.get(id) {
                        SessionState::LiveElsewhere {
                            machine: machine.clone(),
                            remote_url: url.clone(),
                        }
                    } else if !s.cwd_exists {
                        SessionState::FolderMissing
                    } else {
                        let newest = NewestTurn::from_file(
                            s.meta.size_bytes,
                            s.meta.modified,
                            scan.origins.get(id).map(|o| (o.bytes, o.at)),
                        );
                        // Not in `elsewhere`: no machine lists it as running.
                        // Whether its origin publishes at all does not
                        // change the outcome short of a listing
                        // (`resume_mode`), so `Unknown` is the honest input.
                        match resume_mode(Some(newest), since, AgentsLive::Unknown) {
                            ResumeMode::Fork => SessionState::PulledRecently,
                            ResumeMode::ResumeInPlace => SessionState::Resumable,
                        }
                    };
                    let origin = scan
                        .origins
                        .get(id)
                        .filter(|o| o.bytes == s.meta.size_bytes)
                        .map(|o| mnemos_ide_agents::platform_name(&o.platform).to_string());
                    SessionRow {
                        id: id.clone(),
                        title: s.meta.title.clone(),
                        title_is_ai: s.meta.title_is_ai,
                        branch: s.meta.git_branch.clone(),
                        modified: s.meta.modified,
                        cwd: s.cwd.clone(),
                        origin,
                        state,
                    }
                })
                .collect(),
        })
        .collect()
}

/// Session ids other machines are running, with the machine's label and the
/// session's Remote Control URL (only one that passes
/// `is_remote_control_url`).
pub(super) fn live_elsewhere(
    machines: &[MachineAgents],
) -> HashMap<String, (String, Option<String>)> {
    let mut out = HashMap::new();
    for machine in machines {
        for agent in &machine.agents {
            let Some(id) = agent.id.as_ref().filter(|id| !id.is_empty()) else {
                continue;
            };
            let url = agent
                .remote_url
                .clone()
                .filter(|u| mnemos_ide_agents::is_remote_control_url(u));
            out.insert(id.clone(), (machine.label.clone(), url));
        }
    }
    out
}

/// What `agents-live` says about session `id`. `machines` is the lists read
/// (`None`: none read); `origin_platform` the platform the session's newest
/// pulled turns came from. A list is definitive only for machines that
/// publish one: the live feed is off by default, so a machine missing from
/// the lists may well be running the session. Hence `NotListed` only when
/// a machine of the origin's platform (any machine, without an origin)
/// published a fresh list that does not hold `id`.
pub(super) fn agents_live_for(
    machines: Option<&[MachineAgents]>,
    id: &str,
    origin_platform: Option<&str>,
) -> AgentsLive {
    let Some(machines) = machines else {
        return AgentsLive::Unknown;
    };
    if machines
        .iter()
        .any(|m| m.agents.iter().any(|a| a.id.as_deref() == Some(id)))
    {
        return AgentsLive::ListedElsewhere;
    }
    let publishes = match origin_platform {
        Some(platform) => machines.iter().any(|m| m.platform == platform),
        None => !machines.is_empty(),
    };
    if publishes {
        AgentsLive::NotListed
    } else {
        AgentsLive::Unknown
    }
}

/// Whether agent restore should wait before deciding (pure, for tests).
/// Signed out, nothing more will come, so never; signed in, until both the
/// first `agents-live` answer and the first transcript pull are in, or the
/// deadline passes.
pub(super) fn restore_should_wait(
    signed_in: bool,
    live_pending: bool,
    transcripts_pending: bool,
    before_deadline: bool,
) -> bool {
    signed_in && before_deadline && (live_pending || transcripts_pending)
}

/// What agent restore does with one pane once its command is decided
/// (pure, for tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RestorePaneStep {
    /// Type the command now: the shell is up and the pane untouched.
    Run,
    /// The shell is still starting: look again shortly. A command parked
    /// now would sit in the input, and whatever the user typed before the
    /// shell came up would join it and run with it.
    Wait,
    /// Leave the pane alone and keep the entry for the next restore: the
    /// user has used the pane, or its shell never came up in time.
    Defer,
}

/// [`RestorePaneStep`] for a pane that is `untouched` (empty input, nothing
/// queued, nothing run), whose shell is `shell_ready` (its bootstrap precmd
/// is in, so a command runs at once), `before_give_up` while restore still
/// waits for a slow shell.
pub(super) fn restore_pane_step(
    untouched: bool,
    shell_ready: bool,
    before_give_up: bool,
) -> RestorePaneStep {
    if !untouched {
        RestorePaneStep::Defer
    } else if shell_ready {
        RestorePaneStep::Run
    } else if before_give_up {
        RestorePaneStep::Wait
    } else {
        RestorePaneStep::Defer
    }
}

/// How a plain click on a row the last scan called resumable should run,
/// from a fresh look at its transcript and the current live lists: the scan
/// can be 30 s old, and a pull since may have brought the other machine's
/// turns. `pulled` is what the last appending pull left (`bytes`, `at`).
pub(super) fn click_mode(
    transcript: Option<(u64, SystemTime)>,
    pulled: Option<(u64, Option<SystemTime>)>,
    live: AgentsLive,
    now: SystemTime,
) -> ResumeMode {
    let since = now.checked_sub(RECENT_ELSEWHERE).unwrap_or(UNIX_EPOCH);
    let newest = transcript.map(|(len, modified)| NewestTurn::from_file(len, modified, pulled));
    resume_mode(newest, since, live)
}

/// Session ids running in a terminal on this machine, any window.
fn live_here(ctx: &AppContext) -> HashMap<String, u64> {
    CLIAgentSessionsModel::as_ref(ctx)
        .iter()
        .filter_map(|(id, session)| {
            let sid = session.session_context.session_id.clone()?;
            Some((sid, id.as_usize() as u64))
        })
        .collect()
}

/// What a window knows about one of its tabs, for [`resume_tab`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TabFacts {
    pub project_id: Option<String>,
    pub cwds: Vec<PathBuf>,
}

/// How a tab is matched to a session's project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MatchBy {
    /// The tab carries the project's id.
    ProjectId,
    /// One of the tab's terminals is under the project root.
    Cwd,
}

/// The tab in which a session of the project (`project_id`, checked out at
/// `root`) should resume, matching `by`; the active tab when it matches,
/// else the first. By cwd, a tab linked to another project never matches.
/// Never the tab-strip's idea of the active project, which can be stale:
/// always what the tabs themselves carry.
pub(super) fn resume_tab(
    tabs: &[TabFacts],
    active: usize,
    project_id: Option<&str>,
    root: &Path,
    by: MatchBy,
) -> Option<usize> {
    let matches = |tab: &TabFacts| match by {
        MatchBy::ProjectId => project_id.is_some() && tab.project_id.as_deref() == project_id,
        // Never another project's tab, even with a terminal cd'd in here
        // (or a nested project's tab, whose terminals are all under root).
        MatchBy::Cwd => {
            (tab.project_id.is_none() || tab.project_id.as_deref() == project_id)
                && tab.cwds.iter().any(|cwd| is_within(cwd, root))
        }
    };
    if tabs.get(active).is_some_and(matches) {
        return Some(active);
    }
    tabs.iter().position(matches)
}

/// `<program> --resume <id>`, with `--fork-session` for a fork. `program` is
/// `claude` or the user's `agents.restore.claude_command`. The id has passed
/// `is_safe_session_id` (hex and dashes), so no shell needs quoting.
pub(super) fn resume_command(program: &str, session_id: &str, fork: bool) -> String {
    if fork {
        mnemos_ide_agent_restore::fork_command_with(program, session_id, false)
    } else {
        mnemos_ide_agent_restore::resume_command_with(program, session_id, false)
    }
}

/// A resume from the list was launched (`launched`) or not. Only the
/// original resumed in place makes restore's carried-over entry for it
/// obsolete: after a fork (a new id) the original is still the pane's
/// session, and a launch that never happened resumed nothing.
pub(super) fn note_resumed_from_list(session_id: &str, fork: bool, launched: bool) {
    if launched && !fork {
        mnemos_ide_agent_restore::forget_deferred(session_id);
    }
}

/// A resume, as the Agents panel asked for it.
pub(super) struct ResumeRequest<'a> {
    pub session_id: &'a str,
    pub cwd: &'a Path,
    pub project_root: &'a Path,
    pub project_id: Option<&'a str>,
    pub fork: bool,
}

impl Workspace {
    /// Start the visible-panel rescan timer. Idempotent.
    pub(super) fn ide_start_claude_sessions_tick(&mut self, ctx: &mut ViewContext<Self>) {
        if self.ide_claude_sessions.ticking || cfg!(test) {
            return;
        }
        self.ide_claude_sessions.ticking = true;
        // The first look comes soon: a window restored with the Agents panel
        // open gets no "panel shown" event.
        self.ide_schedule_claude_sessions_tick(Duration::from_secs(2), ctx);
    }

    fn ide_schedule_claude_sessions_tick(&mut self, after: Duration, ctx: &mut ViewContext<Self>) {
        let _ = ctx.spawn(
            async move {
                Timer::after(after).await;
            },
            |me, _, ctx| {
                if me.ide_claude_sessions_visible(ctx) {
                    me.ide_refresh_claude_sessions(false, ctx);
                    // The panel has no age timer of its own for sessions.
                    me.left_panel_view
                        .update(ctx, |panel, pctx| panel.repaint_session_ages(pctx));
                    me.ide_refresh_team_board(ctx);
                }
                me.ide_schedule_claude_sessions_tick(TICK, ctx);
            },
        );
    }

    /// Re-read the active project's team board (`team.sh`) off the main
    /// thread and show it as the Agents panel's TEAM section.
    fn ide_refresh_team_board(&mut self, ctx: &mut ViewContext<Self>) {
        let root = self.left_panel_view.as_ref(ctx).agents_active_project_path(ctx);
        let _ = ctx.spawn(
            async move {
                if root.is_empty() {
                    return Vec::new();
                }
                mnemos_ide_team::board::read(Path::new(&root))
                    .into_iter()
                    .map(|r| mnemos_ide_agents::TeamRow {
                        name: r.name,
                        state: r.state,
                        task: r.task,
                        sha: r.sha,
                    })
                    .collect()
            },
            |me, rows, ctx| {
                me.left_panel_view
                    .update(ctx, |panel, pctx| panel.set_team_board(rows, pctx));
            },
        );
    }

    /// The Agents panel is on screen in this window.
    pub(crate) fn ide_claude_sessions_visible(&self, ctx: &AppContext) -> bool {
        // The left panel's open state lives on each tab's pane group.
        self.tabs
            .get(self.active_tab_index)
            .is_some_and(|tab| tab.pane_group.as_ref(ctx).left_panel_open)
            && self.left_panel_view.as_ref(ctx).shows_agents()
    }

    /// Rescan the transcripts. `force` (the refresh chip) drops the parse
    /// cache and ignores [`MIN_GAP`].
    pub(super) fn ide_refresh_claude_sessions(&mut self, force: bool, ctx: &mut ViewContext<Self>) {
        // The list is on screen: the other machines' live agents are wanted
        // too (the registry reads them only while some panel shows them).
        WorkspaceRegistry::handle(ctx).update(ctx, |registry, ctx| registry.live_agents_wanted(ctx));
        let state = &mut self.ide_claude_sessions;
        if state.busy {
            state.again = Some(state.again.unwrap_or(false) || force);
            return;
        }
        if !force && state.last_scan.is_some_and(|at| at.elapsed() < MIN_GAP) {
            return;
        }
        self.ide_start_claude_sessions_scan(force, ctx);
    }

    fn ide_start_claude_sessions_scan(&mut self, clear_cache: bool, ctx: &mut ViewContext<Self>) {
        // Paths are this machine's checkouts (`local_path`), never `path`.
        let roots: Vec<ScanRoot> = mnemos_ide_projects::ProjectStore::load_blocking()
            .map(|store| {
                store
                    .projects
                    .iter()
                    .filter_map(|p| {
                        Some(ScanRoot {
                            project_id: Some(p.id.clone()),
                            name: p.name.clone(),
                            root: p.local_path()?,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let extra = self
            .read_from_active_terminal_view(ctx, |tv| tv.pwd())
            .flatten()
            .map(PathBuf::from);
        self.ide_claude_sessions.busy = true;
        let _ = ctx.spawn(
            async move {
                let store_dir = mnemos_ide_core::paths::ide_data_dir()
                    .ok()
                    .map(|dir| dir.join("store"));
                scan_sessions(roots, extra, store_dir, clear_cache)
            },
            |me, scan, ctx| {
                let state = &mut me.ide_claude_sessions;
                state.busy = false;
                state.last_scan = Some(Instant::now());
                state.scan = Arc::new(scan);
                me.ide_push_claude_sessions(ctx);
                if let Some(clear_cache) = me.ide_claude_sessions.again.take() {
                    me.ide_start_claude_sessions_scan(clear_cache, ctx);
                }
            },
        );
    }

    /// Rebuild the rows' states from the last scan and push them.
    pub(super) fn ide_push_claude_sessions(&mut self, ctx: &mut ViewContext<Self>) {
        let groups = session_groups(
            &self.ide_claude_sessions.scan,
            &live_here(ctx),
            &live_elsewhere(
                self.ide_claude_sessions
                    .machines
                    .as_deref()
                    .map_or(&[], Vec::as_slice),
            ),
            SystemTime::now(),
        );
        self.left_panel_view.update(ctx, |panel, pctx| {
            panel.set_session_groups(groups, pctx);
        });
    }

    /// The other machines' live agents changed.
    pub(super) fn ide_claude_sessions_machines(
        &mut self,
        machines: Arc<Vec<MachineAgents>>,
        ctx: &mut ViewContext<Self>,
    ) {
        self.ide_claude_sessions.machines = Some(machines);
        self.ide_push_claude_sessions(ctx);
    }

    /// Resume a session (see the module docs for where it opens).
    pub(super) fn ide_resume_claude_session(
        &mut self,
        req: ResumeRequest<'_>,
        ctx: &mut ViewContext<Self>,
    ) {
        // The id is a file stem from disk; it must never reach a shell unchecked.
        if !mnemos_ide_sessions::is_safe_session_id(req.session_id) {
            log::warn!(target: "mnemos_ide", "sessions: refusing to resume implausible session id {:?}", req.session_id);
            return;
        }
        // The panel's state may be a scan old: check again.
        if let Some(bits) = live_here(ctx).get(req.session_id) {
            self.activate_cli_agent_terminal(*bits, ctx);
            return;
        }
        if let Some(terminal) = self.ide_launched_terminal(req.session_id, ctx) {
            // Launched from here and still starting (or running) there.
            self.activate_cli_agent_terminal(terminal.id().as_usize() as u64, ctx);
            return;
        }
        if !req.fork {
            match &self.ide_claude_sessions.machines {
                None => {
                    log::info!(target: "mnemos_ide", "sessions: not resuming {} in place yet, the other machines' agents are not known", req.session_id);
                    return;
                }
                Some(machines) if live_elsewhere(machines).contains_key(req.session_id) => {
                    log::info!(target: "mnemos_ide", "sessions: not resuming {} in place, another machine is running it", req.session_id);
                    return;
                }
                Some(_) => {}
            }
        }
        // A plain click on a row the last scan called resumable: look again
        // now. A pull since that scan may have brought the other machine's
        // turns, and then this resumes a fork, as the row would have said.
        let mut fork = req.fork;
        if !fork {
            let path = self
                .ide_claude_sessions
                .scan
                .groups
                .iter()
                .flat_map(|g| g.sessions.iter())
                .find(|s| s.meta.id == req.session_id)
                .map(|s| s.meta.path.clone());
            let transcript = path.and_then(|p| {
                let meta = std::fs::metadata(p).ok()?;
                Some((meta.len(), meta.modified().unwrap_or(UNIX_EPOCH)))
            });
            // Cached by the state file's stamp: a stat, not a parse, per click.
            let origin = mnemos_ide_core::paths::ide_data_dir()
                .ok()
                .and_then(|dir| {
                    mnemos_ide_sync::transcripts::pulled_origins_cached(&dir.join("store"))
                        .get(req.session_id)
                        .cloned()
                });
            let machines = WorkspaceRegistry::as_ref(ctx)
                .live_agents_last()
                .or_else(|| self.ide_claude_sessions.machines.clone());
            let live = agents_live_for(
                machines.as_deref().map(Vec::as_slice),
                req.session_id,
                origin.as_ref().map(|o| o.platform.as_str()),
            );
            let pulled = origin.as_ref().map(|o| (o.bytes, o.at));
            if click_mode(transcript, pulled, live, SystemTime::now()) == ResumeMode::Fork {
                log::info!(target: "mnemos_ide", "sessions: resuming a fork of {}, not the original: another machine may be running it (agents-live: {live:?})", req.session_id);
                fork = true;
                // The row is out of date: rescan so it says so.
                self.ide_refresh_claude_sessions(true, ctx);
            }
        }
        let req = ResumeRequest { fork, ..req };
        // `claude --resume` looks the session up under the slug of the folder
        // it runs in, so it has to be the session's own folder.
        if !req.cwd.is_dir() {
            log::info!(target: "mnemos_ide", "sessions: {} ran in {}, which is not here", req.session_id, req.cwd.display());
            return;
        }
        let cwd = req.cwd.to_path_buf();
        let command = resume_command(&Workspace::ide_claude_program(ctx), req.session_id, req.fork);
        log::info!(target: "mnemos_ide", "sessions: resuming {} (fork: {}) in {}", req.session_id, req.fork, cwd.display());

        // An existing tab of the project: by id first, in any window, then by
        // a terminal under the project root.
        let session_id = req.session_id.to_string();
        for by in [MatchBy::ProjectId, MatchBy::Cwd] {
            let here: Vec<TabFacts> = self.ide_resume_tab_facts(ctx);
            if let Some(index) = resume_tab(
                &here,
                self.active_tab_index,
                req.project_id,
                req.project_root,
                by,
            ) {
                let terminal = self.ide_run_in_tab(index, cwd.clone(), &command, ctx);
                self.ide_note_launched(session_id, req.fork, terminal);
                return;
            }
            // This workspace is being updated, so `all_workspaces` leaves it out.
            let others = WorkspaceRegistry::as_ref(ctx).all_workspaces(ctx);
            for (window_id, workspace) in others {
                let found = {
                    let ws = workspace.as_ref(ctx);
                    resume_tab(
                        &ws.ide_resume_tab_facts(ctx),
                        ws.active_tab_index,
                        req.project_id,
                        req.project_root,
                        by,
                    )
                };
                if let Some(index) = found {
                    let cwd = cwd.clone();
                    let terminal = workspace.update(ctx, |ws, wctx| {
                        ws.ide_run_in_tab(index, cwd, &command, wctx)
                    });
                    self.ide_note_launched(session_id, req.fork, terminal);
                    ctx.windows().show_window_and_focus_app(window_id);
                    return;
                }
            }
        }

        // A registered project with no tab anywhere: open its tab here, with
        // the session in its first terminal, and mark it open so the
        // reconcile does not open a second one.
        let project = req.project_id.and_then(|id| {
            let store = mnemos_ide_projects::ProjectStore::load_blocking().ok()?;
            let project = store.get(id)?;
            Some((project.id.clone(), project.name.clone()))
        });
        if let Some((project_id, name)) = project {
            let pane_group_id = self.ide_add_project_tab(&project_id, &name, Some(cwd), ctx);
            let Some(index) = self.ide_tab_index(pane_group_id) else {
                return;
            };
            let pane_group = self.tabs[index].pane_group.clone();
            let terminal = pane_group
                .as_ref(ctx)
                .terminal_views(ctx)
                .into_iter()
                .next();
            if let Some(terminal) = terminal {
                terminal.update(ctx, |terminal, tctx| {
                    terminal.execute_command_or_set_pending(&command, tctx);
                });
                self.ide_note_launched(session_id, req.fork, Some(terminal));
            }
            self.activate_tab(index, ctx);
            self.ide_apply_horizontal_tab_mode(ctx);
            Self::ide_request(IdeJob::TabReopened { project_id }, ctx);
            ctx.dispatch_global_action("workspace:save_app", ());
            return;
        }

        // Outside every project: inside the tab the user is in.
        let active = self.active_tab_index;
        if active < self.tabs.len() {
            let terminal = self.ide_run_in_tab(active, cwd, &command, ctx);
            self.ide_note_launched(session_id, req.fork, terminal);
        }
    }

    fn ide_note_launched(
        &mut self,
        session_id: String,
        fork: bool,
        terminal: Option<ViewHandle<TerminalView>>,
    ) {
        note_resumed_from_list(&session_id, fork, terminal.is_some());
        if let Some(terminal) = terminal {
            self.ide_claude_sessions
                .launched
                .insert(session_id, terminal.downgrade());
        }
    }

    /// The terminal some window launched `session_id` in, while it still
    /// runs that command (pending through shell startup, then executing).
    /// Entries whose terminal is gone or done are dropped on the way.
    fn ide_launched_terminal(
        &mut self,
        session_id: &str,
        ctx: &mut ViewContext<Self>,
    ) -> Option<ViewHandle<TerminalView>> {
        let busy = |terminal: &WeakViewHandle<TerminalView>, ctx: &AppContext| {
            let terminal = terminal.upgrade(ctx)?;
            let view = terminal.as_ref(ctx);
            (view.has_pending_command_or_awaiting_completion(ctx) || view.is_long_running())
                .then_some(terminal)
        };
        let mut found = None;
        let launched = std::mem::take(&mut self.ide_claude_sessions.launched);
        for (id, terminal) in launched {
            if let Some(live) = busy(&terminal, ctx) {
                if id == session_id {
                    found = Some(live);
                }
                self.ide_claude_sessions.launched.insert(id, terminal);
            }
        }
        if found.is_some() {
            return found;
        }
        // Another window's launch. This workspace is being updated, so
        // `all_workspaces` leaves it out.
        WorkspaceRegistry::as_ref(ctx)
            .all_workspaces(ctx)
            .into_iter()
            .find_map(|(_, workspace)| {
                let terminal = workspace
                    .as_ref(ctx)
                    .ide_claude_sessions
                    .launched
                    .get(session_id)?
                    .clone();
                busy(&terminal, ctx)
            })
    }

    /// [`TabFacts`] for this window's tabs, in tab order.
    fn ide_resume_tab_facts(&self, ctx: &AppContext) -> Vec<TabFacts> {
        self.ide_tab_facts(ctx)
            .into_iter()
            .map(|info| TabFacts {
                project_id: info.project_id,
                cwds: info.cwds,
            })
            .collect()
    }

    /// Run `command` in a new terminal tab at `cwd` inside tab `index`, and
    /// switch to it. Returns the new terminal.
    fn ide_run_in_tab(
        &mut self,
        index: usize,
        cwd: PathBuf,
        command: &str,
        ctx: &mut ViewContext<Self>,
    ) -> Option<ViewHandle<TerminalView>> {
        let pane_group = self.tabs.get(index).map(|t| t.pane_group.clone())?;
        let terminal = pane_group.update(ctx, |pane_group, pctx| {
            pane_group.ide_add_terminal_tab_running(cwd, command, pctx)
        });
        self.activate_tab(index, ctx);
        terminal
    }
}

#[cfg(test)]
#[path = "ide_sessions_tests.rs"]
mod tests;

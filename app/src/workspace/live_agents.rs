//! Mnemos IDE: the agents panel across machines.
//!
//! Every [`TICK`] the registry may do two things, each with its own gate:
//!
//! - **Publish** this machine's CLI agents to admin (`agents-live:<host>`,
//!   see `mnemos_ide_sync::live_agents`) when they changed or the last publish
//!   is a [`HEARTBEAT`] old. Only while the live-feed setting is on
//!   (`GeneralSettings::live_feed_enabled`, off by default; installs that were
//!   signed in when it arrived keep it on). Publishing does not depend on the
//!   panel being visible: other readers (Iris) read this key. What goes out is
//!   status only unless `sync.live_feed.share_prompt_text` is on, and secret
//!   patterns are redacted either way (`MachineAgents::for_publish`). When the
//!   setting is off, this machine's key is deleted once per login.
//! - **Read** the other machines' lists back, only while an Agents panel is on
//!   screen in some window (or a window asked for a fresh list, see
//!   [`WorkspaceRegistry::live_agents_wanted`]). The result goes out as
//!   [`WorkspaceRegistryEvent::LiveAgents`]; each workspace hands it to its
//!   agents panel.
//!
//! The state is per admin profile, so a teammate never sees it.
//!
//! It talks through the app's own mnemos-ide-sync manager (`AuthManager`), so a
//! sign-out or an account switch takes effect on the next tick and nothing
//! here ever holds or refreshes a login of its own. Signed out of admin sync
//! it shows no other machines; offline it keeps the last list until it goes
//! stale. Either way it tries again on the next tick. Sign-out deletes this
//! machine's key (`AuthManager::sign_out_of_admin`).

use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use chrono::Utc;
use mnemos_ide_sync::live_agents::{self, LiveAgent, MachineAgents};
use mnemos_ui::r#async::Timer;
use mnemos_ui::{AppContext, ModelContext, SingletonEntity};

use super::ide_sync::WorkspaceRegistryEvent;
use super::WorkspaceRegistry;
use crate::auth::auth_manager::AuthManager;
use crate::terminal::cli_agent_sessions::{CLIAgentSessionStatus, CLIAgentSessionsModel};
use crate::terminal::general_settings::GeneralSettings;

const TICK: Duration = Duration::from_secs(15);
/// Republish an unchanged list this often, so other machines can tell this
/// one is still there (they drop a list after `live_agents::STALE_AFTER`).
const HEARTBEAT: Duration = Duration::from_secs(60);
/// A window's request for a fresh list ([`WorkspaceRegistry::live_agents_wanted`])
/// keeps reading on for this long.
const WANTED_FOR: Duration = Duration::from_secs(45);

#[derive(Default)]
pub(crate) struct LiveAgentsSync {
    started: bool,
    /// The sync manager `last_pushed` was published through. A different
    /// one is a new login (maybe another account), which publishes afresh.
    manager: Option<Weak<mnemos_ide_sync::SyncManager>>,
    /// What was last published (agents and whether text was shared) and when.
    last_pushed: Option<(Vec<LiveAgent>, bool, Instant)>,
    /// With the feed off: this machine's key was deleted under the current
    /// manager (or is known to be absent), so it is not deleted again.
    cleared: bool,
    /// A tick's request is in flight.
    busy: bool,
    /// The last list sent out, for a window that opens after it (see
    /// [`WorkspaceRegistry::live_agents_last`]).
    last: Option<Arc<Vec<MachineAgents>>>,
    /// `last` came from a list actually read from admin, not from the empty
    /// list sent out when signed out or offline from the start.
    last_read: bool,
    /// Read the other machines' lists until then, visible panel or not.
    wanted_until: Option<Instant>,
    /// This machine's random id (`mnemos_ide_sync::machine`), read once.
    machine_id: Option<Option<String>>,
}

/// What is known about the other machines' live agents so far.
#[derive(Debug, Clone)]
pub(crate) enum LiveAgentsAnswer {
    /// No answer yet.
    Pending,
    /// An answer came, but no list was read: signed out, or offline from the
    /// start.
    NotRead,
    /// The latest list read from admin (it outlives a later failed read).
    Read(Arc<Vec<MachineAgents>>),
}

/// What a tick learned about the other machines' lists.
#[derive(Debug)]
pub(crate) enum TickOutcome {
    /// No usable login: nothing can be read.
    SignedOut,
    /// The lists were read.
    Read(Vec<MachineAgents>),
    /// Reading them failed (offline, server error).
    ReadFailed,
}

impl LiveAgentsSync {
    fn answer(&self) -> LiveAgentsAnswer {
        match &self.last {
            None => LiveAgentsAnswer::Pending,
            Some(_) if !self.last_read => LiveAgentsAnswer::NotRead,
            Some(last) => LiveAgentsAnswer::Read(last.clone()),
        }
    }

    /// Record `outcome`; returns the list to send out, if it changed (pure,
    /// for tests).
    fn apply(&mut self, outcome: TickOutcome) -> Option<Arc<Vec<MachineAgents>>> {
        let (machines, read) = match outcome {
            TickOutcome::Read(machines) => (machines, true),
            // Signed out: stop showing other machines (until a new sign-in
            // starts a new manager), and nothing read counts any more.
            TickOutcome::SignedOut => {
                if self.last.as_ref().is_some_and(|l| l.is_empty()) {
                    self.last_read = false;
                    return None;
                }
                (Vec::new(), false)
            }
            // A failed read keeps the last list read (until it goes stale on
            // the other side). Offline from the start, nothing is known: that
            // is still an answer, so nothing waits on it forever.
            TickOutcome::ReadFailed => {
                if self.last.is_some() {
                    return None;
                }
                (Vec::new(), false)
            }
        };
        let machines = Arc::new(machines);
        self.last = Some(machines.clone());
        self.last_read = read;
        Some(machines)
    }
}

/// What one tick does, given the gates (pure, for tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TickPlan {
    pub publish: bool,
    pub delete: bool,
    pub read: bool,
}

/// `enabled`: the live-feed setting. `changed_or_due`: the list or the
/// share setting changed, or the heartbeat is due. `cleared`: the key was
/// already deleted under this login. `visible`: an Agents panel is on screen
/// or a window asked for a fresh list.
pub(crate) fn plan_tick(
    enabled: bool,
    changed_or_due: bool,
    cleared: bool,
    visible: bool,
) -> TickPlan {
    TickPlan {
        publish: enabled && changed_or_due,
        delete: !enabled && !cleared,
        read: visible,
    }
}

impl WorkspaceRegistry {
    /// The other machines' live agents as last sent out, `None` before the
    /// first answer. A new window starts from this instead of waiting a tick.
    pub(crate) fn live_agents_last(&self) -> Option<Arc<Vec<MachineAgents>>> {
        self.live_agents.last.clone()
    }

    /// A window is showing (or about to show) the other machines' agents:
    /// read their lists for a while, and now if nothing was read yet. "Now"
    /// does not wait for [`Self::live_agents_start`]: agent restore asks
    /// while the first window is still being built, before polling starts,
    /// and must not sit out the first scheduled tick for its answer.
    pub(crate) fn live_agents_wanted(&mut self, ctx: &mut ModelContext<Self>) {
        let first = self
            .live_agents
            .wanted_until
            .is_none_or(|until| until <= Instant::now());
        self.live_agents.wanted_until = Some(Instant::now() + WANTED_FOR);
        if first {
            self.live_agents_tick(ctx);
        }
    }

    /// Mnemos IDE: what agent restore can go on (see [`LiveAgentsAnswer`]).
    pub(crate) fn live_agents_answer(&self) -> LiveAgentsAnswer {
        self.live_agents.answer()
    }

    /// Apply what a tick found and send out the list, if there is one to send.
    fn live_agents_outcome(&mut self, outcome: TickOutcome, ctx: &mut ModelContext<Self>) {
        if let Some(machines) = self.live_agents.apply(outcome) {
            ctx.emit(WorkspaceRegistryEvent::LiveAgents(machines));
        }
    }

    pub(crate) fn live_agents_start(&mut self, ctx: &mut ModelContext<Self>) {
        if self.live_agents.started || cfg!(test) {
            return;
        }
        self.live_agents.started = true;
        self.live_agents_schedule(Duration::from_secs(3), ctx);
    }

    fn live_agents_schedule(&mut self, after: Duration, ctx: &mut ModelContext<Self>) {
        ctx.spawn(
            async move {
                Timer::after(after).await;
            },
            |registry, _, ctx| {
                registry.live_agents_tick(ctx);
                registry.live_agents_schedule(TICK, ctx);
            },
        );
    }

    /// Whether some window shows an Agents panel, or asked for the list.
    fn live_agents_visible(&self, ctx: &AppContext) -> bool {
        self.live_agents
            .wanted_until
            .is_some_and(|until| until > Instant::now())
            || self
                .all_workspaces(ctx)
                .iter()
                .any(|(_, workspace)| workspace.as_ref(ctx).ide_claude_sessions_visible(ctx))
    }

    fn live_agents_tick(&mut self, ctx: &mut ModelContext<Self>) {
        if self.live_agents.busy {
            return;
        }
        let manager = AuthManager::as_ref(ctx)
            .ide_sync_manager()
            // Mnemos IDE: a manager stopped by a refused session or a refused
            // sync protocol would only fail every push.
            .filter(|m| !m.auth_state().halts_sync());
        let same_manager = match (&manager, &self.live_agents.manager) {
            (Some(m), Some(prev)) => Weak::ptr_eq(&Arc::downgrade(m), prev),
            _ => false,
        };
        if !same_manager {
            self.live_agents.last_pushed = None;
            self.live_agents.cleared = false;
            self.live_agents.manager = manager.as_ref().map(Arc::downgrade);
        }
        // Signed out, or the server ended the session: stop showing other
        // machines until a new sign-in starts a new manager.
        let Some(manager) = manager else {
            self.live_agents_outcome(TickOutcome::SignedOut, ctx);
            return;
        };
        let (enabled, share_text) = {
            let settings = GeneralSettings::as_ref(ctx);
            (settings.live_feed_enabled(), settings.live_feed_share_text_enabled())
        };
        let host = gethostname::gethostname().to_string_lossy().into_owned();
        let machine = machine_key_for(&host, mnemos_ide_core::paths::data_profile().as_deref());
        let agents = local_agents(ctx);
        let changed_or_due = match &self.live_agents.last_pushed {
            Some((prev, shared, at)) => {
                *prev != agents || *shared != share_text || at.elapsed() >= HEARTBEAT
            }
            None => true,
        };
        let plan = plan_tick(
            enabled,
            changed_or_due,
            self.live_agents.cleared,
            self.live_agents_visible(ctx),
        );
        if !plan.publish && !plan.delete && !plan.read {
            return;
        }
        let machine_id = self
            .live_agents
            .machine_id
            .get_or_insert_with(|| {
                mnemos_ide_sync::machine::MachineRecord::this_machine(&host).map(|r| r.id)
            })
            .clone();
        let own_id = machine_id.clone();
        let snapshot = MachineAgents {
            machine: machine.clone(),
            label: host.trim_end_matches(".local").to_string(),
            platform: mnemos_ide_core::registry::current_platform().to_string(),
            updated_at: Utc::now(),
            agents: agents.clone(),
            machine_id,
        }
        .for_publish(share_text);
        let client = manager.client.clone();
        let sent_with = Arc::downgrade(&manager);
        self.live_agents.busy = true;
        ctx.spawn(
            async move {
                if plan.publish {
                    client
                        .put_live_agents(&snapshot)
                        .await
                        .map_err(|e| format!("publish: {e}"))?;
                }
                if plan.delete {
                    client
                        .delete_live_agents(&machine)
                        .await
                        .map_err(|e| format!("delete: {e}"))?;
                }
                if !plan.read {
                    return Ok(None);
                }
                let states = client
                    .list_live_agents()
                    .await
                    .map_err(|e| format!("list: {e}"))?;
                Ok(Some(live_agents::others(
                    states,
                    &machine,
                    own_id.as_deref(),
                    Utc::now(),
                )))
            },
            move |registry, result: Result<Option<Vec<MachineAgents>>, String>, ctx| {
                registry.live_agents.busy = false;
                // The login changed while the request was in flight: drop
                // the answer, the next tick starts over with the new one.
                let current = registry.live_agents.manager.as_ref();
                if !current.is_some_and(|m| Weak::ptr_eq(m, &sent_with)) {
                    return;
                }
                match result {
                    Ok(others) => {
                        if plan.publish {
                            registry.live_agents.last_pushed =
                                Some((agents, share_text, Instant::now()));
                            registry.live_agents.cleared = false;
                        }
                        if plan.delete {
                            registry.live_agents.cleared = true;
                            registry.live_agents.last_pushed = None;
                        }
                        if let Some(others) = others {
                            registry.live_agents_outcome(TickOutcome::Read(others), ctx);
                        }
                    }
                    Err(e) => {
                        log::debug!(target: "mnemos_ide", "live agents: {e}");
                        if plan.read {
                            registry.live_agents_outcome(TickOutcome::ReadFailed, ctx);
                        }
                    }
                }
            },
        );
    }
}

/// This machine's live-feed key suffix, as [`machine_key_for`] computes it
/// for this host and data profile. Used on sign-out to delete the key (not
/// under `cfg(test)`, where sign-out never reaches the server).
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn this_machine_key() -> String {
    let host = gethostname::gethostname().to_string_lossy().into_owned();
    machine_key_for(&host, mnemos_ide_core::paths::data_profile().as_deref())
}

/// This instance's key suffix. The installed app (no data profile) uses the
/// bare host key, which is what admin computes for this host; a profiled
/// build (`./script/run` sets `dev`) gets its own key so it does not
/// overwrite the installed app's list.
fn machine_key_for(host: &str, profile: Option<&str>) -> String {
    let key = live_agents::machine_key(host);
    match profile.filter(|p| !p.is_empty()) {
        Some(profile) => format!("{key}-{}", live_agents::machine_key(profile)),
        None => key,
    }
}

/// This machine's CLI agents, as the other machines will see them.
fn local_agents(ctx: &AppContext) -> Vec<LiveAgent> {
    let model = CLIAgentSessionsModel::as_ref(ctx);
    let mut agents: Vec<LiveAgent> = model
        .iter()
        .map(|(_, session)| {
            let c = &session.session_context;
            let project = c
                .project
                .clone()
                .filter(|p| !p.is_empty())
                .or_else(|| {
                    c.cwd.as_deref().and_then(|cwd| {
                        cwd.rsplit(['/', '\\'])
                            .find(|s| !s.is_empty())
                            .map(str::to_string)
                    })
                })
                .unwrap_or_default();
            let status = match &session.status {
                CLIAgentSessionStatus::Blocked { .. } => "blocked",
                CLIAgentSessionStatus::InProgress => "running",
                CLIAgentSessionStatus::Success => "idle",
            };
            // Waiting on background tasks is `running`; `tool` spells it out
            // for readers that predate `background_tasks`.
            let background_tasks = match session.status {
                CLIAgentSessionStatus::InProgress => c.background_tasks,
                _ => 0,
            };
            let tool = match background_tasks {
                0 => c.tool_name.clone(),
                1 => Some("1 background task".to_string()),
                n => Some(format!("{n} background tasks")),
            };
            LiveAgent {
                id: c.session_id.clone(),
                project,
                agent: format!("{:?}", session.agent).to_lowercase(),
                status: status.to_string(),
                query: c.query.clone(),
                response: c.response.clone(),
                tool,
                last_activity: c.last_event_at.map(chrono::DateTime::<Utc>::from),
                remote_url: c.remote_url.clone(),
                background_tasks,
            }
        })
        .collect();
    agents.sort_by(|a, b| {
        a.project
            .cmp(&b.project)
            .then(a.last_activity.cmp(&b.last_activity))
    });
    agents
}

/// Another machine's list as panel rows. Remote rows have no terminal here;
/// their id only has to be unique within the list.
pub(crate) fn remote_machines(machines: &[MachineAgents]) -> Vec<mnemos_ide_agents::RemoteMachine> {
    machines
        .iter()
        .map(|m| mnemos_ide_agents::RemoteMachine {
            label: m.label.clone(),
            platform: m.platform.clone(),
            rows: m
                .agents
                .iter()
                .enumerate()
                .map(|(i, a)| mnemos_ide_agents::AgentRow {
                    terminal_view_id_bits: i as u64,
                    agent: match a.agent.as_str() {
                        "claude" => mnemos_ide_agents::AgentKind::Claude,
                        "codex" => mnemos_ide_agents::AgentKind::Codex,
                        "gemini" => mnemos_ide_agents::AgentKind::Gemini,
                        "opencode" => mnemos_ide_agents::AgentKind::OpenCode,
                        _ => mnemos_ide_agents::AgentKind::Other,
                    },
                    status: match a.status.as_str() {
                        "blocked" => mnemos_ide_agents::AgentStatus::Blocked { message: None },
                        "running" => mnemos_ide_agents::AgentStatus::InProgress,
                        _ => mnemos_ide_agents::AgentStatus::Success,
                    },
                    project: a.project.clone(),
                    cwd: None,
                    query: a.query.clone(),
                    response: a.response.clone(),
                    tool_name: a.tool.clone(),
                    last_activity: a.last_activity.map(std::time::SystemTime::from),
                    background_tasks: a.background_tasks,
                    remote_url: a
                        .remote_url
                        .clone()
                        .filter(|url| mnemos_ide_agents::is_remote_control_url(url)),
                    session_id: a.id.clone(),
                })
                .collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_key_without_profile_is_the_host_key() {
        assert_eq!(
            machine_key_for("Alice-MacBook.local", None),
            "alice-macbook"
        );
        assert_eq!(
            machine_key_for("Alice-MacBook.local", None),
            live_agents::machine_key("Alice-MacBook.local")
        );
        assert_eq!(machine_key_for("pc", Some("")), "pc");
    }

    #[test]
    fn remote_rows_keep_only_claude_remote_control_urls() {
        let agent = |url: Option<&str>, background_tasks: u32| LiveAgent {
            id: Some(format!("sid-{background_tasks}")),
            project: "p".into(),
            agent: "claude".into(),
            status: "running".into(),
            query: None,
            response: None,
            tool: None,
            last_activity: None,
            remote_url: url.map(Into::into),
            background_tasks,
        };
        let machines = vec![MachineAgents {
            machine: "mac".into(),
            label: "Mac".into(),
            platform: "darwin".into(),
            updated_at: Utc::now(),
            agents: vec![
                agent(Some("https://claude.ai/code/session_01Test"), 2),
                agent(Some("https://evil.example/code/session_01Test"), 0),
                agent(None, 0),
            ],
            machine_id: None,
        }];
        let rows = &remote_machines(&machines)[0].rows;
        assert_eq!(
            rows[0].remote_url.as_deref(),
            Some("https://claude.ai/code/session_01Test")
        );
        assert_eq!(rows[0].background_tasks, 2);
        // The session id rides along, so the sessions list can tell which
        // transcript that machine is running.
        assert_eq!(rows[0].session_id.as_deref(), Some("sid-2"));
        assert_eq!(rows[1].remote_url, None);
        assert_eq!(rows[2].remote_url, None);
    }

    #[test]
    fn machine_key_with_profile_gets_a_suffix() {
        assert_eq!(
            machine_key_for("Alice-MacBook.local", Some("dev")),
            "alice-macbook-dev"
        );
        assert_eq!(machine_key_for("pc", Some("My Test")), "pc-my-test");
    }

    fn machine(name: &str, ids: &[&str]) -> MachineAgents {
        MachineAgents {
            machine: name.into(),
            label: name.into(),
            platform: "win32".into(),
            updated_at: Utc::now(),
            agents: ids
                .iter()
                .map(|id| LiveAgent {
                    id: Some((*id).into()),
                    project: "p".into(),
                    agent: "claude".into(),
                    status: "running".into(),
                    query: None,
                    response: None,
                    tool: None,
                    last_activity: None,
                    remote_url: None,
                    background_tasks: 0,
                })
                .collect(),
            machine_id: None,
        }
    }

    fn read_ids(answer: LiveAgentsAnswer) -> Option<Vec<String>> {
        match answer {
            LiveAgentsAnswer::Read(m) => Some(
                m.iter()
                    .flat_map(|m| m.agents.iter().filter_map(|a| a.id.clone()))
                    .collect(),
            ),
            _ => None,
        }
    }

    #[test]
    fn answer_starts_pending_and_offline_from_the_start_is_not_read() {
        let mut sync = LiveAgentsSync::default();
        assert!(matches!(sync.answer(), LiveAgentsAnswer::Pending));
        assert!(sync.apply(TickOutcome::ReadFailed).is_some());
        assert!(matches!(sync.answer(), LiveAgentsAnswer::NotRead));
        // A second failure sends nothing new.
        assert!(sync.apply(TickOutcome::ReadFailed).is_none());
        assert!(matches!(sync.answer(), LiveAgentsAnswer::NotRead));
    }

    #[test]
    fn a_read_list_outlives_a_later_failed_read() {
        let mut sync = LiveAgentsSync::default();
        sync.apply(TickOutcome::Read(vec![machine("pc", &["s1"])]));
        assert!(sync.apply(TickOutcome::ReadFailed).is_none());
        assert_eq!(read_ids(sync.answer()), Some(vec!["s1".to_string()]));
    }

    #[test]
    fn signing_out_turns_any_read_list_into_not_read() {
        // A non-empty list: an empty one goes out.
        let mut sync = LiveAgentsSync::default();
        sync.apply(TickOutcome::Read(vec![machine("pc", &["s1"])]));
        let sent = sync.apply(TickOutcome::SignedOut).expect("emptied");
        assert!(sent.is_empty());
        assert!(matches!(sync.answer(), LiveAgentsAnswer::NotRead));
        // An empty list read before: nothing to send, but it no longer
        // counts as read.
        let mut sync = LiveAgentsSync::default();
        sync.apply(TickOutcome::Read(Vec::new()));
        assert_eq!(read_ids(sync.answer()), Some(vec![]));
        assert!(sync.apply(TickOutcome::SignedOut).is_none());
        assert!(matches!(sync.answer(), LiveAgentsAnswer::NotRead));
        // Signed out from the start is an answer at once.
        let mut sync = LiveAgentsSync::default();
        assert!(sync.apply(TickOutcome::SignedOut).is_some());
        assert!(matches!(sync.answer(), LiveAgentsAnswer::NotRead));
    }

    #[test]
    fn tick_publishes_only_when_on_and_reads_only_when_visible() {
        // Off (new install): never publish, delete the key once, read only
        // while a panel shows the list.
        assert_eq!(
            plan_tick(false, true, false, false),
            TickPlan {
                publish: false,
                delete: true,
                read: false
            }
        );
        assert_eq!(
            plan_tick(false, true, true, false),
            TickPlan {
                publish: false,
                delete: false,
                read: false
            }
        );
        // On and hidden: keep publishing (Iris reads it) without polling.
        assert_eq!(
            plan_tick(true, true, false, false),
            TickPlan {
                publish: true,
                delete: false,
                read: false
            }
        );
        assert_eq!(
            plan_tick(true, false, false, true),
            TickPlan {
                publish: false,
                delete: false,
                read: true
            }
        );
    }
}

// SPDX-License-Identifier: AGPL-3.0-only
//! Mnemos IDE: team sessions. A Claude Code session in a Mnemos pane (the
//! coordinator) starts worker sessions in panes next to it, lists, focuses,
//! renames and retires them, and cleans up the git worktrees they used.
//!
//! Transport: an MCP server over streamable HTTP on loopback, at a random port
//! that terminals get as `MNEMOS_TEAM_MCP_URL`. The `mnemos-team` Claude Code
//! plugin points Claude at it with the terminal's `MNEMOS_CLI_AGENT_TOKEN` as
//! a bearer token. That token is only in the environment of processes started
//! in that pane, so it both authenticates the caller and says which pane and
//! tab it runs in. Requests with an `Origin` header (a browser) or a foreign
//! `Host` (DNS rebinding) are refused before the token is looked at. Nothing
//! here is reachable through the `mnemos://` URL scheme.
//!
//! Guards: the server only runs with `agents.team.enabled`. A pane may spawn
//! only after the user allows it in a dialog, once per pane; spawned panes can
//! never spawn. At most 6 spawned sessions live and 3 spawns a minute
//! (`mnemos_ide_team::limits`). Each spawn shows a toast and its brief is the
//! session's first prompt, on screen in its pane. Spawned sessions start
//! Claude Code with `--dangerously-skip-permissions` only when the local,
//! never-synced `agents.team.spawn_skip_permissions` is on. Every call is
//! logged under the `mnemos_ide_team` target.
//!
//! The UI-free parts (validation, command line, caps, worktrees, MCP
//! messages) live in `ide/mnemos-ide-team`.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use futures::channel::oneshot;
use mnemos_ide_team::command::{LaunchSpec, ShellFamily};
use mnemos_ide_team::leases::{LeaseTable, Resource};
use mnemos_ide_team::limits::SpawnLimiter;
use mnemos_ide_team::mcp::{self, Incoming, ToolCall};
use mnemos_ide_team::names::{validate_brief, validate_name};
use mnemos_ide_team::worktree;
use mnemos_ui::modals::{AlertDialogWithCallbacks, ModalButton};
use mnemos_ui::{AppContext, Entity, EntityId, ModelContext, SingletonEntity, WindowId};
use serde_json::{json, Value};
use settings::{macros::define_settings_group, Setting as _, SupportedPlatforms, SyncToCloud};

use super::view::Workspace;
use super::WorkspaceRegistry;
use crate::terminal::cli_agent_sessions::file_ingest::CLIAgentEventFileIngestor;
use crate::terminal::cli_agent_sessions::{CLIAgentSessionStatus, CLIAgentSessionsModel};
use crate::view_components::DismissibleToast;
use crate::workspace::ToastStack;

define_settings_group!(TeamSettings, settings: [
    enabled: TeamSessionsEnabled {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        private: false,
        toml_path: "agents.team.enabled",
        description: "Run the local team server that lets a Claude Code session you allow start other Claude Code sessions in panes next to it (the mnemos-team plugin). Takes effect on the next launch.",
    },
    keep_awake: TeamKeepAwake {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        private: false,
        toml_path: "agents.team.keep_awake",
        description: "Keep the computer from idle-sleeping while any agent in Mnemos is working. Does not keep a closed laptop awake.",
    },
    spawn_skip_permissions: TeamSpawnSkipPermissions {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        private: false,
        toml_path: "agents.team.spawn_skip_permissions",
        description: "Start team sessions with --dangerously-skip-permissions, so they never ask before running tools. Applies to every spawned session on this machine; never synced.",
    },
]);

/// Environment variable terminals get with the server's MCP endpoint.
pub(crate) const MCP_URL_ENV: &str = "MNEMOS_TEAM_MCP_URL";

static MCP_URL: OnceLock<String> = OnceLock::new();

/// The MCP endpoint, once the server is listening.
pub(crate) fn mcp_url() -> Option<&'static str> {
    MCP_URL.get().map(String::as_str)
}

/// What the HTTP side gets back.
enum Reply {
    Json(Value),
    Unauthorized,
}

/// One request from the HTTP side, handled on the main thread.
struct TeamRequest {
    token: String,
    body: Incoming,
    reply: oneshot::Sender<Reply>,
}

/// A session this server spawned.
#[derive(Debug, Clone)]
struct Spawned {
    name: String,
    view: EntityId,
    coordinator: EntityId,
    cwd: PathBuf,
    worktree: Option<PathBuf>,
    model: Option<String>,
    /// MNEMOS_TEAM_DIR given to the session, when its directory is not the team's repository.
    team_dir: Option<PathBuf>,
    /// Persistent pane uuids, so the session is known again after an app restart.
    pane_uuid: Option<String>,
    coordinator_uuid: Option<String>,
}

/// A spawned session as `store/team_sessions.json` keeps it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SavedSpawn {
    pane_uuid: String,
    #[serde(default)]
    coordinator_uuid: Option<String>,
    name: String,
    cwd: PathBuf,
    #[serde(default)]
    worktree: Option<PathBuf>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    team_dir: Option<PathBuf>,
}

/// How long after launch saved sessions may still turn up (agent restore
/// brings panes back in the first seconds); unresolved ones are dropped after.
const ADOPT_WINDOW: std::time::Duration = std::time::Duration::from_secs(120);

/// A spawn waiting for the user to allow its coordinator.
struct PendingSpawn {
    id: Value,
    call: ToolCall,
    reply: oneshot::Sender<Reply>,
}

pub(crate) struct TeamControl {
    _runtime: Option<tokio::runtime::Runtime>,
    spawned: Vec<Spawned>,
    allowed: HashSet<EntityId>,
    denied: HashSet<EntityId>,
    asking: HashMap<EntityId, Vec<PendingSpawn>>,
    limiter: SpawnLimiter,
    leases: LeaseTable,
    /// Held while any agent works and `agents.team.keep_awake` is on.
    awake: Option<prevent_sleep::Guard>,
    /// Sessions saved by an earlier run whose panes have not been matched yet.
    unbound: Vec<SavedSpawn>,
    started: instant::Instant,
    /// The team dir each coordinator last passed, reused when it passes none.
    team_dirs: HashMap<EntityId, PathBuf>,
}

impl Entity for TeamControl {
    type Event = ();
}

impl SingletonEntity for TeamControl {}

impl TeamControl {
    pub(crate) fn new(ctx: &mut ModelContext<Self>) -> Self {
        let mut me = Self {
            _runtime: None,
            spawned: Vec::new(),
            allowed: HashSet::new(),
            denied: HashSet::new(),
            asking: HashMap::new(),
            limiter: SpawnLimiter::default(),
            leases: LeaseTable::default(),
            awake: None,
            unbound: load_saved(),
            started: instant::Instant::now(),
            team_dirs: HashMap::new(),
        };
        ctx.subscribe_to_model(&CLIAgentSessionsModel::handle(ctx), |me, _, ctx| me.update_awake(ctx));
        ctx.subscribe_to_model(&TeamSettings::handle(ctx), |me, _, ctx| me.update_awake(ctx));
        if !*TeamSettings::as_ref(ctx).enabled.value() {
            return me;
        }
        let (tx, rx) = async_channel::unbounded::<TeamRequest>();
        match start_server(tx) {
            Ok((runtime, addr)) => {
                let url = format!("http://{addr}/mcp");
                log::info!(target: "mnemos_ide_team", "team MCP server listening at {url}");
                let _ = MCP_URL.set(url);
                me._runtime = Some(runtime);
                ctx.spawn_stream_local(rx, |me, request, ctx| me.handle_request(request, ctx), |_, _| {});
            }
            Err(err) => log::warn!(target: "mnemos_ide_team", "team MCP server did not start: {err:#}"),
        }
        me
    }

    fn handle_request(&mut self, request: TeamRequest, ctx: &mut ModelContext<Self>) {
        let TeamRequest { token, body, reply } = request;
        let caller = CLIAgentEventFileIngestor::as_ref(ctx).terminal_view_for_token(&token);
        let Some(caller) = caller else {
            log::warn!(target: "mnemos_ide_team", "refused a request with an unknown token");
            let _ = reply.send(Reply::Unauthorized);
            return;
        };
        let (id, method, params) = match body {
            Incoming::Request { id, method, params } => (id, method, params),
            // Notifications are answered on the HTTP side; nothing reaches here.
            Incoming::Notification { .. } => return,
            Incoming::Invalid(error) => {
                let _ = reply.send(Reply::Json(error));
                return;
            }
        };
        let respond = |result: Value| Reply::Json(mcp::result(id.clone(), result));
        match method.as_str() {
            "initialize" => {
                let _ = reply.send(respond(mcp::initialize_result(&params, env!("CARGO_PKG_VERSION"))));
            }
            "ping" => {
                let _ = reply.send(respond(json!({})));
            }
            "tools/list" => {
                let _ = reply.send(respond(mcp::tools_list()));
            }
            "tools/call" => match mcp::parse_tool_call(&params) {
                Ok(call) => {
                    log::info!(target: "mnemos_ide_team", "tool call from terminal {caller:?}: {}", describe(&call));
                    self.call(caller, id, call, reply, ctx);
                }
                Err(err) => {
                    let _ = reply.send(respond(mcp::tool_text(err, true)));
                }
            },
            other => {
                let _ = reply.send(Reply::Json(mcp::error(id, -32601, &format!("unknown method {other}"))));
            }
        }
    }

    fn call(
        &mut self,
        caller: EntityId,
        id: Value,
        call: ToolCall,
        reply: oneshot::Sender<Reply>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.adopt(ctx);
        self.forget_closed(ctx);
        if let ToolCall::Spawn { .. } = call {
            self.spawn_or_ask(caller, id, call, reply, ctx);
            return;
        }
        let wrap = move |text: String, is_error: bool| Reply::Json(mcp::result(id, mcp::tool_text(text, is_error)));
        match call {
            ToolCall::Spawn { .. } => unreachable!("handled above"),
            ToolCall::List => {
                let _ = reply.send(wrap(self.list(caller, ctx), false));
            }
            ToolCall::Focus { name } => {
                let result = self.find_in_tab(caller, &name, ctx).map(|view| {
                    with_workspace_of(view, ctx, |ws, wctx| ws.ide_team_focus(view, wctx));
                    format!("focused {name}")
                });
                let _ = reply.send(text_result(wrap, result));
            }
            ToolCall::Rename { name, new_name } => {
                let result = self.rename(caller, &name, &new_name, ctx);
                let _ = reply.send(text_result(wrap, result));
            }
            ToolCall::Retire { name, keep_worktree } => self.retire(caller, &name, keep_worktree, reply, wrap, ctx),
            ToolCall::ListWorktrees => {
                let Some(repo) = self.repo_of(caller, ctx) else {
                    let _ = reply.send(wrap("this pane is not in a git repository".into(), true));
                    return;
                };
                ctx.spawn(async move { worktree::list(&repo) }, move |_, result, _| {
                    let _ = reply.send(match result {
                        Ok(list) => wrap(serde_json::to_string_pretty(&list).unwrap_or_default(), false),
                        Err(err) => wrap(err.to_string(), true),
                    });
                });
            }
            ToolCall::PruneWorktrees { dry_run } => {
                let Some(repo) = self.repo_of(caller, ctx) else {
                    let _ = reply.send(wrap("this pane is not in a git repository".into(), true));
                    return;
                };
                let live: Vec<PathBuf> = self.spawned.iter().filter_map(|s| s.worktree.clone()).collect();
                ctx.spawn(async move { prune_except(&repo, &live, dry_run) }, move |_, result, _| {
                    let _ = reply.send(match result {
                        Ok(text) => wrap(text, false),
                        Err(err) => wrap(err, true),
                    });
                });
            }
            ToolCall::Lease { resource, ttl_seconds } => {
                let result = self.lease(caller, &resource, ttl_seconds, ctx);
                let _ = reply.send(text_result(wrap, result));
            }
            ToolCall::Release { lease_id } => {
                let result = self
                    .leases
                    .release(&lease_id, caller.as_usize() as u64)
                    .map(|()| format!("released {lease_id}"));
                let _ = reply.send(text_result(wrap, result));
            }
            ToolCall::ListLeases => {
                let now = instant::Instant::now();
                let rows: Vec<Value> = self
                    .leases
                    .list(now)
                    .into_iter()
                    .map(|(lease, left)| json!({ "lease": lease, "seconds_left": left }))
                    .collect();
                let _ = reply.send(wrap(serde_json::to_string_pretty(&rows).unwrap_or_default(), false));
            }
        }
    }

    /// Spawns at once for an allowed coordinator, refuses for a spawned pane
    /// or a denied one, and otherwise asks the user first.
    fn spawn_or_ask(
        &mut self,
        caller: EntityId,
        id: Value,
        call: ToolCall,
        reply: oneshot::Sender<Reply>,
        ctx: &mut ModelContext<Self>,
    ) {
        let wrap = |text: String, is_error: bool| Reply::Json(mcp::result(id.clone(), mcp::tool_text(text, is_error)));
        if self.spawned.iter().any(|s| s.view == caller) {
            let _ = reply.send(wrap("a spawned session cannot spawn sessions; ask your coordinator".into(), true));
            return;
        }
        if self.denied.contains(&caller) {
            let _ = reply.send(wrap("the user did not allow this session to spawn sessions".into(), true));
            return;
        }
        if self.allowed.contains(&caller) {
            let result = self.spawn(caller, call, ctx);
            let _ = reply.send(text_result(wrap, result));
            return;
        }
        let first = !self.asking.contains_key(&caller);
        self.asking.entry(caller).or_default().push(PendingSpawn { id, call, reply });
        if first {
            ask_user(caller, ctx);
        }
    }

    /// The user's answer to [`ask_user`].
    fn answer(&mut self, caller: EntityId, allow: bool, ctx: &mut ModelContext<Self>) {
        log::info!(target: "mnemos_ide_team", "user {} spawning for terminal {caller:?}", if allow { "allowed" } else { "denied" });
        if allow {
            self.denied.remove(&caller);
            self.allowed.insert(caller);
        } else {
            self.allowed.remove(&caller);
            self.denied.insert(caller);
        }
        for pending in self.asking.remove(&caller).unwrap_or_default() {
            let result = if allow {
                self.spawn(caller, pending.call, ctx)
            } else {
                Err("the user did not allow this session to spawn sessions".to_string())
            };
            let _ = pending.reply.send(Reply::Json(mcp::result(
                pending.id,
                match result {
                    Ok(text) => mcp::tool_text(text, false),
                    Err(err) => mcp::tool_text(err, true),
                },
            )));
        }
    }

    fn spawn(&mut self, caller: EntityId, call: ToolCall, ctx: &mut ModelContext<Self>) -> Result<String, String> {
        let ToolCall::Spawn { name, brief, model, cwd, worktree_branch, worktree_base, team_dir } = call else {
            return Err("not a spawn".into());
        };
        validate_name(&name).map_err(|e| e.to_string())?;
        validate_brief(&brief).map_err(|e| e.to_string())?;
        if self.spawned.iter().any(|s| s.name == name) {
            return Err(format!("a session named {name} already exists; retire it or pick another name"));
        }
        let skip_permissions = *TeamSettings::as_ref(ctx).spawn_skip_permissions.value();
        let spec = LaunchSpec::new(&name, model.as_deref(), skip_permissions).map_err(|e| e.to_string())?;
        self.limiter
            .try_spawn(self.spawned.len(), instant::Instant::now())
            .map_err(|e| e.to_string())?;

        let caller_cwd = caller_cwd(caller, ctx);
        let (directory, worktree_path) = match worktree_branch {
            Some(branch) => {
                // An explicit cwd names the repository; else the caller's own.
                let repo = cwd
                    .map(PathBuf::from)
                    .or_else(|| caller_cwd.clone())
                    .ok_or("cannot tell your directory; pass cwd or run from a repository")?;
                let path = worktree::create(&repo, &name, &branch, worktree_base.as_deref()).map_err(|e| e.to_string())?;
                (path.clone(), Some(path))
            }
            None => {
                let dir = match cwd {
                    Some(dir) => PathBuf::from(dir),
                    None => caller_cwd.ok_or("cannot tell your directory; pass cwd")?,
                };
                if !dir.is_absolute() || !dir.is_dir() {
                    return Err(format!("{} is not an existing absolute directory", dir.display()));
                }
                (dir, None)
            }
        };

        // A session outside the team's repository finds rules.md and the board
        // only through MNEMOS_TEAM_DIR: the one passed, else the last one this
        // coordinator passed.
        let in_repo = worktree::team_dir_of(&directory).is_some();
        let team_dir = match team_dir {
            Some(dir) => {
                let dir = PathBuf::from(dir);
                if !dir.is_absolute() || !dir.is_dir() {
                    return Err(format!("team_dir {} is not an existing absolute directory", dir.display()));
                }
                self.team_dirs.insert(caller, dir.clone());
                Some(dir)
            }
            None if !in_repo => self.team_dirs.get(&caller).cloned(),
            None => None,
        };
        let team_note = match (&team_dir, in_repo) {
            (Some(dir), _) => format!(" It reads team rules and the board from {}.", dir.display()),
            (None, true) => String::new(),
            (None, false) => format!(
                " Note: {} is not in a git repository, so this session gets no team rules or board; pass team_dir (e.g. <repo>/.git/mnemos-team).",
                directory.display()
            ),
        };

        let powershell = with_workspace_of(caller, ctx, |ws, wctx| ws.ide_team_is_powershell(caller, wctx)).unwrap_or(false);
        let family = if powershell { ShellFamily::PowerShell } else { ShellFamily::Posix };
        let command = spec.command_line(family);
        let mut env: HashMap<OsString, OsString> = spec
            .env(&brief)
            .into_iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect();
        if let Some(dir) = &team_dir {
            env.insert(OsString::from("MNEMOS_TEAM_DIR"), dir.clone().into_os_string());
        }

        let view = with_workspace_of(caller, ctx, |ws, wctx| {
            ws.ide_team_split(caller, directory.clone(), env, &command, &name, wctx)
        })
        .flatten()
        .ok_or("could not open a pane next to yours")?;

        let pane_uuid = with_workspace_of_ref(view, ctx, |ws| ws.ide_team_pane_uuid(view, ctx)).flatten();
        let coordinator_uuid = with_workspace_of_ref(caller, ctx, |ws| ws.ide_team_pane_uuid(caller, ctx)).flatten();
        self.spawned.push(Spawned {
            name: name.clone(),
            view,
            coordinator: caller,
            cwd: directory.clone(),
            worktree: worktree_path.clone(),
            model: model.clone(),
            team_dir: team_dir.clone(),
            pane_uuid,
            coordinator_uuid,
        });
        self.save();
        log::info!(target: "mnemos_ide_team", "spawned {name} in {} (terminal {view:?}, bypass {skip_permissions})", directory.display());
        if let Some(window) = window_of(view, ctx) {
            let text = format!("Started team session \"{name}\"{}", if skip_permissions { " (skipping permission prompts)" } else { "" });
            ToastStack::handle(ctx).update(ctx, |stack, sctx| {
                stack.add_ephemeral_toast(DismissibleToast::default(text), window, sctx);
            });
        }
        Ok(format!(
            "spawned {name} in {}{}. Its first prompt is your brief. Message it with SendMessage once it is listed by ListAgents.{team_note}",
            directory.display(),
            worktree_path.map(|p| format!(" (worktree {})", p.display())).unwrap_or_default(),
        ))
    }

    fn list(&self, caller: EntityId, ctx: &ModelContext<Self>) -> String {
        let terminals = with_workspace_of_ref(caller, ctx, |ws| ws.ide_team_tab_terminals(caller, ctx)).unwrap_or_default();
        let sessions = CLIAgentSessionsModel::as_ref(ctx);
        let rows: Vec<Value> = terminals
            .into_iter()
            .filter_map(|view| {
                let spawned = self.spawned.iter().find(|s| s.view == view);
                let agent = sessions.session(view);
                if spawned.is_none() && agent.is_none() && view != caller {
                    return None;
                }
                Some(json!({
                    "name": spawned.map(|s| s.name.clone()),
                    "you": view == caller,
                    "spawned_by_you": spawned.is_some_and(|s| s.coordinator == caller),
                    "status": agent.map(|a| match &a.status {
                        // Started (or resumed after a restart) and no turn yet:
                        // it sits at its prompt. The model calls a new session
                        // in progress until its first `Stop`.
                        CLIAgentSessionStatus::InProgress
                            if a.session_context.query.is_none() && a.session_context.response.is_none() =>
                        {
                            "idle".to_string()
                        }
                        CLIAgentSessionStatus::InProgress => "working".to_string(),
                        CLIAgentSessionStatus::Success => "idle".to_string(),
                        CLIAgentSessionStatus::Blocked { message } => format!("blocked: {}", message.as_deref().unwrap_or("")),
                    }),
                    "cwd": spawned.map(|s| s.cwd.display().to_string())
                        .or_else(|| agent.and_then(|a| a.session_context.cwd.clone())),
                    "worktree": spawned.and_then(|s| s.worktree.as_ref().map(|p| p.display().to_string())),
                    "model": spawned.and_then(|s| s.model.clone()),
                    "session_id": agent.and_then(|a| a.session_context.session_id.clone()),
                    "last_prompt": agent.and_then(|a| a.session_context.query.clone()),
                }))
            })
            .collect();
        serde_json::to_string_pretty(&rows).unwrap_or_default()
    }

    /// A spawned session called `name` in the caller's tab.
    fn find_in_tab(&self, caller: EntityId, name: &str, ctx: &ModelContext<Self>) -> Result<EntityId, String> {
        let terminals = with_workspace_of_ref(caller, ctx, |ws| ws.ide_team_tab_terminals(caller, ctx)).unwrap_or_default();
        self.spawned
            .iter()
            .find(|s| s.name == name && terminals.contains(&s.view))
            .map(|s| s.view)
            .ok_or_else(|| format!("no spawned session named {name} in your tab"))
    }

    fn rename(&mut self, caller: EntityId, name: &str, new_name: &str, ctx: &mut ModelContext<Self>) -> Result<String, String> {
        validate_name(new_name).map_err(|e| e.to_string())?;
        if self.spawned.iter().any(|s| s.name == new_name) {
            return Err(format!("{new_name} is taken"));
        }
        let view = self.find_in_tab(caller, name, ctx)?;
        with_workspace_of(view, ctx, |ws, wctx| ws.ide_team_set_title(view, new_name, wctx));
        if let Some(s) = self.spawned.iter_mut().find(|s| s.view == view) {
            s.name = new_name.to_string();
        }
        self.save();
        Ok(format!("renamed {name} to {new_name} (its Claude session keeps its own /rename name)"))
    }

    fn retire(
        &mut self,
        caller: EntityId,
        name: &str,
        keep_worktree: bool,
        reply: oneshot::Sender<Reply>,
        wrap: impl FnOnce(String, bool) -> Reply + 'static,
        ctx: &mut ModelContext<Self>,
    ) {
        let view = match self.find_in_tab(caller, name, ctx) {
            Ok(view) => view,
            Err(err) => {
                let _ = reply.send(wrap(err, true));
                return;
            }
        };
        let working = matches!(
            CLIAgentSessionsModel::as_ref(ctx).session(view).map(|s| &s.status),
            Some(CLIAgentSessionStatus::InProgress)
        );
        if working {
            let _ = reply.send(wrap(
                format!("{name} is still working; tell it to stop (SendMessage) or ask the user to close its pane"),
                true,
            ));
            return;
        }
        let spawned = self.spawned.iter().find(|s| s.view == view).cloned();
        let worktree_path = spawned.as_ref().and_then(|s| s.worktree.clone()).filter(|_| !keep_worktree);
        let Some(path) = worktree_path else {
            self.close(view, ctx);
            let _ = reply.send(wrap(format!("retired {name}"), false));
            return;
        };
        // Check the worktree before closing anything: a refused removal keeps
        // the session, so its work is never stranded without a pane.
        let repo = path.clone();
        let name = name.to_string();
        ctx.spawn(
            async move {
                worktree::list(&repo)
                    .map_err(|e| e.to_string())
                    .and_then(|list| {
                        let info = list
                            .into_iter()
                            .find(|w| w.path == repo)
                            .ok_or_else(|| format!("{} is no longer a worktree", repo.display()))?;
                        let base = worktree::default_base(&repo);
                        match worktree::keep_reason(&info, base.as_deref()) {
                            Some(reason) => Err(format!("kept {}: {reason}", repo.display())),
                            None => Ok(()),
                        }
                    })
            },
            move |me, checked, ctx| {
                if let Err(err) = checked {
                    let _ = reply.send(wrap(
                        format!("did not retire {name}: {err}. Commit and merge first, or retire with keep_worktree."),
                        true,
                    ));
                    return;
                }
                // The pane goes first so the worktree is no longer its cwd.
                me.close(view, ctx);
                ctx.spawn(
                    async move { worktree::remove(&path, &path).map(|info| info.size_bytes).map_err(|e| e.to_string()) },
                    move |_, removed, _| {
                        let _ = reply.send(match removed {
                            Ok(bytes) => wrap(format!("retired {name} and removed its worktree ({} MB)", bytes / 1_000_000), false),
                            Err(err) => wrap(format!("retired {name}, but its worktree stays: {err}"), true),
                        });
                    },
                );
            },
        );
    }

    fn lease(&mut self, caller: EntityId, resource: &str, ttl_seconds: u64, ctx: &ModelContext<Self>) -> Result<String, String> {
        let resource = Resource::parse(resource)?;
        let now = instant::Instant::now();
        let holder_name = self
            .spawned
            .iter()
            .find(|s| s.view == caller)
            .map(|s| s.name.clone())
            .or_else(|| caller_cwd(caller, ctx).map(|p| format!("the session in {}", p.display())))
            .unwrap_or_else(|| "another session".to_string());
        let lease = self.leases.acquire(
            resource,
            caller.as_usize() as u64,
            &holder_name,
            std::time::Duration::from_secs(ttl_seconds),
            now,
            free_port,
        )?;
        let left = lease.expires.saturating_duration_since(now).as_secs();
        Ok(match lease.port {
            Some(port) => format!("lease {} on port {port} for {left} s; release it when done", lease.id),
            None => format!("lease {} for {left} s; release it when done", lease.id),
        })
    }

    /// Holds or drops the keep-awake assertion to match whether any agent works.
    fn update_awake(&mut self, ctx: &mut ModelContext<Self>) {
        let want = *TeamSettings::as_ref(ctx).keep_awake.value()
            && CLIAgentSessionsModel::as_ref(ctx)
                .iter()
                .any(|(_, s)| matches!(s.status, CLIAgentSessionStatus::InProgress));
        match (want, self.awake.is_some()) {
            (true, false) => {
                log::info!(target: "mnemos_ide_team", "an agent is working: keeping the computer awake");
                self.awake = Some(prevent_sleep::prevent_sleep("A Claude Code agent is working in Mnemos"));
            }
            (false, true) => {
                log::info!(target: "mnemos_ide_team", "no agent is working: allowing sleep");
                self.awake = None;
            }
            _ => {}
        }
    }

    fn close(&mut self, view: EntityId, ctx: &mut ModelContext<Self>) {
        with_workspace_of(view, ctx, |ws, wctx| ws.ide_team_close(view, wctx));
        self.spawned.retain(|s| s.view != view);
        self.save();
    }

    /// Matches sessions saved by an earlier run to the panes agent restore
    /// brought back, by pane uuid. Unmatched ones are kept for the first
    /// [`ADOPT_WINDOW`] after launch and dropped after that.
    fn adopt(&mut self, ctx: &mut ModelContext<Self>) {
        if self.unbound.is_empty() {
            return;
        }
        let find = |uuid: &str, ctx: &AppContext| {
            WorkspaceRegistry::as_ref(ctx)
                .all_workspaces(ctx)
                .into_iter()
                .find_map(|(_, ws)| ws.as_ref(ctx).ide_team_view_for_uuid(uuid, ctx))
        };
        let mut still = Vec::new();
        for saved in std::mem::take(&mut self.unbound) {
            let Some(view) = find(&saved.pane_uuid, ctx) else {
                still.push(saved);
                continue;
            };
            let coordinator = saved
                .coordinator_uuid
                .as_deref()
                .and_then(|uuid| find(uuid, ctx))
                .unwrap_or(view);
            log::info!(target: "mnemos_ide_team", "found {} again after a restart (terminal {view:?})", saved.name);
            with_workspace_of(view, ctx, |ws, wctx| ws.ide_team_set_title(view, &saved.name, wctx));
            if let Some(dir) = &saved.team_dir {
                self.team_dirs.entry(coordinator).or_insert_with(|| dir.clone());
            }
            self.spawned.push(Spawned {
                name: saved.name,
                view,
                coordinator,
                cwd: saved.cwd,
                worktree: saved.worktree,
                model: saved.model,
                team_dir: saved.team_dir,
                pane_uuid: Some(saved.pane_uuid),
                coordinator_uuid: saved.coordinator_uuid,
            });
        }
        if self.started.elapsed() < ADOPT_WINDOW {
            self.unbound = still;
        } else if !still.is_empty() {
            log::info!(target: "mnemos_ide_team", "dropped {} saved session(s) whose panes did not come back", still.len());
        }
        self.save();
    }

    /// Writes the spawned sessions (and any not matched yet) to the store.
    fn save(&self) {
        let mut saved: Vec<SavedSpawn> = self
            .spawned
            .iter()
            .filter_map(|s| {
                Some(SavedSpawn {
                    pane_uuid: s.pane_uuid.clone()?,
                    coordinator_uuid: s.coordinator_uuid.clone(),
                    name: s.name.clone(),
                    cwd: s.cwd.clone(),
                    worktree: s.worktree.clone(),
                    model: s.model.clone(),
                    team_dir: s.team_dir.clone(),
                })
            })
            .collect();
        saved.extend(self.unbound.iter().cloned());
        let Ok(path) = mnemos_ide_core::paths::ide_team_sessions_file() else {
            return;
        };
        let tmp = path.with_extension("json.tmp");
        let written = serde_json::to_vec_pretty(&saved)
            .map_err(std::io::Error::other)
            .and_then(|bytes| std::fs::write(&tmp, bytes))
            .and_then(|()| std::fs::rename(&tmp, &path));
        if let Err(err) = written {
            log::warn!(target: "mnemos_ide_team", "could not save team sessions: {err}");
        }
    }

    /// Drops spawned sessions whose pane is gone, approvals of closed panes,
    /// and leases that expired or whose holder's pane closed.
    fn forget_closed(&mut self, ctx: &ModelContext<Self>) {
        let alive = |view: EntityId| window_of(view, ctx).is_some();
        self.leases.purge(instant::Instant::now(), |holder| {
            alive(EntityId::from_usize(holder as usize))
        });
        let before = self.spawned.len();
        self.spawned.retain(|s| alive(s.view));
        if self.spawned.len() != before {
            self.save();
        }
        self.allowed.retain(|v| alive(*v));
        self.denied.retain(|v| alive(*v));
    }

    fn repo_of(&self, caller: EntityId, ctx: &ModelContext<Self>) -> Option<PathBuf> {
        caller_cwd(caller, ctx).and_then(|cwd| worktree::toplevel(&cwd).ok())
    }
}

/// Sessions an earlier run saved; empty when there is no file or it is unreadable.
fn load_saved() -> Vec<SavedSpawn> {
    mnemos_ide_core::paths::ide_team_sessions_file()
        .ok()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn text_result(wrap: impl FnOnce(String, bool) -> Reply, result: Result<String, String>) -> Reply {
    match result {
        Ok(text) => wrap(text, false),
        Err(err) => wrap(err, true),
    }
}

fn describe(call: &ToolCall) -> String {
    match call {
        ToolCall::Spawn { name, model, worktree_branch, brief, .. } => format!(
            "spawn {name} model={model:?} worktree={worktree_branch:?} brief={} chars",
            brief.chars().count()
        ),
        other => format!("{other:?}"),
    }
}

/// The caller's working directory as its agent reported it, else the shell's.
fn caller_cwd(caller: EntityId, ctx: &AppContext) -> Option<PathBuf> {
    if let Some(cwd) = CLIAgentSessionsModel::as_ref(ctx)
        .session(caller)
        .and_then(|s| s.session_context.cwd.clone())
    {
        return Some(PathBuf::from(cwd));
    }
    with_workspace_of_ref(caller, ctx, |ws| ws.ide_team_shell_cwd(caller, ctx)).flatten()
}

/// Prunes clean merged worktrees, leaving the ones live sessions still use.
fn prune_except(repo: &Path, live: &[PathBuf], dry_run: bool) -> Result<String, String> {
    if live.is_empty() {
        let report = worktree::prune(repo, dry_run).map_err(|e| e.to_string())?;
        return Ok(serde_json::to_string_pretty(&report).unwrap_or_default());
    }
    // With live sessions, go one by one so their worktrees are kept.
    let base = worktree::default_base(repo);
    let mut report = worktree::PruneReport::default();
    for info in worktree::list(repo).map_err(|e| e.to_string())? {
        if info.main {
            continue;
        }
        if live.iter().any(|p| p == &info.path) {
            report.kept.push((info.path.clone(), "a live team session uses it".into()));
            continue;
        }
        if let Some(reason) = worktree::keep_reason(&info, base.as_deref()) {
            report.kept.push((info.path.clone(), reason));
            continue;
        }
        if dry_run {
            report.freed_bytes += info.size_bytes;
            report.removed.push(info);
            continue;
        }
        match worktree::remove(repo, &info.path) {
            Ok(removed) => {
                report.freed_bytes += removed.size_bytes;
                report.removed.push(removed);
            }
            Err(err) => report.kept.push((info.path.clone(), err.to_string())),
        }
    }
    Ok(serde_json::to_string_pretty(&report).unwrap_or_default())
}

/// A loopback port nothing listens on now and no lease holds.
fn free_port(taken: &[u16]) -> Option<u16> {
    (0..20).find_map(|_| {
        let port = std::net::TcpListener::bind(("127.0.0.1", 0)).ok()?.local_addr().ok()?.port();
        (!taken.contains(&port)).then_some(port)
    })
}

/// The window whose workspace hosts terminal `view`.
fn window_of(view: EntityId, ctx: &AppContext) -> Option<WindowId> {
    WorkspaceRegistry::as_ref(ctx)
        .all_workspaces(ctx)
        .into_iter()
        .find(|(_, ws)| ws.as_ref(ctx).ide_team_tab_of(view, ctx).is_some())
        .map(|(window, _)| window)
}

fn with_workspace_of<R>(
    view: EntityId,
    ctx: &mut AppContext,
    f: impl FnOnce(&mut Workspace, &mut mnemos_ui::ViewContext<Workspace>) -> R,
) -> Option<R> {
    let (_, workspace) = WorkspaceRegistry::as_ref(ctx)
        .all_workspaces(ctx)
        .into_iter()
        .find(|(_, ws)| ws.as_ref(ctx).ide_team_tab_of(view, ctx).is_some())?;
    Some(workspace.update(ctx, f))
}

fn with_workspace_of_ref<R>(view: EntityId, ctx: &AppContext, f: impl FnOnce(&Workspace) -> R) -> Option<R> {
    let (_, workspace) = WorkspaceRegistry::as_ref(ctx)
        .all_workspaces(ctx)
        .into_iter()
        .find(|(_, ws)| ws.as_ref(ctx).ide_team_tab_of(view, ctx).is_some())?;
    Some(f(workspace.as_ref(ctx)))
}

/// Asks the user whether terminal `caller` may spawn sessions.
fn ask_user(caller: EntityId, ctx: &mut ModelContext<TeamControl>) {
    let skip = *TeamSettings::as_ref(ctx).spawn_skip_permissions.value();
    let title = CLIAgentSessionsModel::as_ref(ctx)
        .session(caller)
        .and_then(|s| s.session_context.cwd.clone())
        .unwrap_or_else(|| "this pane".to_string());
    let info = format!(
        "The Claude Code session in {title} wants to start Claude Code sessions in panes next to it{}. \
         Allow it for as long as this pane is open?",
        if skip { ", with permission prompts turned off (agents.team.spawn_skip_permissions)" } else { "" }
    );
    let buttons = vec![
        ModalButton::for_app("Allow", move |app| {
            TeamControl::handle(app).update(app, |me, ctx| me.answer(caller, true, ctx));
        }),
        ModalButton::for_app("Deny", move |app| {
            TeamControl::handle(app).update(app, |me, ctx| me.answer(caller, false, ctx));
        }),
    ];
    // The third argument runs when the user ticks macOS's "do not show again"
    // box, before the clicked button's callback; only the button decides here.
    let dialog = AlertDialogWithCallbacks::for_app("Let this session start sessions?", info, buttons, |_| {});
    if cfg!(target_os = "macos") {
        ctx.show_native_platform_modal(dialog);
    } else {
        with_workspace_of(caller, ctx, |ws, wctx| ws.show_native_modal(dialog, wctx));
    }
}

/// Starts the HTTP server on a random loopback port. Requests go to the main
/// thread through `tx`; the HTTP side only checks transport-level things.
fn start_server(tx: async_channel::Sender<TeamRequest>) -> anyhow::Result<(tokio::runtime::Runtime, SocketAddr)> {
    use axum::body::Bytes;
    use axum::extract::State;
    use axum::http::{header, HeaderMap, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::post;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("mnemos-team-mcp")
        .enable_io()
        .enable_time()
        .build()?;
    let listener = runtime.block_on(tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))))?;
    let addr = listener.local_addr()?;

    #[derive(Clone)]
    struct Shared {
        tx: async_channel::Sender<TeamRequest>,
        hosts: [String; 2],
    }

    async fn mcp_post(State(shared): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
        // A browser always sends Origin on a cross-site POST; Claude Code never does.
        if headers.contains_key(header::ORIGIN) {
            return StatusCode::FORBIDDEN.into_response();
        }
        let host_ok = headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .is_some_and(|h| shared.hosts.iter().any(|ok| ok == h));
        if !host_ok {
            return StatusCode::FORBIDDEN.into_response();
        }
        let Some(token) = headers
            .get(header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.strip_prefix("Bearer "))
            .map(str::trim)
            .filter(|t| !t.is_empty() && !t.starts_with("${"))
        else {
            return StatusCode::UNAUTHORIZED.into_response();
        };
        let incoming = mcp::parse(&body);
        if let Incoming::Notification { .. } = incoming {
            return StatusCode::ACCEPTED.into_response();
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        let request = TeamRequest { token: token.to_string(), body: incoming, reply: reply_tx };
        if shared.tx.send(request).await.is_err() {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        match reply_rx.await {
            Ok(Reply::Json(value)) => (
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::to_vec(&value).unwrap_or_default(),
            )
                .into_response(),
            Ok(Reply::Unauthorized) => StatusCode::UNAUTHORIZED.into_response(),
            Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }
    }

    let shared = Shared {
        tx,
        hosts: [format!("127.0.0.1:{}", addr.port()), format!("localhost:{}", addr.port())],
    };
    let app = axum::Router::new()
        .route("/mcp", post(mcp_post).get(|| async { StatusCode::METHOD_NOT_ALLOWED }))
        .with_state(shared);
    runtime.spawn(async move {
        if let Err(err) = axum::serve(listener, app).await {
            log::warn!(target: "mnemos_ide_team", "team MCP server stopped: {err:#}");
        }
    });
    Ok((runtime, addr))
}

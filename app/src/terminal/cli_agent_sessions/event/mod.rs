mod v1;

use serde::Deserialize;

use crate::terminal::CLIAgent;

#[cfg_attr(not(feature = "local_tty"), allow(dead_code))]
type EventParser = fn(&str) -> Option<CLIAgentEvent>;

/// Sentinel title that identifies structured CLI agent events sent via OSC 777.
/// The `"agent"` field in the JSON body distinguishes which agent sent it.
///
/// We accept BOTH `mnemos://cli-agent` (the v2 native title) and the legacy
/// `warp://cli-agent` (still emitted by every shipped Claude Code/Codex
/// plugin since their `MNEMOS_TERMINAL=1` install path predates Mnemos's fork).
/// Without the legacy match the deep-link falls through to a generic OSC 777
/// toast that surfaces the raw JSON to the user.
pub const CLI_AGENT_NOTIFICATION_SENTINEL: &str = "mnemos://cli-agent";
pub const CLI_AGENT_NOTIFICATION_SENTINEL_LEGACY: &str = "warp://cli-agent";

/// Returns true when an OSC 777 title is one of the recognized CLI agent
/// sentinels (current Mnemos or legacy Mnemos).
pub fn is_cli_agent_sentinel(title: Option<&str>) -> bool {
    matches!(
        title,
        Some(CLI_AGENT_NOTIFICATION_SENTINEL) | Some(CLI_AGENT_NOTIFICATION_SENTINEL_LEGACY),
    )
}

/// Env var carrying this terminal's CLI agent event token. Mnemos IDE: OSC 777
/// `mnemos://cli-agent` events used to be accepted from any output in the
/// pane (`cat` of a file, a remote host, a web page piped through curl), so
/// anything printed could start phantom agent sessions, raise notifications
/// and reach the cross-machine agents feed. A sentinel title is now trusted
/// only as `mnemos://cli-agent#<token>`, with the token Mnemos exported to
/// this PTY's processes. The file transport (`file_ingest`) is authenticated
/// by its per-terminal path, which carries the same token.
pub const CLI_AGENT_TOKEN_ENV_VAR: &str = "MNEMOS_CLI_AGENT_TOKEN";

/// What an OSC 777 title says about where the notification came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliAgentTitleCheck {
    /// Not a CLI agent title: an ordinary desktop notification.
    NotSentinel,
    /// A CLI agent title carrying this terminal's token, or the token of
    /// another live terminal of this app (an agent in a tmux session started
    /// from another pane keeps that pane's token).
    Authentic,
    /// A bare CLI agent title with no token. The Gemini and OpenCode plugins,
    /// the upstream Claude plugin before its hook is patched and mnemos-agents
    /// before 1.2.0 send these. Dropped unless the user enabled
    /// `AcceptUntokenedCliAgentEvents` ([`accept_untokened_events`]) or the
    /// agent's installed plugin predates the token
    /// ([`installed_plugin_predates_token`]), and then trusted only while the
    /// pane itself runs that agent ([`agent_runs_command`]).
    Unverified,
    /// A CLI agent title with a token no live terminal of this app owns (a
    /// tmux session that outlived a restart, or made up). Dropped.
    Forged,
}

/// Mnemos IDE: mirrors the `AcceptUntokenedCliAgentEvents` setting (see
/// `event_settings`), off by default, so the terminal model can read it while parsing
/// PTY output without an app context. Kept current by `CLIAgentEventFileIngestor`.
static ACCEPT_UNTOKENED_EVENTS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_accept_untokened_events(accept: bool) {
    ACCEPT_UNTOKENED_EVENTS.store(accept, std::sync::atomic::Ordering::Relaxed);
}

/// Whether [`CliAgentTitleCheck::Unverified`] events may be trusted at all (still only
/// while the pane runs the agent they name).
pub fn accept_untokened_events() -> bool {
    ACCEPT_UNTOKENED_EVENTS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Mnemos IDE: how long [`installed_plugin_predates_token`] reuses its read of
/// `installed_plugins.json`, so a plugin update made while the app runs is noticed
/// without reading the file for every event.
#[cfg(not(target_family = "wasm"))]
const INSTALLED_PLUGIN_RECHECK: std::time::Duration = std::time::Duration::from_secs(30);

#[cfg(not(target_family = "wasm"))]
static MNEMOS_AGENTS_PREDATES_TOKEN: std::sync::Mutex<Option<(std::time::Instant, bool)>> =
    std::sync::Mutex::new(None);

/// Mnemos IDE: whether `agent`'s Mnemos plugin installed on this machine predates
/// the PTY token, so its events arrive without one. True only for Claude Code with
/// mnemos-agents below 1.2.0. Such an install (every machine set up before the
/// token) keeps its agents panel working, with bare events trusted only while the
/// pane runs `claude`, as before the token; once the plugin is updated its events
/// carry the token and bare ones are dropped again.
pub fn installed_plugin_predates_token(agent: CLIAgent) -> bool {
    if !matches!(agent, CLIAgent::Claude) {
        return false;
    }
    #[cfg(not(target_family = "wasm"))]
    {
        let now = std::time::Instant::now();
        let Ok(mut cached) = MNEMOS_AGENTS_PREDATES_TOKEN.lock() else {
            return false;
        };
        if let Some((checked, predates)) = *cached {
            if now.duration_since(checked) < INSTALLED_PLUGIN_RECHECK {
                return predates;
            }
        }
        let predates = super::plugin_manager::claude::mnemos_agents_predates_token();
        *cached = Some((now, predates));
        predates
    }
    #[cfg(target_family = "wasm")]
    {
        false
    }
}

/// Mnemos IDE: the CLI agent tokens of every live terminal in this app, so an
/// event from a process that inherited another pane's token (tmux, screen) is
/// still recognised. Filled and emptied by `CLIAgentEventFileIngestor`.
static LIVE_TOKENS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::LazyLock::new(Default::default);

pub(crate) fn register_live_token(token: &str) {
    if let Ok(mut tokens) = LIVE_TOKENS.lock() {
        tokens.insert(token.to_owned());
    }
}

pub(crate) fn unregister_live_token(token: &str) {
    if let Ok(mut tokens) = LIVE_TOKENS.lock() {
        tokens.remove(token);
    }
}

fn is_live_token(supplied: &str) -> bool {
    LIVE_TOKENS.lock().is_ok_and(|tokens| {
        tokens
            .iter()
            .any(|token| constant_time_eq(supplied.as_bytes(), token.as_bytes()))
    })
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    !a.is_empty()
        && a.len() == b.len()
        && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Classify an OSC 777 `title` against this terminal's token.
pub fn check_cli_agent_title(title: Option<&str>, token: Option<&str>) -> CliAgentTitleCheck {
    let Some(title) = title else {
        return CliAgentTitleCheck::NotSentinel;
    };
    let (sentinel, supplied) = match title.split_once('#') {
        Some((sentinel, supplied)) => (sentinel, Some(supplied)),
        None => (title, None),
    };
    if !is_cli_agent_sentinel(Some(sentinel)) {
        return CliAgentTitleCheck::NotSentinel;
    }
    match supplied {
        None => CliAgentTitleCheck::Unverified,
        Some(supplied)
            if token.is_some_and(|token| constant_time_eq(supplied.as_bytes(), token.as_bytes()))
                || is_live_token(supplied) =>
        {
            CliAgentTitleCheck::Authentic
        }
        Some(_) => CliAgentTitleCheck::Forged,
    }
}

/// Mnemos IDE: whether `command`, the pane's running command, starts `agent`
/// (its first word, after any `VAR=value` prefixes, is the agent's command,
/// with or without a directory). An event without a token is trusted only
/// then: the agent the user started in this pane is what printed it, not a
/// `cat`, a remote host or a program that merely prints the sequence.
pub fn agent_runs_command(agent: CLIAgent, command: &str) -> bool {
    if matches!(agent, CLIAgent::Unknown) {
        return false;
    }
    let Some(first) = command.split_whitespace().find(|word| !word.contains('=')) else {
        return false;
    };
    let program = first.rsplit(['/', '\\']).next().unwrap_or(first);
    let program = program.strip_suffix(".exe").unwrap_or(program);
    program == agent.command_prefix()
}

/// The event type encoded in the `"event"` field of the JSON body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CLIAgentEventType {
    SessionStart,
    PromptSubmit,
    ToolComplete,
    Stop,
    /// The turn ended on an error (API error, connection lost while the
    /// computer slept). Claude Code's `StopFailure` hook; `response` carries
    /// the error text.
    StopFailure,
    PermissionRequest,
    PermissionReplied,
    QuestionAsked,
    IdlePrompt,
    Unknown(String),
}

/// Event-specific fields that vary by event type.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct CLIAgentEventPayload {
    pub query: Option<String>,
    pub response: Option<String>,
    pub transcript_path: Option<String>,
    pub summary: Option<String>,
    pub tool_name: Option<String>,
    pub tool_input_preview: Option<String>,
    pub plugin_version: Option<String>,
    /// The session's claude.ai Remote Control URL, when the plugin found one
    /// in the transcript and it passed `mnemos_ide_agents::is_remote_control_url`.
    pub remote_url: Option<String>,
    /// On `stop`: background tasks (subagents, background shells, monitors)
    /// the transcript shows as started and not yet finished. `None` from
    /// plugins that predate the field.
    pub background_tasks: Option<u32>,
    /// Whether the session runs with permission checks bypassed, from the
    /// hook's `permission_mode`. `None` when the event carries no mode
    /// (SessionStart, or a plugin older than mnemos-agents 1.3.2).
    pub bypass_permissions: Option<bool>,
}

/// A parsed event from a CLI agent plugin.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct CLIAgentEvent {
    pub v: u32,
    pub agent: CLIAgent,
    pub event: CLIAgentEventType,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub project: Option<String>,
    pub payload: CLIAgentEventPayload,
}

/// Version-specific parsers, indexed by (version - 1).
/// Adding a new version means appending a parser here,
/// which automatically bumps `current_protocol_version()`.
#[cfg_attr(not(feature = "local_tty"), allow(dead_code))]
const VERSIONED_PARSERS: &[EventParser] = &[v1::parse];

/// The current CLI agent protocol version this build of Mnemos supports.
/// Exported as both `MNEMOS_CLI_AGENT_PROTOCOL_VERSION` and
/// `WARP_CLI_AGENT_PROTOCOL_VERSION` env vars on the PTY so plugins
/// (including the upstream Warp plugin) can negotiate a compatible payload format.
#[cfg_attr(not(feature = "local_tty"), allow(dead_code))]
pub const fn current_protocol_version() -> u32 {
    VERSIONED_PARSERS.len() as u32
}

/// Attempts to parse an OSC 777 `PluggableNotification` into a typed `CLIAgentEvent`.
/// Dispatches to the correct version-specific parser based on the `"v"` field. Returns `None`
/// if the title doesn't match the sentinel, the body isn't valid JSON, or the version is unsupported.
pub fn parse_event(title: Option<&str>, body: &str) -> Option<CLIAgentEvent> {
    if !is_cli_agent_sentinel(title) {
        return None;
    }

    let version_probe: VersionProbe = serde_json::from_str(body).ok()?;
    let version = version_probe.v.unwrap_or(1);

    let index = (version as usize).checked_sub(1)?;
    match VERSIONED_PARSERS.get(index) {
        Some(parser) => parser(body),
        None => {
            log::error!(
                "Received CLI agent event with unsupported schema version \
                 {version}. The CLI agent plugin or Mnemos may need to be updated."
            );
            None
        }
    }
}

#[derive(Deserialize)]
struct VersionProbe {
    v: Option<u32>,
}

// SPDX-License-Identifier: AGPL-3.0-only
//! Wire types between Mnemos's `CLIAgentSessionsModel` and `AgentsPanel`.

use serde::{Deserialize, Serialize};

/// Which CLI agent the row represents. Cancel is supported for Claude
/// (PTY Ctrl-C); other variants render status but no ✕ button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentKind {
    Claude,
    Codex,
    Gemini,
    OpenCode,
    Other,
}

impl AgentKind {
    pub fn label(&self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::Gemini => "gemini",
            AgentKind::OpenCode => "opencode",
            AgentKind::Other => "agent",
        }
    }

    pub fn supports_cancel(&self) -> bool {
        matches!(self, AgentKind::Claude)
    }
}

/// Status pill on a row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentStatus {
    InProgress,
    Blocked { message: Option<String> },
    Success,
}

impl AgentStatus {
    pub fn label(&self) -> &'static str {
        match self {
            AgentStatus::InProgress => "running",
            AgentStatus::Blocked { .. } => "blocked",
            AgentStatus::Success => "idle",
        }
    }
}

/// Another machine's agents, as published through admin sync. Shown read-only
/// under the machine's name: there is no terminal here to switch to.
#[derive(Debug, Clone)]
pub struct RemoteMachine {
    /// Host name, e.g. "ALICE" or "Alice-MacBook-Pro".
    pub label: String,
    /// `darwin`, `win32` or `linux`.
    pub platform: String,
    pub rows: Vec<AgentRow>,
}

impl RemoteMachine {
    /// "Mac", "Windows" or "Linux", for the section header.
    pub fn platform_name(&self) -> &'static str {
        platform_name(&self.platform)
    }
}

/// "Mac", "Windows" or "Linux" for a `darwin` / `win32` / `linux` platform
/// key, else "Machine".
pub fn platform_name(platform: &str) -> &'static str {
    match platform {
        "darwin" => "Mac",
        "win32" => "Windows",
        "linux" => "Linux",
        _ => "Machine",
    }
}

/// Severity of a [`PluginHint`] — drives the banner tint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginHintSeverity {
    /// Plugin missing or outdated — responses can't work until fixed.
    Warning,
    /// Plugin is fine but no events have arrived — likely needs a reload.
    Info,
}

/// A one-line banner shown above the agent rows when the Warp Claude Code
/// plugin can't be feeding OSC events (so query/response previews are blank).
/// The app computes this from `installed_plugins.json` + live session state and
/// pushes it via [`crate::AgentsPanel::set_plugin_hint`]. `None` hides the banner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginHint {
    pub message: String,
    pub severity: PluginHintSeverity,
}

/// One row in the Agents panel: the projection of a CLI agent session.
/// `terminal_view_id_bits` is the EntityId of the terminal view backing it.
#[derive(Debug, Clone)]
pub struct AgentRow {
    pub terminal_view_id_bits: u64,
    pub agent: AgentKind,
    pub status: AgentStatus,
    /// Project label (display name). Empty string if unknown — we group those
    /// rows under an "Unattached" header.
    pub project: String,
    /// Working directory for grouping. Used as the sort key when `project` is
    /// not provided.
    pub cwd: Option<String>,
    /// Last user query (truncated to 80 chars).
    pub query: Option<String>,
    /// Latest agent response (truncated to 160 chars). Populated on `Stop`
    /// events from the CLI agent plugin.
    pub response: Option<String>,
    /// Tool-in-flight name (e.g. "Write", "Bash") — only set while blocked
    /// awaiting permission.
    pub tool_name: Option<String>,
    /// When the session last reported anything. Drives the "3m" age and the
    /// recency order. `None` for sessions that predate the field.
    pub last_activity: Option<std::time::SystemTime>,
    /// Background tasks (subagents, background shells, monitors) still
    /// running after the turn ended. Nonzero keeps the row in WORKING with
    /// "N background tasks" in place of "done".
    pub background_tasks: u32,
    /// The session's claude.ai Remote Control URL. Only rows from another
    /// machine use it ("open" drives that session from here), and only when
    /// [`is_remote_control_url`] accepts it.
    pub remote_url: Option<String>,
    /// The Claude Code session id behind the row, when the agent reported
    /// one. Lets the sessions list tell a transcript that is running right
    /// now (here or on another machine) from one that can be resumed.
    pub session_id: Option<String>,
}

/// What can be done with a past Claude Code session from here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    /// Nothing runs it: click resumes the original, right-click a fork.
    Resumable,
    /// No machine is known to run it, but its newest turns were pulled in
    /// from another machine a few minutes ago and nothing ran here since.
    /// That machine's `claude` may still be running unseen (its
    /// `agents-live` snapshot gone stale, or never published), and resuming
    /// in place would take its Remote Control session over, so both clicks
    /// fork, as for [`SessionState::LiveElsewhere`].
    PulledRecently,
    /// A terminal on this machine is running it. Both clicks switch to that
    /// terminal; starting a second `claude` on the same id would fork the
    /// transcript under its own feet.
    LiveHere { terminal_view_id_bits: u64 },
    /// Another machine is running it (its `agents-live` snapshot names this
    /// id). Resuming it in place here would make two writers on one session,
    /// so only the fork (from what has synced so far) and, with a Remote
    /// Control URL, "open" are offered.
    LiveElsewhere {
        /// The other machine's host label.
        machine: String,
        remote_url: Option<String>,
    },
    /// The directory it ran in does not exist on this machine, so
    /// `claude --resume` has nowhere to run (and from anywhere else it would
    /// look in the wrong slug folder and not find the session).
    FolderMissing,
}

/// One past Claude Code session in the agents panel's SESSIONS section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    /// The session UUID, the argument to `claude --resume`.
    pub id: String,
    pub title: String,
    /// `title` is Claude's own summary rather than the first prompt.
    pub title_is_ai: bool,
    pub branch: Option<String>,
    /// Transcript mtime: the age column and the order.
    pub modified: std::time::SystemTime,
    /// Where it ran, on this machine (synced transcripts carry this
    /// machine's equivalent of the other machine's path).
    pub cwd: std::path::PathBuf,
    /// "Windows", "Mac" or "Linux" when the latest turns came in from that
    /// machine through transcript sync and nothing ran here since.
    pub origin: Option<String>,
    pub state: SessionState,
}

/// A project's sessions, newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionGroup {
    /// Registry id, when the root is a registered project.
    pub project_id: Option<String>,
    /// Display name.
    pub project: String,
    /// The project's checkout on this machine.
    pub root: std::path::PathBuf,
    pub rows: Vec<SessionRow>,
}

/// What every Claude Code Remote Control session URL starts with.
pub const REMOTE_CONTROL_URL_PREFIX: &str = "https://claude.ai/code/session_";

/// Whether `url` is a claude.ai Remote Control session URL and nothing else:
/// the prefix, then a session id of ASCII letters, digits, `_` and `-`. The
/// URL comes from another machine through admin sync, so the panel opens
/// nothing that fails this.
pub fn is_remote_control_url(url: &str) -> bool {
    url.strip_prefix(REMOTE_CONTROL_URL_PREFIX)
        .is_some_and(|id| {
            !id.is_empty()
                && id.len() <= 128
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_control_url_accepts_only_a_bare_session_url() {
        assert!(is_remote_control_url(
            "https://claude.ai/code/session_01AAA"
        ));
        assert!(is_remote_control_url(
            "https://claude.ai/code/session_a-b_C9"
        ));
        assert!(!is_remote_control_url("https://claude.ai/code/session_"));
        assert!(!is_remote_control_url(
            "https://claude.ai/code/session_01AAA/x"
        ));
        assert!(!is_remote_control_url(
            "https://claude.ai/code/session_01AAA?x=1"
        ));
        assert!(!is_remote_control_url(
            "https://claude.ai.evil.com/code/session_01AAA"
        ));
        assert!(!is_remote_control_url(
            "http://claude.ai/code/session_01AAA"
        ));
        assert!(!is_remote_control_url(&format!(
            "https://claude.ai/code/session_{}",
            "a".repeat(129)
        )));
    }
}

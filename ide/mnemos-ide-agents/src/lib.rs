// SPDX-License-Identifier: AGPL-3.0-only
//! Mnemos IDE Agents: Tools-panel list of running agent sessions (CLI agents such
//! as Claude Code, Codex, Gemini and OpenCode), grouped by project, and below
//! them the past Claude Code sessions of every project (both machines', once
//! transcript sync has brought them over). Clicking a running row focuses its
//! tab; Claude rows have a cancel button; clicking a past session resumes it
//! inside its project's tab. `LeftPanelView` hosts it as a Tools-panel view.
//!
//! Running rows come from `app::terminal::cli_agent_sessions::CLIAgentSessionsModel`
//! (pushed via `set_rows`), other machines' from `agents-live` snapshots
//! (`set_remote_machines`), past sessions from the transcripts on disk
//! (`set_session_groups`). The panel never reads the disk or the network.

#![forbid(unsafe_code)]
// Mnemos IDE: alongside the forbid above. If unsafe is ever allowed in this crate, every
// unsafe block needs a `// SAFETY:` comment.
#![deny(clippy::undocumented_unsafe_blocks)]

pub mod types;
pub mod view;

pub use types::{
    is_remote_control_url, platform_name, AgentKind, AgentRow, AgentStatus, PluginHint,
    PluginHintSeverity, RemoteMachine, SessionGroup, SessionRow, SessionState,
};
pub use view::{AgentsPanel, AgentsPanelEvent, CancelTarget};

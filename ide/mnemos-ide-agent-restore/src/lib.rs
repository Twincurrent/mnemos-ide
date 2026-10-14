// SPDX-License-Identifier: AGPL-3.0-only
//! Mnemos IDE Agent Restore: carry live Claude Code sessions across an app restart.
//!
//! Mnemos already restores the *shape* of a workspace across a restart: the
//! tabs, the pane tree and each terminal pane's working directory are written
//! to SQLite (`terminal_panes.cwd`, `terminal_panes.uuid`) and replayed on
//! launch. What it does not restore is the **agent** that was running inside
//! those panes — an autoupdate silently drops every live `claude` session.
//!
//! This crate is the missing half. It stores nothing about layout, because
//! upstream already does that. It stores only the association
//!
//! ```text
//!     pane uuid  ->  claude session id  (+ the last cwd it reported)
//! ```
//!
//! and hands it back on the next launch so the app can drop
//! `claude --resume <id>` into the *same* pane that came back. Because the
//! pane uuid is stable across runs (see `TerminalPane::uuid`, documented as
//! "used to uniquely identify the pane, even across separate runs of the
//! app"), no new tabs are ever created and there is nothing to duplicate.
//!
//! # Safety posture
//!
//! A restore mechanism that misfires is worse than none — it can spawn panes
//! or run commands in directories that no longer exist. Four rules keep that
//! from happening, all enforced in [`model`]:
//!
//! 1. **Consume-on-read.** [`RestoreSnapshot::take_blocking`] deletes the file
//!    as it loads it. A restore that crashes the app cannot repeat on the next
//!    launch, so there is no way to build a restore loop.
//! 2. **Validated before replay.** [`RestoreSnapshot::resumable`] drops any
//!    entry whose session id is not shaped like a UUID, for which no
//!    transcript exists under `~/.claude/projects/`, or whose launch dir
//!    (the first `cwd` recorded at the head of that transcript) is gone. The
//!    `cwd` in the snapshot is not what gets checked: it is the last one the
//!    hooks reported, it moves with every `cd` the agent makes, and it is
//!    only used as a hint for finding the transcript. This crate never sets
//!    the pane's cwd; the pane restores its own saved cwd, which is normally
//!    the directory `claude` was started in.
//! 3. **Capped.** At most [`MAX_RESTORED_SESSIONS`] entries survive, and a
//!    snapshot older than [`MAX_SNAPSHOT_AGE_SECS`] is discarded whole.
//! 4. **Deduped by session id.** Claude Code writes subagent transcripts
//!    *underneath* the parent's uuid
//!    (`projects/<cwd>/<session_uuid>/subagents/*.jsonl`), so a subagent
//!    reports its parent's `session_id`. Resuming the same id once per pane
//!    that saw it would fork the same conversation N times.
//!
//! One more rule lives with the app, which has the facts: a session another
//! machine may still be running is restored as a fork, never in place
//! ([`guard::resume_mode`]), because a second `claude --resume` of the same id
//! takes its Remote Control session over. And a pane the user typed or ran
//! something in while the restore waited never gets the command: its entry
//! is carried into the next snapshot instead ([`queue::defer`]).
//!
//! No I/O beyond a single small JSON file and a bounded look at
//! `~/.claude/projects/` (a stat per slug dir and the head of the chosen
//! transcript), no network, no tokio: every entry point is blocking and safe
//! to call from `on_will_terminate`, which is a synchronous shutdown hook that
//! must not spawn work it cannot await.

#![forbid(unsafe_code)]
// Mnemos IDE: alongside the forbid above. If unsafe is ever allowed in this crate, every
// unsafe block needs a `// SAFETY:` comment.
#![deny(clippy::undocumented_unsafe_blocks)]

pub mod guard;
pub mod model;
pub mod queue;

pub use guard::{resume_mode, AgentsLive, NewestTurn, ResumeMode, RECENT_ELSEWHERE};
pub use model::{
    claude_program, fork_command, fork_command_with, resume_command, resume_command_with,
    snapshot_path, transcript_stat, RestoreError, RestoreSnapshot,
    SessionEntry, DEFAULT_CLAUDE_PROGRAM, MAX_CLAUDE_PROGRAM_LEN, MAX_RESTORED_SESSIONS, MAX_SNAPSHOT_AGE_SECS, SNAPSHOT_VERSION,
};
pub use queue::{
    defer, forget_deferred, last_live_at, pane_uuid_hex, snapshot_saved_at, take_matching,
    with_deferred,
};

/// Kill switch. Set `MNEMOS_NO_AGENT_RESTORE=1` and neither half runs: nothing
/// is written at shutdown and nothing is replayed at launch.
///
/// This exists so a restore that misbehaves can be shut off without a new
/// build — the user can export it from their shell and relaunch. It is checked
/// on both halves, so setting it also stops a snapshot accumulating while it
/// is off.
pub fn is_disabled() -> bool {
    matches!(
        std::env::var("MNEMOS_NO_AGENT_RESTORE").as_deref(),
        Ok("1") | Ok("true")
    )
}

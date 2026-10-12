// SPDX-License-Identifier: AGPL-3.0-only
//! Mnemos IDE Sessions: reads Claude Code's own session transcripts.
//!
//! Claude Code already writes every conversation to
//! `~/.claude/projects/<slug>/<uuid>.jsonl`, and the `claude` CLI already
//! knows how to resume one. This crate is the filesystem half of the
//! sessions list in the agents panel: enumerate those transcripts for a
//! project root (sessions started in its subfolders included), and pull a
//! cheap label out of each. The rows themselves are drawn by
//! `mnemos-ide-agents`; the app turns a click into `claude --resume <uuid>`
//! inside the project's own tab.
//!
//! No storage of our own, no sync, no network, no UI. Transcripts synced in
//! from another machine (`mnemos-ide-sync`) already sit in the right slug
//! directory with their `cwd` mapped to this machine, so they list here like
//! local ones. If Claude Code changes its on-disk layout the list goes empty;
//! it never invents data.
//!
//! [`suggest`] holds the pure half of the panel's "Suggest" (the instruction,
//! the `claude -p` arguments and the output cleanup); the app runs the
//! process.

#![forbid(unsafe_code)]
// Mnemos IDE: alongside the forbid above. If unsafe is ever allowed in this crate, every
// unsafe block needs a `// SAFETY:` comment.
#![deny(clippy::undocumented_unsafe_blocks)]

pub mod exchange;
pub mod model;
pub mod suggest;

pub use exchange::{last_exchange, LastExchange};
pub use model::{
    claude_projects_dir, is_safe_session_id, is_within, project_slug, SessionError, SessionMeta,
    SessionStore, MAX_SESSIONS,
};

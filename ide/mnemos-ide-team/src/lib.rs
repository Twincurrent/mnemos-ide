// SPDX-License-Identifier: AGPL-3.0-only
//! Mnemos IDE Team: a coordinator Claude Code session starting and managing
//! worker sessions in the same project tab.
//!
//! The app runs a small MCP server on loopback (`app/src/workspace/team_control.rs`).
//! A Claude Code session in a Mnemos terminal connects to it with that
//! terminal's `MNEMOS_CLI_AGENT_TOKEN` as a bearer token, which both
//! authenticates the caller and names the pane it runs in. The tools let it
//! split a new pane next to itself running `claude` with a brief, list, focus,
//! rename and retire those sessions, and clean up the git worktrees they used.
//!
//! This crate holds the parts that need no UI, so they are unit-tested here:
//! - [`board`]: the team status board the agents panel shows.
//! - [`names`]: validation of session names, model names and briefs.
//! - [`command`]: the command line and environment a spawned pane runs.
//! - [`limits`]: the caps on live sessions and spawns per minute.
//! - [`leases`]: leases on shared resources (cpu, chrome, ports).
//! - [`worktree`]: creating, inspecting and removing git worktrees.
//! - [`mcp`]: the MCP JSON-RPC messages and tool schemas.

#![forbid(unsafe_code)]

pub mod board;
pub mod command;
pub mod leases;
pub mod limits;
pub mod mcp;
pub mod names;
pub mod worktree;

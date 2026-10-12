// SPDX-License-Identifier: AGPL-3.0-only
//! `mnemos-ide-sync`: Admin platform auth + state-sync API client.
//!
//! This crate is the Rust port of v1's `apps/frontend/src/main/{admin-auth,sync-client,sync-manager}.ts`.
//! Live implementation: reqwest HTTP client (profile + team scopes), `notify`-based
//! file-watcher, CAS push with per-key merge, a live pull of the project
//! registry, Claude transcript sync, and an observable `SyncStatusSnapshot`
//! watch channel that feeds the Mnemos Drive panel.

#![forbid(unsafe_code)]
// Mnemos IDE: alongside the forbid above. If unsafe is ever allowed in this crate, every
// unsafe block needs a `// SAFETY:` comment.
#![deny(clippy::undocumented_unsafe_blocks)]

pub mod auth;
pub mod client;
pub mod crypto;
pub mod desktop_auth;
pub mod e2e;
pub mod error;
pub mod gates;
pub mod live_agents;
pub mod machine;
pub mod manager;
pub mod merge;
pub mod options;
pub mod protocol;
pub mod settings_toml;
mod team_choice;
pub mod teams;
pub mod transcripts;
pub mod watcher;

pub use auth::AdminAuth;
pub use client::SyncClient;
pub use crypto::{E2eStatus, RecoveryKeyError, SyncKey};
pub use e2e::{E2eReport, UnlockError};
pub use error::IdeError;
pub use gates::{GateSource, PendingApproval};
pub use manager::{KeySyncState, KeySyncStatus, SyncAuthState, SyncManager, SyncStatusSnapshot};
pub use settings_toml::{SettingsKeyScope, SettingsPolicy};
pub use teams::{Team, TeamMember};

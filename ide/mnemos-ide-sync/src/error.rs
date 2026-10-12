// SPDX-License-Identifier: AGPL-3.0-only
//! mnemos-ide-sync's error type.
//!
//! Deliberately separate from `mnemos_ide_core::IdeError`: mnemos-ide-core does no network
//! or keyring I/O, so the reqwest, keyring and `/api/tcb/state` CAS-conflict
//! variants live here. mnemos-ide-core errors are wrapped as [`IdeError::Store`].

use thiserror::Error;

#[derive(Debug, Error)]
pub enum IdeError {
    #[error("not authenticated")]
    NotAuthenticated,

    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("keyring error: {0}")]
    Keyring(#[from] keyring::Error),

    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("auth error: {0}")]
    Auth(String),

    /// 409 Conflict from `/api/tcb/state` — caller must reconcile against
    /// `current_version` + `current_value` returned by the server.
    #[error("CAS conflict on key {key}: server version is {current_version}, server value {current_value}")]
    CasConflict {
        key: String,
        current_version: i64,
        current_value: serde_json::Value,
    },

    #[error("http error: {status} {body}")]
    Http {
        status: reqwest::StatusCode,
        body: String,
    },

    /// Loading, parsing or saving `projects.json` through `mnemos-ide-projects`'
    /// locked store failed.
    #[error("project store error: {0}")]
    Store(#[from] mnemos_ide_core::IdeError),

    #[error("other: {0}")]
    Other(String),
}

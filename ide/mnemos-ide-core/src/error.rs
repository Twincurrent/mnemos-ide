// SPDX-License-Identifier: AGPL-3.0-only
use thiserror::Error;

#[derive(Debug, Error)]
pub enum IdeError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("could not resolve user data directory")]
    MissingDataDir,

    #[error("invalid project id: {0}")]
    InvalidProjectId(String),
}

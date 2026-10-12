// SPDX-License-Identifier: AGPL-3.0-only
//! Per-machine sync choices, kept in `<Mnemos store>/sync_options.json`.
//!
//! Local only: this file is never pushed (nothing classifies it as a sync
//! key) and never pulled, so each machine decides for itself. A missing or
//! unreadable file means every option at its default.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::IdeError;

pub const OPTIONS_FILE: &str = "sync_options.json";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncOptions {
    /// Sync `~/.claude/.env` (key `claude-env`). Off by default: the file
    /// usually holds API keys and other secrets. When off, this machine
    /// neither uploads its `.env` nor writes one it pulled.
    #[serde(default)]
    pub claude_env: bool,
}

/// The options file in the default Mnemos store, `None` without a data dir.
pub fn default_path() -> Option<PathBuf> {
    mnemos_ide_core::paths::ide_data_dir()
        .ok()
        .map(|d| d.join("store").join(OPTIONS_FILE))
}

/// The options saved at `path`, defaults when there are none.
pub fn load(path: &Path) -> SyncOptions {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Save `options` at `path` (write a temp file, then rename over).
pub fn save(path: &Path, options: &SyncOptions) -> Result<(), IdeError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| IdeError::Other(format!("create {}: {e}", dir.display())))?;
    }
    let bytes = serde_json::to_vec_pretty(options)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)
        .map_err(|e| IdeError::Other(format!("write {}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| IdeError::Other(format!("save {}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_broken_options_are_the_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("store").join(OPTIONS_FILE);
        assert_eq!(load(&path), SyncOptions::default());
        assert!(!load(&path).claude_env, "claude-env is off by default");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(load(&path), SyncOptions::default());
    }

    #[test]
    fn options_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("store").join(OPTIONS_FILE);
        save(&path, &SyncOptions { claude_env: true }).unwrap();
        assert!(load(&path).claude_env);
        save(&path, &SyncOptions { claude_env: false }).unwrap();
        assert!(!load(&path).claude_env);
    }
}

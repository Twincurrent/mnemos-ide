// SPDX-License-Identifier: AGPL-3.0-only
//! This machine's identity for sync: one record shared by transcript sync and
//! the live agents feed.
//!
//! The id is a random UUID minted on first use and kept in
//! `<Mnemos data dir>/store/machine_id` (local only, never synced). It names
//! the transcript segments this machine uploads
//! (`claude-sessions/{sid}/{id}-{seq}.jsonl.gz`) and rides along in the live
//! feed as the optional `machine_id` field. The live feed's state key keeps
//! its host-name suffix (`agents-live:<machine_key(host)>`), because other
//! readers (Iris) find "this machine" by that key; the id is what joins a
//! machine's live agents to its transcripts.
//!
//! A data-profiled build (`./script/run`) has its own data dir, so it gets its
//! own id, the same way it gets its own live-feed key.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

const TARGET: &str = "mnemos_ide_sync::machine";
pub(crate) const MACHINE_ID_FILE: &str = "machine_id";

/// Serializes reading and minting the id in this process, so transcript sync
/// and the live feed starting at the same moment cannot mint two ids and
/// leave one of them using an id the file no longer holds.
static ID_LOCK: Mutex<()> = Mutex::new(());

/// One machine as the sync layer knows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineRecord {
    /// Random, stable for this data dir ([`load_or_create_id`]).
    pub id: String,
    /// What to show: the host name as the machine reports it, without a
    /// trailing `.local`.
    pub label: String,
    /// `darwin`, `win32` or `linux`.
    pub platform: String,
}

impl MachineRecord {
    /// This machine's record: the id from the Mnemos data dir (minted on
    /// first use), `host` as the label. `None` when there is no data dir or
    /// the id file cannot be read or written.
    pub fn this_machine(host: &str) -> Option<Self> {
        let store = store_dir()?;
        let id = load_or_create_id(&store)
            .inspect_err(|e| tracing::warn!(target: TARGET, error = %e, "machine id unavailable"))
            .ok()?;
        Some(Self::new(id, host))
    }

    pub fn new(id: String, host: &str) -> Self {
        Self {
            id,
            label: host.trim().trim_end_matches(".local").to_string(),
            platform: mnemos_ide_core::registry::current_platform().to_string(),
        }
    }
}

fn store_dir() -> Option<PathBuf> {
    mnemos_ide_core::paths::ide_data_dir()
        .ok()
        .map(|d| d.join("store"))
}

/// Whether `s` can be a machine id (also the rule for ids read from the blob
/// store, which other machines wrote).
pub(crate) fn is_machine_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

/// The id kept in `<store_dir>/machine_id`, minted (a random UUID) when the
/// file is missing or unusable.
pub fn load_or_create_id(store_dir: &Path) -> std::io::Result<String> {
    let _guard = ID_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = store_dir.join(MACHINE_ID_FILE);
    match std::fs::read_to_string(&path) {
        Ok(s) if is_machine_id(s.trim()) => return Ok(s.trim().to_string()),
        Ok(_) => {
            tracing::warn!(target: TARGET, path = %path.display(), "machine id file is unusable; minting a new id");
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let id = uuid::Uuid::new_v4().simple().to_string();
    write_atomic(&path, id.as_bytes())?;
    tracing::info!(target: TARGET, machine = %id, "created machine id");
    Ok(id)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".tmp");
    let tmp = path.with_file_name(name);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_minted_once_and_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let a = load_or_create_id(tmp.path()).unwrap();
        assert!(is_machine_id(&a));
        assert_eq!(load_or_create_id(tmp.path()).unwrap(), a);
        // An unusable file is replaced.
        std::fs::write(tmp.path().join(MACHINE_ID_FILE), "../x").unwrap();
        let b = load_or_create_id(tmp.path()).unwrap();
        assert_ne!(a, b);
        assert!(is_machine_id(&b));
    }

    #[test]
    fn record_label_drops_local_suffix() {
        let r = MachineRecord::new("abc".into(), " Alice-MacBook.local ");
        assert_eq!(r.label, "Alice-MacBook");
        assert_eq!(r.id, "abc");
        assert!(["darwin", "win32", "linux"].contains(&r.platform.as_str()));
    }
}

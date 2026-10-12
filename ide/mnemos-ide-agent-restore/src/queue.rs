// SPDX-License-Identifier: AGPL-3.0-only
//! The one-shot launch queue.
//!
//! Restore runs per *window*: `Workspace::configure_new_workspace` is called
//! once for each window Mnemos brings up, and each one only knows about its
//! own panes. The snapshot, meanwhile, is a single file covering every pane
//! from the previous run.
//!
//! So the file cannot simply be read by whichever window happens to go first
//! — that window would consume entries belonging to panes it does not have,
//! and the second window would come up with dead agents. Instead the file is
//! loaded into a process-wide queue on first touch, and each window *drains
//! the entries matching the pane uuids it owns*, leaving the rest.
//!
//! The file itself is still consumed exactly once, on that first touch. If no
//! window ever claims an entry it simply expires with the process, which is
//! the correct outcome: the pane it belonged to did not come back.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::model::{RestoreSnapshot, SessionEntry};

fn queue() -> &'static Mutex<Vec<SessionEntry>> {
    static QUEUE: OnceLock<Mutex<Vec<SessionEntry>>> = OnceLock::new();
    QUEUE.get_or_init(|| Mutex::new(Vec::new()))
}

/// `saved_at_unix` of the snapshot loaded into the queue, if one was.
fn saved_at() -> &'static Mutex<Option<u64>> {
    static SAVED_AT: OnceLock<Mutex<Option<u64>>> = OnceLock::new();
    SAVED_AT.get_or_init(|| Mutex::new(None))
}

fn loaded() -> &'static Mutex<bool> {
    static LOADED: OnceLock<Mutex<bool>> = OnceLock::new();
    LOADED.get_or_init(|| Mutex::new(false))
}

/// Load the snapshot into the queue if this is the first call of the process.
///
/// Reading is destructive (see [`RestoreSnapshot::take_blocking`]) and only
/// validated entries make it in, so by the time anything reaches the queue the
/// session ids are shell-safe, the directories exist and the transcripts are
/// still on disk.
fn ensure_loaded() {
    let mut loaded = match loaded().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if *loaded {
        return;
    }
    *loaded = true;

    let path = match crate::model::snapshot_path() {
        Ok(path) => path,
        Err(e) => {
            tracing::warn!(target: "mnemos_ide_agent_restore", "no snapshot path: {e}");
            return;
        }
    };

    let Some(snapshot) = RestoreSnapshot::take_blocking(&path) else {
        return;
    };

    let entries: Vec<SessionEntry> = snapshot.resumable().into_iter().cloned().collect();
    if entries.is_empty() {
        return;
    }
    match saved_at().lock() {
        Ok(mut at) => *at = Some(snapshot.saved_at_unix),
        Err(poisoned) => *poisoned.into_inner() = Some(snapshot.saved_at_unix),
    }
    tracing::info!(
        target: "mnemos_ide_agent_restore",
        "{} agent session(s) available to restore",
        entries.len()
    );
    match queue().lock() {
        Ok(mut q) => *q = entries,
        Err(poisoned) => *poisoned.into_inner() = entries,
    }
}

/// Remove and return the queued entries whose pane came back in this window.
///
/// `pane_uuids_hex` is what the caller actually has on screen; anything not in
/// it stays queued for another window. Returns an empty vec when there is
/// nothing to restore, which is the overwhelmingly common case.
pub fn take_matching(pane_uuids_hex: &[String]) -> Vec<SessionEntry> {
    ensure_loaded();

    let wanted: HashSet<&str> = pane_uuids_hex.iter().map(String::as_str).collect();
    let mut queue = match queue().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };

    let mut taken = Vec::new();
    queue.retain(|entry| {
        if wanted.contains(entry.pane_uuid_hex.as_str()) {
            taken.push(entry.clone());
            false
        } else {
            true
        }
    });
    taken
}

/// When the loaded snapshot was written, as a time. `None` before the first
/// [`take_matching`] or when nothing was loaded. Agent restore compares it
/// with when the other machine's turns arrived ([`crate::guard`]).
pub fn snapshot_saved_at() -> Option<SystemTime> {
    let at = match saved_at().lock() {
        Ok(guard) => *guard,
        Err(poisoned) => *poisoned.into_inner(),
    };
    at.map(|secs| UNIX_EPOCH + Duration::from_secs(secs))
}

/// When this machine last had `entry`'s session open: the entry's own
/// time for one carried over ([`defer`]), else the loaded snapshot's write
/// time. `None` when neither is known.
pub fn last_live_at(entry: &SessionEntry) -> Option<SystemTime> {
    entry
        .last_live_unix
        .map(|secs| UNIX_EPOCH + Duration::from_secs(secs))
        .or_else(snapshot_saved_at)
}

fn deferred_entries() -> &'static Mutex<Vec<SessionEntry>> {
    static DEFERRED: OnceLock<Mutex<Vec<SessionEntry>>> = OnceLock::new();
    DEFERRED.get_or_init(|| Mutex::new(Vec::new()))
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Keep an entry that was taken but not run (its pane was busy: the user
/// had typed or run something there), so the next snapshot carries it again
/// ([`with_deferred`]). Its [`SessionEntry::last_live_unix`] is set to
/// when the session was last open here, if it was not set already, so the
/// next restore's guard does not mistake the time it sat unrun for a run.
pub fn defer(mut entry: SessionEntry) {
    if entry.last_live_unix.is_none() {
        entry.last_live_unix = last_live_at(&entry)
            .and_then(|at| at.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
    }
    let mut deferred = lock(deferred_entries());
    deferred.retain(|e| e.session_id != entry.session_id);
    deferred.push(entry);
}

/// Forget a deferred entry: the session was resumed some other way (a click
/// in the sessions list), so the next launch must not resume it again.
pub fn forget_deferred(session_id: &str) {
    lock(deferred_entries()).retain(|e| e.session_id != session_id);
}

/// `live` (the sessions running in panes now) followed by the deferred
/// entries whose session and pane are both not among them: a pane that now
/// runs another session keeps only that one, and a session that runs again
/// keeps its new pane.
pub fn with_deferred(live: Vec<SessionEntry>) -> Vec<SessionEntry> {
    let deferred = lock(deferred_entries()).clone();
    let mut out = live;
    for entry in deferred {
        if !out
            .iter()
            .any(|e| e.session_id == entry.session_id || e.pane_uuid_hex == entry.pane_uuid_hex)
        {
            out.push(entry);
        }
    }
    out
}

/// Lowercase hex of a pane uuid — the form stored in the snapshot.
pub fn pane_uuid_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
#[path = "queue_tests.rs"]
mod queue_tests;

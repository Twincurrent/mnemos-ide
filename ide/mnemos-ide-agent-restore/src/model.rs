// SPDX-License-Identifier: AGPL-3.0-only
//! The snapshot file: its shape, where it lives, and the validation that
//! stands between a stale file and a command line.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use mnemos_ide_core::paths::ide_agent_sessions_file;
use mnemos_ide_sessions::model::read_session_head;
use mnemos_ide_sessions::{claude_projects_dir, is_safe_session_id, project_slug};

/// Bumped whenever [`SessionEntry`] changes incompatibly. A snapshot written
/// by a different version is discarded rather than migrated — it describes a
/// process that is already gone, so there is nothing worth salvaging.
pub const SNAPSHOT_VERSION: u32 = 1;

/// Hard cap on panes we will ever drive a command into. Well above any real
/// workspace; it exists so a corrupted file cannot turn into a fork bomb.
pub const MAX_RESTORED_SESSIONS: usize = 12;

/// Snapshots older than this are ignored. A relaunch takes seconds and an
/// overnight quit-and-reopen is still a legitimate resume, but a snapshot from
/// last week describes a workspace the user has long since moved on from.
pub const MAX_SNAPSHOT_AGE_SECS: u64 = 24 * 60 * 60;

#[derive(Debug, thiserror::Error)]
pub enum RestoreError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Paths(#[from] mnemos_ide_core::IdeError),
}

/// One pane that had a resumable agent session when the app went down.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEntry {
    /// Lowercase hex of `TerminalPane::uuid` — the pane's identity across
    /// runs. Hex rather than the raw bytes so the file stays readable and
    /// diffable by hand.
    pub pane_uuid_hex: String,
    /// Claude Code session id, the argument to `claude --resume`.
    pub session_id: String,
    /// The last cwd the agent's hooks reported. It follows every `cd` the
    /// agent's Bash tool makes, so it is usually not where the session was
    /// launched and can name a worktree that has since been deleted. Used
    /// only as a hint for finding the transcript; the pane brings its own
    /// restored cwd.
    pub cwd: Option<PathBuf>,
    /// Agent name as reported by `CLIAgent::to_serialized_name` (e.g.
    /// `"Claude"`). Recorded so a future agent with its own resume syntax can
    /// be told apart from Claude's; today only `"Claude"` is ever written.
    pub agent: String,
    /// Unix seconds when this machine last had the session open, for an
    /// entry carried over from an earlier restore that never ran (see
    /// `queue::defer`). `None`: it was open here when the snapshot was
    /// written. The takeover guard counts turns pulled after this time as
    /// the other machine's (`crate::guard::resume_mode`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_live_unix: Option<u64>,
    /// The session ran with permission checks bypassed (it was started with
    /// `--dangerously-skip-permissions`), so it is resumed with that flag
    /// again instead of in the user's default permission mode. Only ever
    /// set from the pane's own token-checked hook events, and this file
    /// never syncs.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skip_permissions: bool,
}

/// Everything worth replaying from the previous run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreSnapshot {
    pub version: u32,
    /// Unix seconds at write time. Drives the [`MAX_SNAPSHOT_AGE_SECS`] check.
    pub saved_at_unix: u64,
    pub entries: Vec<SessionEntry>,
}

/// Where the snapshot lives: alongside `projects.json` and
/// `workspace_state.json` in the Mnemos store directory.
///
/// Deliberately **not** a new top-level location and deliberately **not**
/// under `~/.claude` — `mnemos-ide-sync`'s directory sweep owns everything it is
/// pointed at, and this file is per-machine state that must never sync. It
/// sits next to `workspace_state.json`, which is local-only for the same
/// reason: which panes were open is a property of *this* machine.
pub fn snapshot_path() -> Result<PathBuf, RestoreError> {
    Ok(ide_agent_sessions_file()?)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl RestoreSnapshot {
    /// Build a snapshot from raw capture, applying the two rules that must
    /// hold before anything is written: entries with an implausible session id
    /// are dropped, and each session id appears at most once.
    ///
    /// Dedupe is not defensive paranoia. Claude Code files a subagent's
    /// transcript under the parent's session uuid, so a subagent's hook events
    /// carry the *parent's* `session_id`. Two panes can therefore legitimately
    /// report the same id, and resuming it in both would fork one conversation
    /// into two divergent ones. First pane seen wins; iteration order at the
    /// call site is the pane order, so that is the leftmost pane.
    pub fn from_entries(entries: impl IntoIterator<Item = SessionEntry>) -> Self {
        let mut seen: Vec<String> = Vec::new();
        let mut kept = Vec::new();
        for entry in entries {
            if !is_safe_session_id(&entry.session_id) {
                tracing::warn!(
                    target: "mnemos_ide_agent_restore",
                    "dropping implausible session id {:?}",
                    entry.session_id
                );
                continue;
            }
            if seen.iter().any(|id| id == &entry.session_id) {
                continue;
            }
            seen.push(entry.session_id.clone());
            kept.push(entry);
            if kept.len() >= MAX_RESTORED_SESSIONS {
                break;
            }
        }

        Self {
            version: SNAPSHOT_VERSION,
            saved_at_unix: now_unix(),
            entries: kept,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Write atomically to `path` (tmp sibling + rename), mirroring
    /// `ProjectStore::save_blocking`.
    ///
    /// An empty snapshot **removes** the file rather than writing `[]`. That
    /// keeps "no agents were running" and "nothing to restore" the same state
    /// on disk, so a clean quit leaves nothing behind to go stale.
    pub fn save_blocking(&self, path: &Path) -> Result<(), RestoreError> {
        if self.entries.is_empty() {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            return Ok(());
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self)?;
        let tmp = tmp_sibling(path);
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Read the snapshot **and delete the file**, whatever the outcome.
    ///
    /// The delete is the whole safety story: restore is a one-shot: if
    /// replaying it wedges or crashes the app, the next launch finds nothing
    /// and comes up clean. Never change this to a non-destructive read.
    ///
    /// Returns `None` for a missing file, unparseable JSON, a version we don't
    /// recognise, or a snapshot past [`MAX_SNAPSHOT_AGE_SECS`] — all of which
    /// are ordinary outcomes, not errors worth surfacing to the user.
    pub fn take_blocking(path: &Path) -> Option<Self> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::debug!(target: "mnemos_ide_agent_restore", "cannot read snapshot: {e}");
                }
                return None;
            }
        };

        // Delete before parsing, not after: a snapshot that panics the parser
        // must not be left on disk to do it again.
        if let Err(e) = std::fs::remove_file(path) {
            tracing::warn!(target: "mnemos_ide_agent_restore", "could not clear snapshot: {e}");
        }

        let snapshot: Self = match serde_json::from_slice(&bytes) {
            Ok(snapshot) => snapshot,
            Err(e) => {
                tracing::warn!(target: "mnemos_ide_agent_restore", "discarding unparseable snapshot: {e}");
                return None;
            }
        };

        if snapshot.version != SNAPSHOT_VERSION {
            tracing::info!(
                target: "mnemos_ide_agent_restore",
                "discarding snapshot from version {} (want {SNAPSHOT_VERSION})",
                snapshot.version
            );
            return None;
        }

        let age = now_unix().saturating_sub(snapshot.saved_at_unix);
        if age > MAX_SNAPSHOT_AGE_SECS {
            tracing::info!(
                target: "mnemos_ide_agent_restore",
                "discarding snapshot {age}s old (cap {MAX_SNAPSHOT_AGE_SECS}s)"
            );
            return None;
        }

        Some(snapshot)
    }

    /// The entries that are still safe to replay, capped at
    /// [`MAX_RESTORED_SESSIONS`].
    ///
    /// Three things have to still be true, and all three routinely stop being
    /// true between one run and the next:
    ///
    /// 1. The id must be shaped like a session uuid. It reaches a command line
    ///    and a path join, so this is checked before anything else.
    /// 2. Claude Code must still have a transcript for it somewhere under
    ///    `~/.claude/projects/` (`/clear`, manual cleanup and a different
    ///    machine all remove it). Resuming a session whose transcript is gone
    ///    drops the user into a `claude` error instead of a conversation.
    /// 3. The directory the session was *launched* in, read from the head of
    ///    that transcript, must still be a directory (worktrees get deleted,
    ///    volumes get unmounted). When the head records no cwd the launch dir
    ///    is unknown and the entry is kept.
    ///
    /// The entry's own `cwd` decides neither: it is the last cwd the hooks
    /// reported and drifts with every `cd`, while Claude files the transcript
    /// under the launch dir's slug. It is only a hint for where to look.
    ///
    /// When the projects dir cannot be resolved at all (no home dir), nothing
    /// can be checked and every well-formed entry is kept: `claude` gives a
    /// clearer error than an agent that silently fails to come back.
    pub fn resumable(&self) -> Vec<&SessionEntry> {
        let projects = claude_projects_dir().ok();
        self.entries
            .iter()
            .filter(|entry| {
                if !is_safe_session_id(&entry.session_id) {
                    return false;
                }
                let Some(projects) = &projects else {
                    return true;
                };
                let Some(transcript) =
                    find_transcript(projects, &entry.session_id, entry.cwd.as_deref())
                else {
                    tracing::info!(
                        target: "mnemos_ide_agent_restore",
                        "skipping session {}: transcript is gone",
                        entry.session_id
                    );
                    return false;
                };
                if let Some(launch_dir) = launch_dir(&transcript) {
                    if !launch_dir.is_dir() {
                        tracing::info!(
                            target: "mnemos_ide_agent_restore",
                            "skipping session {}: {} is no longer a directory",
                            entry.session_id,
                            launch_dir.display()
                        );
                        return false;
                    }
                }
                true
            })
            .take(MAX_RESTORED_SESSIONS)
            .collect()
    }
}

/// Where Claude Code keeps the transcript for `session_id`, if anywhere.
///
/// Claude files a transcript under `projects/<slug of the launch dir>/`, and
/// `hint` (an entry's last hook cwd) is usually somewhere else, so its slug is
/// only the first place tried. Every top-level `<slug>/<session_id>.jsonl` is
/// a candidate. A session resumed from another folder can have a copy in
/// several slug dirs, and the newest is the one Claude Code writes (the same
/// rule `mnemos-ide-sync` uses), so the newest mtime wins. A tie goes to the hint's
/// slug, then to the larger path, so the answer never depends on `read_dir`
/// order.
///
/// Only direct children of each slug dir are looked at: the
/// `<slug>/<session_id>/subagents/*.jsonl` files never match.
///
/// `session_id` must already have passed [`is_safe_session_id`], because it
/// is joined into a path.
fn find_transcript(projects: &Path, session_id: &str, hint: Option<&Path>) -> Option<PathBuf> {
    let file_name = format!("{session_id}.jsonl");
    let hint_slug = hint.map(project_slug);

    let mut best: Option<(SystemTime, bool, PathBuf)> = None;
    let mut consider = |path: PathBuf, is_hint: bool| {
        let Ok(meta) = std::fs::metadata(&path) else {
            return;
        };
        if !meta.is_file() {
            return;
        }
        let candidate = (meta.modified().unwrap_or(UNIX_EPOCH), is_hint, path);
        if best.as_ref().is_none_or(|current| candidate > *current) {
            best = Some(candidate);
        }
    };

    if let Some(slug) = &hint_slug {
        consider(projects.join(slug).join(&file_name), true);
    }
    if let Ok(slugs) = std::fs::read_dir(projects) {
        for slug in slugs.flatten() {
            if !slug.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            if hint_slug
                .as_deref()
                .is_some_and(|hint| slug.file_name() == hint)
            {
                continue;
            }
            consider(slug.path().join(&file_name), false);
        }
    }

    best.map(|(_, _, path)| path)
}

/// The directory the session was launched in: the first `cwd` recorded at the
/// head of its transcript. Transcripts run to hundreds of MB, so only the
/// bounded head `mnemos-ide-sessions` reads for its picker is looked at.
///
/// `None` when the head is unreadable or records no cwd. That means the launch
/// dir is unknown, not that it is gone.
fn launch_dir(transcript: &Path) -> Option<PathBuf> {
    // The mtime and size only feed `SessionMeta` fields that are not used here.
    read_session_head(transcript, UNIX_EPOCH, 0)
        .ok()
        .and_then(|meta| meta.cwd)
}

/// The command that resumes `session_id` in place.
///
/// Deliberately **not** `--fork-session`: the point of restoring after an
/// update is to continue the same thread, not to branch it, unless another
/// machine may be running it ([`crate::guard`], then [`fork_command`]).
/// With `skip_permissions` it adds `--dangerously-skip-permissions`, for a
/// session that was started with it ([`SessionEntry::skip_permissions`]).
/// Callers must have checked [`is_safe_session_id`]; [`RestoreSnapshot::resumable`] does.
pub fn resume_command(session_id: &str, skip_permissions: bool) -> String {
    if skip_permissions {
        format!("claude --resume {session_id} --dangerously-skip-permissions")
    } else {
        format!("claude --resume {session_id}")
    }
}

/// The command that resumes a fork of `session_id`: a copy with a new id and
/// its own Remote Control session, which takes nothing over from a process
/// on another machine. Same precondition as [`resume_command`].
pub fn fork_command(session_id: &str, skip_permissions: bool) -> String {
    format!("{} --fork-session", resume_command(session_id, skip_permissions))
}

/// The length and mtime of the transcript [`RestoreSnapshot::resumable`]
/// would find for `entry`, for [`crate::guard::NewestTurn::from_file`].
/// `None` when there is none (or no home dir). Blocking: a stat per slug dir.
pub fn transcript_stat(entry: &SessionEntry) -> Option<(u64, SystemTime)> {
    if !is_safe_session_id(&entry.session_id) {
        return None;
    }
    let projects = claude_projects_dir().ok()?;
    let path = find_transcript(&projects, &entry.session_id, entry.cwd.as_deref())?;
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().unwrap_or(UNIX_EPOCH)))
}

fn tmp_sibling(path: &Path) -> PathBuf {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    PathBuf::from(tmp)
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod model_tests;

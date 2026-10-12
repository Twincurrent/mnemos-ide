// SPDX-License-Identifier: AGPL-3.0-only
//! Claude Code transcript sync: `~/.claude/projects/<slug>/<session>.jsonl`
//! follows the user between machines through the admin blob store.
//!
//! Why numbered segments instead of one blob per session: a transcript only
//! grows, and a session can be continued on the PC and then back on the Mac.
//! So each machine uploads only the lines *it* wrote, as append-only gzip
//! segments that are never overwritten. A machine that lost its local state
//! picks the next free sequence number from the listing and re-uploads; the
//! reader drops lines it already has (by `uuid`), so the worst case is some
//! wasted storage and never lost lines. The one way to double lines: a
//! re-upload segment containing only uuid-less marker lines has nothing to
//! anchor the dedupe on. At 4 MiB segments no real transcript produced one.
//!
//! Remote layout (profile-scoped, see [`SyncClient::put_blob`]):
//!
//! ```text
//! claude-sessions/{sid}/{machine}.meta.json           SessionMeta (latest wins)
//! claude-sessions/{sid}/{machine}-{seq:06}.jsonl.gz   complete lines, <= 4 MiB raw
//! ```
//!
//! Paths differ per machine (`/Users/alice/code/tcbv2` vs `C:\code\mnemos`), so
//! a session is tied to a registry project (id, gitRepo, name) plus its cwd
//! relative to that project's checkout. A session outside every project but
//! inside one of the usual code folders (`~/code`, `C:\code`, see
//! [`mnemos_ide_projects::resolve::code_roots`]) is tied to that folder's name
//! instead, and lands in the local code folder of the same name (compared
//! case-insensitively); where there is none, under this machine's home if the
//! folder was under the sender's (`originHome`), else at its unchanged cwd. A
//! session outside both but under the user's home
//! (`~/Desktop/foo`, `~/Documents/x`, home itself) is tied to home
//! (`homeRel`) and lands under this machine's home
//! (`/Users/alice` <-> `C:\Users\bob` <-> `/home/x`). One outside home too
//! (`/tmp/x`, `D:\work`) keeps its cwd unchanged (`absCwd`): the other
//! machine files it under that cwd's slug so it is listed and readable,
//! resumable only where the path exists. Precedence is project, code folder,
//! home, absolute; only the first cwd decides. A pulled line's top-level
//! `cwd` is rewritten onto this machine's checkout, code folder or home, and
//! the file lands in the slug dir Claude Code itself uses for that cwd, so
//! `claude --resume` finds it.
//!
//! Never synced: a session whose cwd is inside `~/.claude` or the Mnemos
//! data dir, in either direction, nor one whose meta names a path with a
//! `..` component. Mapping a pulled cwd only builds a path string, never
//! stats it, and a transcript a pull created is marked in the local state
//! (`pulledHere`) so the memory checkout scan in `manager.rs` passes over it.
//! So a pulled session for `~/Desktop` or `~/Documents` does not make
//! Mnemos stat that path (no macOS privacy prompt, no stall on an absent
//! network volume); the transcripts themselves are read from
//! `~/.claude/projects`. Older clients see no
//! project and no code folder in a `homeRel` or `absCwd` meta and skip it.
//! Sync runs only under a signed-in admin account; nothing here changes that.
//!
//! Local state (never synced): `<store>/machine_id` (shared with the live
//! feed, see [`crate::machine`]) and
//! `<store>/transcript_sync.json`.
//!
//! Delivery: after uploading, a machine records its newest segment number
//! per session in the shared session index, a small state key
//! ([`crate::merge::SESSION_INDEX_KEY`]; [`TranscriptSync::index_owed`] says
//! what to add). The other machine polls that key and lists only the
//! sessions it names that have segments not applied there
//! ([`TranscriptSync::pull_indexed`]), so a turn on one machine usually
//! reaches the other within a minute of the session going quiet. A full
//! listing ([`TranscriptSync::pull`]) stays as the hourly safety net.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::future::Future;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use mnemos_ide_core::registry;
use mnemos_ide_projects::{FsEnv, LocalResolver, ProjectStore, RealFs};
use tokio::sync::Mutex;

use crate::client::{BlobItem, SyncClient, MAX_BLOB_BYTES};
use crate::error::IdeError;
use crate::machine::{is_machine_id, MACHINE_ID_FILE};
use crate::manager::is_transient;

const TARGET: &str = "mnemos_ide_sync::transcripts";

const REMOTE_PREFIX: &str = "claude-sessions/";
const SEGMENT_SUFFIX: &str = ".jsonl.gz";
const META_SUFFIX: &str = ".meta.json";
/// Raw (uncompressed) bytes per segment. Small enough that one upload stays
/// well inside what the proxy in front of the admin backend passes through.
pub(crate) const MAX_SEGMENT_RAW_BYTES: usize = 4 * 1024 * 1024;
/// Sessions untouched for longer than this are not pushed.
const MAX_SESSION_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// A transcript written this recently is in use here; merging into it could
/// interleave with Claude Code's own appends.
const LIVE_WINDOW: Duration = Duration::from_secs(60);
/// Refuse to inflate a segment past this: it can only be corrupt or hostile.
const MAX_SEGMENT_DECODED_BYTES: u64 = 1024 * 1024 * 1024;
const STATE_FILE: &str = "transcript_sync.json";
/// How many of the last pushed lines are remembered to find the push
/// position again after Claude Code rewrites a transcript's tail.
const TAIL_KEYS: usize = 8;
/// A cheap sweep ([`TranscriptSync::sweep_changed`]) or an index pull that
/// has more sessions to look at than this lists the whole store once
/// instead of each session's own prefix.
const PER_SESSION_LISTING_MAX: usize = 16;
/// Strings longer than this are cut from a stub line (see [`stub_line`]).
const STUB_MAX_STRING: usize = 64 * 1024;

/// One pass at a time in the whole process, not just per `TranscriptSync`.
/// Signing out and back in builds a new instance while the old worker may
/// still be mid-pass; two passes would both take `max seq + 1` from the
/// listing and overwrite each other's segment.
static PASS_LOCK: Mutex<()> = Mutex::const_new(());

// ─────────────────────────────────────────────────────────────────────────────
// Blob backend
// ─────────────────────────────────────────────────────────────────────────────

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The blob calls the sync needs. [`SyncClient`] is the real one; tests
/// use an in-memory store so two "machines" can share it without a network.
pub(crate) trait BlobStore: Send + Sync {
    fn put<'a>(&'a self, path: &'a str, bytes: Vec<u8>) -> BoxFuture<'a, Result<(), IdeError>>;
    fn get<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Option<Vec<u8>>, IdeError>>;
    fn list<'a>(&'a self, prefix: &'a str) -> BoxFuture<'a, Result<Vec<BlobItem>, IdeError>>;
    /// Only for this machine's own uploads of a session whose project sync
    /// was turned off here ([`TranscriptSync::purge_opted_out`]).
    fn delete<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), IdeError>>;
}

impl BlobStore for SyncClient {
    fn put<'a>(&'a self, path: &'a str, bytes: Vec<u8>) -> BoxFuture<'a, Result<(), IdeError>> {
        Box::pin(self.put_blob(path, bytes))
    }
    fn get<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Option<Vec<u8>>, IdeError>> {
        Box::pin(self.get_blob(path))
    }
    fn list<'a>(&'a self, prefix: &'a str) -> BoxFuture<'a, Result<Vec<BlobItem>, IdeError>> {
        Box::pin(self.list_blobs(prefix))
    }
    fn delete<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), IdeError>> {
        Box::pin(self.delete_blob(path))
    }
}

/// Everything that differs between the app and a test acting out a machine.
pub(crate) struct Options {
    /// Filesystem view for project checkouts. Tests pass a fake so one host
    /// can play both a Mac and a PC; transcript files themselves are always
    /// real files under `claude_projects_dir`.
    pub fs: Arc<dyn FsEnv + Send + Sync>,
    /// `projects.json` + `workspace_state.json`; `None` = Mnemos's defaults.
    pub registry_files: Option<(PathBuf, PathBuf)>,
    pub max_segment_bytes: usize,
}

impl Options {
    fn real() -> Self {
        Self {
            fs: Arc::new(RealFs),
            registry_files: None,
            max_segment_bytes: MAX_SEGMENT_RAW_BYTES,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Public API
// ─────────────────────────────────────────────────────────────────────────────

/// Pushes this machine's Claude Code transcripts to the blob store and pulls
/// other machines' sessions into place. Every call runs under one internal
/// lock, so the watcher's pushes and the periodic pull never interleave.
pub struct TranscriptSync {
    blobs: Arc<dyn BlobStore>,
    store_dir: PathBuf,
    claude_projects_dir: PathBuf,
    opts: Options,
    inner: Mutex<Inner>,
    /// The last pull (indexed or full) left a session waiting (open or
    /// being written here) or failed for a reason that may pass (offline, a
    /// 5xx), so the next index poll should pull again even when the index
    /// has not changed. Outside `inner` so the poll can read it without
    /// waiting for a pass.
    index_retry: AtomicBool,
}

#[derive(Default)]
struct Inner {
    /// Kept across calls so the checkout discovery scan is cached (it
    /// expires on its own, see `mnemos_ide_projects::resolve::SCAN_TTL`).
    resolver: LocalResolver,
    machine_id: Option<String>,
    metas: MetaCache,
    tried: Tried,
    skipped: SkipMarks,
}

/// Other machines' session metas by `(session, machine)`, with the listing's
/// `lastModified` they were read at. A meta only changes when its machine
/// pushes again, and every pull pass looks at every session, including ones
/// this machine cannot apply (no checkout here), so without this each pass
/// would download all of those metas again.
type MetaCache = HashMap<(String, String), (String, SessionMeta)>;

/// `(session, machine)` -> the highest of that machine's segment numbers a
/// pull already dealt with: applied, found it cannot apply here (no
/// checkout of the project, no meta, untouched for a month), or failed to
/// apply for a reason that would only repeat (a segment that does not
/// unpack, a merge the disk refuses). An index poll
/// does not list such a session again until the machine uploads past that
/// number. In memory only; the hourly full pull looks at everything
/// regardless.
type Tried = HashMap<(String, String), u64>;

/// Transcripts a push skipped (no `cwd` yet, a cwd inside `~/.claude` or
/// the Mnemos data dir, or only a partial last line left), by path. A cheap
/// sweep passes over them until the file changes. None of these reasons
/// depends on the registry: a session outside every checkout and code
/// folder still syncs (see [`Anchor`]). In memory only, so a restart (an
/// upgrade included) looks at every session again.
type SkipMarks = HashMap<PathBuf, SkipMark>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SkipMark {
    len: u64,
    mtime: SystemTime,
}

impl TranscriptSync {
    pub fn new(client: SyncClient, store_dir: PathBuf, claude_projects_dir: PathBuf) -> Self {
        Self::with_parts(Arc::new(client), store_dir, claude_projects_dir, Options::real())
    }

    pub(crate) fn with_parts(
        blobs: Arc<dyn BlobStore>,
        store_dir: PathBuf,
        claude_projects_dir: PathBuf,
        opts: Options,
    ) -> Self {
        Self {
            blobs,
            store_dir,
            claude_projects_dir,
            opts,
            inner: Mutex::new(Inner::default()),
            index_retry: AtomicBool::new(false),
        }
    }

    /// Resolve checkouts against these registry files instead of Mnemos's
    /// defaults. The sync manager passes the same files its `projects` apply
    /// writes, so a manager pointed at a test store never touches the real
    /// `projects.json` (or scans the real code folders) through this path.
    pub(crate) fn with_registry_files(
        mut self,
        projects_path: PathBuf,
        workspace_state_path: PathBuf,
    ) -> Self {
        self.opts.registry_files = Some((projects_path, workspace_state_path));
        self
    }

    /// `<claude_projects_dir>/<slug>/<uuid>.jsonl` and nothing deeper:
    /// `subagents/` and `tool-results/` under a session dir are not synced.
    pub fn is_transcript_path(&self, path: &Path) -> bool {
        is_transcript_path_in(&self.claude_projects_dir, path)
    }

    /// Upload the new complete lines of each changed transcript in `paths`
    /// (anything that is not a transcript is ignored). Every session is
    /// attempted; the first failure is returned after the rest ran.
    pub async fn push_changed(&self, paths: Vec<PathBuf>) -> Result<(), IdeError> {
        let mut files: Vec<PathBuf> = Vec::new();
        for p in paths {
            if self.is_transcript_path(&p) && !files.contains(&p) {
                files.push(p);
            }
        }
        if files.is_empty() {
            return Ok(());
        }
        let _pass_lock = PASS_LOCK.lock().await;
        let mut inner = self.inner.lock().await;
        let mut pass = self.begin_pass(&mut inner).await?;
        let files = newest_copy_per_session(files, &pass.state);
        let mut first_err = None;
        for file in &files {
            if let Err(e) = self.push_file(&mut pass, file).await {
                tracing::warn!(target: TARGET, path = %file.display(), error = %e, "transcript push failed");
                first_err.get_or_insert(e);
            }
        }
        pass.log_push("push");
        self.end_pass(&mut inner, pass);
        first_err.map_or(Ok(()), Err)
    }

    /// Push every eligible session (modified within 30 days). Run at startup
    /// to catch up on whatever changed while the app was closed. Lists the
    /// whole store once, which also re-uploads a meta the server lost.
    pub async fn sweep_push(&self) -> Result<(), IdeError> {
        let _pass_lock = PASS_LOCK.lock().await;
        let mut inner = self.inner.lock().await;
        let mut pass = self.begin_pass(&mut inner).await?;
        let result = self.sweep_pass(&mut pass, true).await;
        pass.log_push("sweep");
        self.end_pass(&mut inner, pass);
        result
    }

    /// [`Self::sweep_push`] for the periodic cycles: only sessions that
    /// changed since their last push (by length), are not being written
    /// right now (the watcher's push has those), and were not skipped
    /// before without changing since. Catches lines the watcher or a failed
    /// upload left behind. With nothing to push it reads no file and makes
    /// no request.
    pub async fn sweep_changed(&self) -> Result<(), IdeError> {
        let _pass_lock = PASS_LOCK.lock().await;
        let mut inner = self.inner.lock().await;
        let mut pass = self.begin_pass(&mut inner).await?;
        let result = self.sweep_pass(&mut pass, false).await;
        pass.log_push("sweep");
        self.end_pass(&mut inner, pass);
        result
    }

    /// Bring other machines' new segments into the local transcripts. Lists
    /// the whole store: the safety net behind [`Self::pull_indexed`].
    pub async fn pull(&self) -> Result<(), IdeError> {
        let _pass_lock = PASS_LOCK.lock().await;
        let mut inner = self.inner.lock().await;
        let mut pass = self.begin_pass(&mut inner).await?;
        let result = self.pull_pass(&mut pass).await;
        // A session left waiting (or a listing that failed for now) is the
        // index poll's to come back for; this pass covered everything else.
        self.index_retry
            .store(retry_soon(&pass, &result), Ordering::Relaxed);
        pass.log_push("pull");
        pass.log_pull();
        self.end_pass(&mut inner, pass);
        result
    }

    /// Pull only the sessions `index` (the shared session index, see
    /// [`crate::merge::SESSION_INDEX_KEY`]) shows another machine has
    /// uploaded segments of that this machine has not applied. Each is
    /// listed by its own prefix, `claude-sessions/{sid}/`; the store as a
    /// whole never is.
    pub async fn pull_indexed(&self, index: &Value) -> Result<(), IdeError> {
        let _pass_lock = PASS_LOCK.lock().await;
        let mut inner = self.inner.lock().await;
        let mut pass = self.begin_pass(&mut inner).await?;
        let result = self.pull_indexed_pass(&mut pass, index).await;
        self.index_retry
            .store(retry_soon(&pass, &result), Ordering::Relaxed);
        pass.log_push("pull");
        pass.log_pull();
        self.end_pass(&mut inner, pass);
        result
    }

    /// Whether the last pull ([`Self::pull_indexed`] or [`Self::pull`]) left
    /// something to try again (a session open or being written here, or a
    /// failure that may pass), even if the index does not change.
    pub fn wants_index_retry(&self) -> bool {
        self.index_retry.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn set_index_retry(&self, retry: bool) {
        self.index_retry.store(retry, Ordering::Relaxed);
    }

    /// This machine's entries the shared session index does not hold yet,
    /// as an index value: one per session whose newest uploaded segment is
    /// past what the index was last known to hold, stamped now. `None` when
    /// nothing is owed. The debt is kept in the local state, so an index
    /// update that fails (or never ran, the app quit) is simply owed again
    /// after the next push or sweep.
    pub async fn index_owed(&self) -> Result<Option<Value>, IdeError> {
        let _pass_lock = PASS_LOCK.lock().await;
        let mut inner = self.inner.lock().await;
        let pass = self.begin_pass(&mut inner).await?;
        let at = registry::format_timestamp(Utc::now());
        let mut owed = serde_json::Map::new();
        for (sid, st) in &pass.state.sessions {
            if st.own_seq > st.indexed_seq {
                let mut entry = serde_json::Map::new();
                entry.insert("seq".to_string(), Value::from(st.own_seq));
                entry.insert("at".to_string(), Value::String(at.clone()));
                let mut machines = serde_json::Map::new();
                machines.insert(pass.machine.clone(), Value::Object(entry));
                owed.insert(sid.clone(), Value::Object(machines));
            }
        }
        self.end_pass(&mut inner, pass);
        Ok((!owed.is_empty()).then_some(Value::Object(owed)))
    }

    /// The shared index now holds `entries` (a value from
    /// [`Self::index_owed`]): they are no longer owed.
    pub async fn index_written(&self, entries: &Value) -> Result<(), IdeError> {
        let _pass_lock = PASS_LOCK.lock().await;
        let mut inner = self.inner.lock().await;
        let mut pass = self.begin_pass(&mut inner).await?;
        let mut changed = false;
        for (sid, machines) in entries.as_object().into_iter().flatten() {
            let seq = machines
                .get(&pass.machine)
                .and_then(|e| e.get("seq"))
                .and_then(Value::as_u64);
            let (Some(seq), Some(st)) = (seq, pass.state.sessions.get_mut(sid)) else {
                continue;
            };
            if seq > st.indexed_seq {
                st.indexed_seq = seq;
                changed = true;
            }
        }
        let result = if changed {
            self.save_state(&pass.state).await
        } else {
            Ok(())
        };
        self.end_pass(&mut inner, pass);
        result
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Local state and remote naming
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncState {
    #[serde(default)]
    sessions: BTreeMap<String, SessionState>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionState {
    /// Complete lines of the local file that are on the server (pushed from
    /// here, or pulled in from another machine).
    #[serde(default)]
    pushed_lines: usize,
    /// File length at the end of line `pushed_lines`, so a sweep can skip a
    /// file that has not grown without reading it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pushed_bytes: Option<u64>,
    /// Other machines' segment file names already merged into the local file.
    #[serde(default)]
    applied_segments: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    local_path: Option<String>,
    /// [`line_key`]s of the last lines counted in `pushed_lines`, oldest
    /// first. Claude Code is not append-only: retracting a streamed message
    /// truncates the file at that line and rewrites what followed, so the
    /// count alone can point past lines that never went up. These keys find
    /// the real position again (see [`resume_line`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tail: Vec<String>,
    /// Highest segment number this machine has uploaded for the session
    /// (0: none since the state file was started).
    #[serde(default, skip_serializing_if = "is_zero")]
    own_seq: u64,
    /// Highest of this machine's segment numbers the shared session index
    /// is known to hold. Below `own_seq`, an index update is owed.
    #[serde(default, skip_serializing_if = "is_zero")]
    indexed_seq: u64,
    /// The local file was created by a pull, so its cwd is another
    /// machine's, mapped here as a string and possibly never visited on this
    /// machine. [`pulled_session_ids`] lets the memory checkout scan pass
    /// over it rather than stat that path.
    #[serde(default, skip_serializing_if = "is_false")]
    pulled_here: bool,
    /// Platform (`darwin`, `win32`, `linux`) of the machine whose segment
    /// was the newest in the last pull that appended lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pulled_from: Option<String>,
    /// The local file's length right after that pull. While the file still
    /// has exactly this length, the latest turns came from `pulled_from` and
    /// nothing ran here since ([`pulled_origins`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pulled_bytes: Option<u64>,
    /// When that pull wrote the file (Unix ms, this machine's clock). Not
    /// the file's mtime: a merge dates the file by its newest line, by the
    /// clock of the machine that wrote it. `None` in a state file written
    /// before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pulled_at_ms: Option<u64>,
}

/// Where a session's latest pulled turns came from (see [`pulled_origins`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PulledOrigin {
    /// `darwin`, `win32` or `linux`.
    pub platform: String,
    /// Length of the local transcript right after that pull. A file that has
    /// grown since has had turns here after it.
    pub bytes: u64,
    /// When that pull wrote the file, by this machine's clock. `None` for a
    /// pull recorded by a build that did not keep the time.
    pub at: Option<SystemTime>,
}

/// For every session a pull has appended lines to: the platform of the
/// machine its newest pulled turns came from, the file length the pull left
/// and when it ran. The sessions list compares that length with the file's
/// current one to say "from Windows" only while nothing has run here since,
/// and the takeover guards date the pull by `at`. Read from the
/// transcript sync state in `store_dir`; empty when there is none. Local
/// state only, nothing on the wire. Blocking.
pub fn pulled_origins(store_dir: &Path) -> HashMap<String, PulledOrigin> {
    std::fs::read(store_dir.join(STATE_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<SyncState>(&b).ok())
        .map(|st| {
            st.sessions
                .into_iter()
                .filter_map(|(sid, s)| {
                    let origin = PulledOrigin {
                        platform: s.pulled_from?,
                        bytes: s.pulled_bytes?,
                        at: s
                            .pulled_at_ms
                            .map(|ms| UNIX_EPOCH + Duration::from_millis(ms)),
                    };
                    Some((sid, origin))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// [`pulled_origins`], read again only when the state file's length or
/// mtime changed since the last read of the same file: cheap enough for the
/// UI thread (one stat), which asks on every click in the sessions list.
/// Every pull that records an origin rewrites the file, so a change is seen
/// on the next call.
pub fn pulled_origins_cached(store_dir: &Path) -> Arc<HashMap<String, PulledOrigin>> {
    type Cached = (PathBuf, Option<(u64, SystemTime)>, Arc<HashMap<String, PulledOrigin>>);
    static CACHE: std::sync::Mutex<Option<Cached>> = std::sync::Mutex::new(None);
    let path = store_dir.join(STATE_FILE);
    let stamp = std::fs::metadata(&path)
        .ok()
        .map(|m| (m.len(), m.modified().unwrap_or(UNIX_EPOCH)));
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((p, s, origins)) = cache.as_ref() {
        if *p == path && *s == stamp {
            return origins.clone();
        }
    }
    let origins = Arc::new(if stamp.is_some() {
        pulled_origins(store_dir)
    } else {
        HashMap::new()
    });
    *cache = Some((path, stamp, origins.clone()));
    origins
}

/// Sessions whose local file a pull created ([`SessionState::pulled_here`]),
/// read from the transcript sync state in `store_dir`. Empty when there is
/// no readable state. Blocking.
pub(crate) fn pulled_session_ids(store_dir: &Path) -> HashSet<String> {
    std::fs::read(store_dir.join(STATE_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<SyncState>(&b).ok())
        .map(|st| {
            st.sessions
                .into_iter()
                .filter(|(_, s)| s.pulled_here)
                .map(|(sid, _)| sid)
                .collect()
        })
        .unwrap_or_default()
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// `claude-sessions/{sid}/{machine}.meta.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionMeta {
    session_id: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    git_repo: Option<String>,
    #[serde(default)]
    project_name: Option<String>,
    /// Session cwd relative to the project root, `/`-separated, `""` at root.
    #[serde(default)]
    rel_cwd: String,
    /// The project root on the machine that wrote this meta.
    origin_root: String,
    #[serde(default)]
    origin_platform: String,
    #[serde(default)]
    updated_at: Option<String>,
    /// mtime of the session file on the machine that wrote this meta. A
    /// pulled copy is dated no earlier than this, so a session whose last
    /// timestamped line is weeks old (trailing marker lines carry none) is not
    /// born past Claude Code's 30-day cleanup on the other machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_mtime: Option<String>,
    /// Folder name of the code folder (`code` for `~/code` or `C:\code`)
    /// that `origin_root` is, for a session outside every project. Set only
    /// when no project field is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    code_root: Option<String>,
    /// The sending machine's home, on a code-folder meta whose folder is
    /// under it (`/Users/alice` for `/Users/alice/projects`). A machine with no
    /// code folder of that name maps the session onto its own home instead
    /// (`C:\Users\bob\projects`); without it, onto the unchanged cwd.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    origin_home: Option<String>,
    /// The session ran outside every project and code folder but under the
    /// user's home: `origin_root` is that machine's home and `rel_cwd` is
    /// relative to it. The other machine maps it onto its own home. Set only
    /// when no project field and no `code_root` is.
    #[serde(default, skip_serializing_if = "is_false")]
    home_rel: bool,
    /// The session ran outside home and outside every project and code
    /// folder (`/tmp/x`, `D:\work`): `origin_root` is its cwd, unchanged,
    /// and `rel_cwd` is empty. The other machine keeps the cwd as it is and
    /// files the session under that cwd's slug, so it is listed and readable
    /// there, though only resumable where that path exists.
    #[serde(default, skip_serializing_if = "is_false")]
    abs_cwd: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl SessionMeta {
    /// The platform whose path rules `origin_root` follows.
    fn origin_platform(&self) -> &'static str {
        registry::PLATFORMS
            .into_iter()
            .find(|p| *p == self.origin_platform)
            .or_else(|| registry::platform_of_path(&self.origin_root))
            .unwrap_or("linux")
    }

    /// Whether the paths this meta names are safe to build a local path
    /// from: `rel_cwd` is `/`-separated with no `..`, the code folder is one
    /// plain name, and `origin_root` and `origin_home` are absolute with no
    /// `..`. A meta comes from another machine (or whoever can write the
    /// blob store), and a `..` would walk out of the root it is mapped onto,
    /// into `~/.claude` for one.
    fn is_well_formed(&self) -> bool {
        let platform = self.origin_platform();
        let clean_abs = |p: &str| registry::platform_of_path(p).is_some() && !has_parent_dir(p, platform);
        let rel_ok = !self.rel_cwd.contains('\\') && !self.rel_cwd.split('/').any(|c| c == "..");
        let root_ok = if self.origin_root.is_empty() {
            !self.home_rel && !self.abs_cwd
        } else {
            clean_abs(&self.origin_root)
        };
        let home_ok = self.origin_home.as_deref().is_none_or(clean_abs);
        let code_root_ok = self.code_root.as_deref().is_none_or(|n| {
            let n = n.trim();
            !n.contains(['/', '\\']) && n != "." && n != ".."
        });
        rel_ok && root_ok && home_ok && code_root_ok
    }
}

/// What the server holds for one session, kept current as a pass uploads.
#[derive(Debug, Default, Clone)]
struct RemoteSession {
    /// machine -> lastModified of its meta
    metas: BTreeMap<String, String>,
    /// machine -> seq -> lastModified
    segments: BTreeMap<String, BTreeMap<u64, String>>,
}

enum BlobName {
    Meta { machine: String },
    Segment { machine: String, seq: u64 },
}

/// Claude Code session ids are v4 uuids. Checked strictly: the id becomes a
/// file name here and a path segment on the server, and older Claude Code
/// builds kept `agent-*.jsonl` sidechains at the top level too.
fn is_session_id(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// This machine's blobs of one session as a listing shows them: its meta
/// and every segment it uploaded.
fn own_blob_paths(remote: &RemoteSession, sid: &str, machine: &str) -> Vec<String> {
    let mut out: Vec<String> = remote
        .segments
        .get(machine)
        .into_iter()
        .flat_map(|segs| segs.keys().map(|seq| segment_path(sid, machine, *seq)))
        .collect();
    if remote.metas.contains_key(machine) {
        out.push(meta_path(sid, machine));
    }
    out
}

/// The first `cwd` in a transcript, read line by line: a session's first
/// lines carry it, so this never reads a whole (possibly large) file.
fn read_first_cwd(path: &Path) -> Option<String> {
    use std::io::BufRead as _;
    let file = std::fs::File::open(path).ok()?;
    std::io::BufReader::new(file)
        .split(b'\n')
        .take(FIRST_CWD_MAX_LINES)
        .map_while(Result::ok)
        .find_map(|line| {
            serde_json::from_slice::<CwdProbe>(&line)
                .ok()?
                .cwd
                .filter(|c| !c.trim().is_empty())
        })
}

/// How far [`read_first_cwd`] looks.
const FIRST_CWD_MAX_LINES: usize = 64;

fn session_prefix(sid: &str) -> String {
    format!("{REMOTE_PREFIX}{sid}/")
}

fn meta_path(sid: &str, machine: &str) -> String {
    format!("{REMOTE_PREFIX}{sid}/{machine}{META_SUFFIX}")
}

fn segment_name(machine: &str, seq: u64) -> String {
    format!("{machine}-{seq:06}{SEGMENT_SUFFIX}")
}

fn segment_path(sid: &str, machine: &str, seq: u64) -> String {
    format!("{REMOTE_PREFIX}{sid}/{}", segment_name(machine, seq))
}

fn parse_blob_path(path: &str) -> Option<(String, BlobName)> {
    let rest = path.strip_prefix(REMOTE_PREFIX)?;
    let (sid, file) = rest.split_once('/')?;
    if !is_session_id(sid) || file.contains('/') {
        return None;
    }
    if let Some(machine) = file.strip_suffix(META_SUFFIX) {
        return is_machine_id(machine).then(|| {
            (sid.to_string(), BlobName::Meta { machine: machine.to_string() })
        });
    }
    let (machine, seq) = parse_segment_name(file)?;
    Some((sid.to_string(), BlobName::Segment { machine: machine.to_string(), seq }))
}

/// `{machine}-{seq:06}.jsonl.gz` -> `(machine, seq)`.
fn parse_segment_name(name: &str) -> Option<(&str, u64)> {
    // Machine ids may contain '-', the sequence number never does.
    let (machine, seq) = name.strip_suffix(SEGMENT_SUFFIX)?.rsplit_once('-')?;
    if !is_machine_id(machine) || seq.is_empty() || !seq.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((machine, seq.parse().ok()?))
}

/// The highest of `machine`'s segment numbers among `applied`.
fn max_applied_seq(applied: &BTreeSet<String>, machine: &str) -> Option<u64> {
    applied
        .iter()
        .filter_map(|name| parse_segment_name(name))
        .filter(|(m, _)| *m == machine)
        .map(|(_, seq)| seq)
        .max()
}

/// Sessions `index` shows another machine has uploaded segments of that
/// this machine has neither applied nor already dealt with at that number
/// (`tried`). Entries this machine cannot name safely (not a session id,
/// not a machine id) are ignored.
fn index_targets(index: &Value, machine: &str, state: &SyncState, tried: &Tried) -> Vec<String> {
    let Some(sessions) = index.as_object() else {
        return Vec::new();
    };
    sessions
        .iter()
        .filter(|(sid, _)| is_session_id(sid))
        .filter(|(sid, machines)| {
            let applied = state.sessions.get(sid.as_str()).map(|s| &s.applied_segments);
            machines.as_object().into_iter().flatten().any(|(m, entry)| {
                if m == machine || !is_machine_id(m) {
                    return false;
                }
                let Some(seq) = entry.get("seq").and_then(Value::as_u64) else {
                    return false;
                };
                let applied = applied.and_then(|a| max_applied_seq(a, m));
                let tried = tried.get(&(sid.to_string(), m.clone()));
                applied.is_none_or(|a| a < seq) && tried.is_none_or(|t| *t < seq)
            })
        })
        .map(|(sid, _)| sid.clone())
        .collect()
}

fn index_listing(items: &[BlobItem]) -> BTreeMap<String, RemoteSession> {
    let mut out: BTreeMap<String, RemoteSession> = BTreeMap::new();
    for item in items {
        let Some((sid, name)) = parse_blob_path(&item.path) else {
            continue;
        };
        let entry = out.entry(sid).or_default();
        match name {
            BlobName::Meta { machine } => {
                entry.metas.insert(machine, item.last_modified.clone());
            }
            BlobName::Segment { machine, seq } => {
                entry
                    .segments
                    .entry(machine)
                    .or_default()
                    .insert(seq, item.last_modified.clone());
            }
        }
    }
    out
}

fn is_transcript_path_in(claude_projects_dir: &Path, path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let Some(stem) = name.strip_suffix(".jsonl") else {
        return false;
    };
    if !is_session_id(stem) {
        return false;
    }
    let Some(slug_dir) = path.parent() else {
        return false;
    };
    if slug_dir.file_name().is_none() {
        return false;
    }
    let Some(root) = slug_dir.parent() else {
        return false;
    };
    registry::same_path(
        &root.to_string_lossy(),
        &claude_projects_dir.to_string_lossy(),
        registry::current_platform(),
    )
}

fn session_id_of(path: &Path) -> Option<&str> {
    path.file_name()?.to_str()?.strip_suffix(".jsonl")
}

// ─────────────────────────────────────────────────────────────────────────────
// Registry view
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct RootedProject {
    id: String,
    name: String,
    git_repo: Option<String>,
    /// The checkout on this machine, when there is one.
    root: Option<String>,
}

struct Registry {
    projects: Vec<RootedProject>,
    tombstones: serde_json::Map<String, Value>,
    /// This machine's code folders ([`mnemos_ide_projects::resolve::code_roots`]).
    code_roots: Vec<String>,
    /// This machine's home directory, when it has one.
    home: Option<String>,
    /// Projects whose sync is turned off on this machine
    /// (`syncDisabledProjectIds` in `workspace_state.json`). Their sessions
    /// are neither pushed nor pulled.
    disabled: HashSet<String>,
}

/// What a pushed session's cwd is tied to, strongest first.
#[derive(Debug, Clone)]
enum Anchor {
    /// A registry project's checkout.
    Project(RootedProject),
    /// One of the usual code folders, carried by its folder name.
    CodeRoot,
    /// The user's home directory ([`SessionMeta::home_rel`]).
    Home,
    /// Nothing here: the cwd travels unchanged ([`SessionMeta::abs_cwd`]).
    Absolute,
}

impl Registry {
    /// Whether `cwd` lies inside the checkout of a project whose sync is off
    /// here. Any containing record counts, not just the innermost, so a
    /// synced project nested inside an opted-out one cannot carry the
    /// opted-out one's sessions out.
    fn is_sync_disabled_cwd(&self, cwd: &str, platform: &str) -> bool {
        !self.disabled.is_empty()
            && self.projects.iter().any(|p| {
                self.disabled.contains(&p.id)
                    && p.root
                        .as_deref()
                        .is_some_and(|root| relative_components(cwd, root, platform).is_some())
            })
    }

    /// Whether another machine's session belongs to a project whose sync is
    /// off here.
    fn is_sync_disabled_meta(&self, meta: &SessionMeta) -> bool {
        !self.disabled.is_empty() && self.find(meta).is_some_and(|p| self.disabled.contains(&p.id))
    }

    /// What `cwd` is tied to, the root it is relative to and its components
    /// below that root. A project wins, then a code folder, then home; a cwd
    /// outside all of them is its own root. `None` only for a cwd that does
    /// not look like an absolute path.
    fn locate(&self, cwd: &str, platform: &str) -> Option<(Anchor, String, Vec<String>)> {
        if let Some((p, rel)) = self.containing(cwd, platform) {
            return Some((Anchor::Project(p.clone()), p.root.clone().unwrap_or_default(), rel));
        }
        if let Some((root, rel)) = self.code_root_containing(cwd, platform) {
            return Some((Anchor::CodeRoot, root.to_string(), rel));
        }
        if let Some(home) = self.home.as_deref() {
            if let Some(rel) = relative_components(cwd, home, platform) {
                return Some((Anchor::Home, home.to_string(), rel));
            }
        }
        registry::platform_of_path(cwd).map(|_| (Anchor::Absolute, cwd.to_string(), Vec::new()))
    }

    /// The code folder containing `cwd`, with `cwd`'s components below it.
    fn code_root_containing(&self, cwd: &str, platform: &str) -> Option<(&str, Vec<String>)> {
        self.code_roots
            .iter()
            .filter_map(|root| {
                let rel = relative_components(cwd, root, platform)?;
                Some((split_components(root, platform).len(), root.as_str(), rel))
            })
            .max_by_key(|(depth, _, _)| *depth)
            .map(|(_, root, rel)| (root, rel))
    }

    /// This machine's code folder called `name`, ignoring case. The first in
    /// [`Self::code_roots`] order when more than one matches.
    fn code_root_named(&self, name: &str, platform: &str) -> Option<&str> {
        self.code_roots
            .iter()
            .find(|r| registry::path_file_name(r, platform).is_some_and(|n| n.eq_ignore_ascii_case(name)))
            .map(String::as_str)
    }

    /// Where another machine's session lives here, with the platform whose
    /// path rules that root follows and how close a match it is (lower is
    /// better): 0 its project's checkout; 1 for a session outside every
    /// project, the code folder of the same name; 2 this machine's home, for
    /// one under the sender's home (a `homeRel` meta, or a code-folder meta
    /// whose folder is missing here and that carries `originHome`); 3 the
    /// sender's own root, unchanged, for an `absCwd` meta or a code-folder
    /// meta with no better place. `None` when this machine has no such place
    /// (a project with no checkout here, or no home here for a home meta).
    fn local_root(&self, meta: &SessionMeta, platform: &'static str) -> Option<(u8, String, &'static str)> {
        if let Some(root) = self.find(meta).and_then(|p| p.root.clone()) {
            return Some((0, root, platform));
        }
        let unchanged = || {
            registry::platform_of_path(&meta.origin_root)
                .map(|_| (3, meta.origin_root.clone(), meta.origin_platform()))
        };
        if let Some(name) = meta.code_root.as_deref().filter(|n| !n.trim().is_empty()) {
            if let Some(r) = self.code_root_named(name.trim(), platform) {
                return Some((1, r.to_string(), platform));
            }
            let via_home = meta.origin_home.as_deref().zip(self.home.as_deref()).and_then(|(from, home)| {
                let rel = relative_components(&meta.origin_root, from, meta.origin_platform())?;
                Some((2, join_components(home, &rel, platform), platform))
            });
            return via_home.or_else(unchanged);
        }
        if meta.home_rel {
            return self.home.clone().map(|h| (2, h, platform));
        }
        if meta.abs_cwd {
            return unchanged();
        }
        None
    }

    /// The record whose checkout contains `cwd` (the innermost, if records
    /// nest), with `cwd`'s components below that checkout.
    fn containing(&self, cwd: &str, platform: &str) -> Option<(&RootedProject, Vec<String>)> {
        self.projects
            .iter()
            .filter_map(|p| {
                let root = p.root.as_deref()?;
                let rel = relative_components(cwd, root, platform)?;
                Some((split_components(root, platform).len(), p, rel))
            })
            .max_by_key(|(depth, _, _)| *depth)
            .map(|(_, p, rel)| (p, rel))
    }

    /// This machine's record for another machine's session: by id (following
    /// `replacedBy`), else by gitRepo, else by name.
    fn find(&self, meta: &SessionMeta) -> Option<&RootedProject> {
        if let Some(id) = meta.project_id.as_deref().filter(|s| !s.is_empty()) {
            let live = registry::resolve_id_with(
                |c| self.projects.iter().any(|p| p.id == c),
                Some(&self.tombstones),
                id,
            );
            if let Some(p) = live.and_then(|l| self.projects.iter().find(|p| p.id == l)) {
                return Some(p);
            }
        }
        let name_is = |p: &RootedProject| {
            meta.project_name
                .as_deref()
                .is_some_and(|n| p.name.eq_ignore_ascii_case(n.trim()))
        };
        if let Some(repo) = meta.git_repo.as_deref().filter(|s| !s.trim().is_empty()) {
            let key = registry::git_repo_key(repo);
            let mut hits: Vec<&RootedProject> = self
                .projects
                .iter()
                .filter(|p| p.git_repo.as_deref().is_some_and(|g| registry::git_repo_key(g) == key))
                .collect();
            // A record checked out here beats one that is not; then the name.
            hits.sort_by_key(|p| (p.root.is_none(), !name_is(p)));
            if let Some(p) = hits.first() {
                return Some(*p);
            }
        }
        let mut hits: Vec<&RootedProject> = self.projects.iter().filter(|p| name_is(p)).collect();
        hits.sort_by_key(|p| p.root.is_none());
        hits.first().copied()
    }
}

/// Resolve every record's checkout on this machine through the locked store
/// update, so whatever discovery learns (`paths.<platform>`, `gitRepo`) is
/// persisted for the app and the other machines too.
fn load_registry(
    mut resolver: LocalResolver,
    fs: Arc<dyn FsEnv + Send + Sync>,
    files: Option<(PathBuf, PathBuf)>,
) -> (LocalResolver, Result<Registry, IdeError>) {
    // Warm the discovery scan on an unlocked snapshot first. The first scan
    // walks the code folders and reads every `.git/config`; doing it inside
    // the update below would hold the store lock (which the app's tab
    // reconcile also takes) for that whole walk. The snapshot's edits are
    // thrown away; the locked pass repeats them from the cached scan.
    let snapshot = match &files {
        Some((projects_path, workspace_path)) => {
            ProjectStore::load_blocking_from(projects_path, workspace_path)
        }
        None => ProjectStore::load_blocking(),
    };
    if let Ok(mut snapshot) = snapshot {
        let ids: Vec<String> = snapshot.projects.iter().map(|p| p.id.clone()).collect();
        for id in &ids {
            resolver.resolve_local_path(&mut snapshot, id, fs.as_ref());
        }
    }

    let result = {
        let resolve = |store: &mut ProjectStore| -> Registry {
            let ids: Vec<String> = store.projects.iter().map(|p| p.id.clone()).collect();
            for id in &ids {
                resolver.resolve_local_path(store, id, fs.as_ref());
            }
            // Fold duplicate registrations now so the ids below are the ones
            // the saved file will hold.
            store.normalize();
            let platform = fs.platform();
            let projects = store
                .projects
                .iter()
                .map(|p| RootedProject {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    git_repo: p.git_repo(),
                    root: p
                        .platform_path(platform)
                        .filter(|r| fs.is_dir(r))
                        .map(|r| r.to_string_lossy().into_owned()),
                })
                .collect();
            let tombstones = store
                .registry_value()
                .get(registry::DELETED_KEY)
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let code_roots = mnemos_ide_projects::resolve::code_roots(fs.as_ref());
            let home = fs.home().map(|h| h.to_string_lossy().into_owned());
            let disabled = store.sync_disabled_set();
            Registry { projects, tombstones, code_roots, home, disabled }
        };
        match files {
            Some((projects_path, workspace_path)) => {
                ProjectStore::update_blocking_at(projects_path, workspace_path, resolve)
            }
            None => ProjectStore::update_blocking(resolve),
        }
    };
    (resolver, result.map_err(IdeError::from))
}

// ─────────────────────────────────────────────────────────────────────────────
// Path strings (platform-aware, so a Mac path can be handled on Windows)
// ─────────────────────────────────────────────────────────────────────────────

/// Whether `path` has a `..` component.
fn has_parent_dir(path: &str, platform: &str) -> bool {
    split_components(path, platform).iter().any(|c| c == "..")
}

fn split_components(path: &str, platform: &str) -> Vec<String> {
    let win = platform == "win32";
    let path = if win {
        registry::strip_verbatim(path)
    } else {
        path.to_string()
    };
    path.split(|c: char| c == '/' || (win && c == '\\'))
        .filter(|c| !c.is_empty() && *c != ".")
        .map(str::to_string)
        .collect()
}

/// `path`'s components below `root`, or `None` when `path` is not inside it.
/// Case-insensitive on win32 and darwin, like `registry::same_path`. Also
/// `None` when either has a `..` component: compared literally, those could
/// claim a path is inside a root it is not.
fn relative_components(path: &str, root: &str, platform: &str) -> Option<Vec<String>> {
    let p = split_components(path, platform);
    let r = split_components(root, platform);
    if r.is_empty() || p.len() < r.len() || p.iter().chain(&r).any(|c| c == "..") {
        return None;
    }
    let fold = platform == "win32" || platform == "darwin";
    let same = r.iter().zip(&p).all(|(a, b)| {
        if fold {
            a.to_lowercase() == b.to_lowercase()
        } else {
            a == b
        }
    });
    same.then(|| p[r.len()..].to_vec())
}

fn join_components(root: &str, rel: &[String], platform: &str) -> String {
    rel.iter()
        .fold(root.to_string(), |acc, c| registry::path_join(&acc, c, platform))
}

// ─────────────────────────────────────────────────────────────────────────────
// Lines
// ─────────────────────────────────────────────────────────────────────────────

/// Offsets just past each `\n`. Bytes after the last `\n` are a line still
/// being written and are never counted.
fn line_ends(bytes: &[u8]) -> Vec<usize> {
    bytes
        .iter()
        .enumerate()
        .filter(|(_, b)| **b == b'\n')
        .map(|(i, _)| i + 1)
        .collect()
}

/// Line `i` without its `\n`.
fn line_at<'a>(bytes: &'a [u8], ends: &[usize], i: usize) -> &'a [u8] {
    let start = if i == 0 { 0 } else { ends[i - 1] };
    &bytes[start..ends[i] - 1]
}

#[derive(Deserialize)]
struct CwdProbe {
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Deserialize)]
struct UuidProbe {
    #[serde(default)]
    uuid: Option<String>,
}

/// The session's working directory: the first non-empty top-level `cwd`.
fn first_cwd(bytes: &[u8], ends: &[usize]) -> Option<String> {
    (0..ends.len()).find_map(|i| {
        serde_json::from_slice::<CwdProbe>(line_at(bytes, ends, i))
            .ok()?
            .cwd
            .filter(|c| !c.trim().is_empty())
    })
}

fn line_uuid(line: &[u8]) -> Option<String> {
    serde_json::from_slice::<UuidProbe>(line)
        .ok()?
        .uuid
        .filter(|u| !u.is_empty())
}

fn line_hash(line: &[u8]) -> [u8; 32] {
    Sha256::digest(line).into()
}

/// A line's identity for the push position: `u:<uuid>`, else `h:<hash>`.
fn line_key(line: &[u8]) -> String {
    match line_uuid(line) {
        Some(u) => format!("u:{u}"),
        None => {
            let hex: String = line_hash(line)[..16].iter().map(|b| format!("{b:02x}")).collect();
            format!("h:{hex}")
        }
    }
}

/// Keys of the (at most [`TAIL_KEYS`]) lines just before line `upto`.
fn tail_keys(bytes: &[u8], ends: &[usize], upto: usize) -> Vec<String> {
    let upto = upto.min(ends.len());
    (upto.saturating_sub(TAIL_KEYS)..upto)
        .map(|i| line_key(line_at(bytes, ends, i)))
        .collect()
}

/// The first line of the file that is not on the server yet.
///
/// Normally `pushed_lines`. But Claude Code rewrites the tail of a
/// transcript when it retracts a streamed message, so the file can hold
/// fewer lines before that position than were pushed, and a plain count then
/// skips the first new lines (or, if nothing was appended, re-sends the whole
/// file). The last pushed lines' keys locate the real position: lines only
/// ever move up in such a rewrite, so the newest of them still present, at or
/// before the old count, is the last line the server has.
///
/// Only uuid keys are searched for: a marker line (mode, title) can repeat,
/// and matching a new copy of one would skip the lines before it. `0` (send
/// everything; the reader drops lines it has) when nothing matches.
fn resume_line(bytes: &[u8], ends: &[usize], st: &SessionState) -> usize {
    let pushed = st.pushed_lines;
    if pushed == 0 {
        return 0;
    }
    let Some(last) = st.tail.last() else {
        // State from before `tail` existed: the count is all there is.
        return if pushed > ends.len() { 0 } else { pushed };
    };
    if pushed <= ends.len() && line_key(line_at(bytes, ends, pushed - 1)) == *last {
        return pushed;
    }
    let wanted: HashSet<&str> = st
        .tail
        .iter()
        .filter_map(|k| k.strip_prefix("u:"))
        .collect();
    if wanted.is_empty() {
        return 0;
    }
    (0..pushed.min(ends.len()))
        .rev()
        .find(|&i| line_uuid(line_at(bytes, ends, i)).is_some_and(|u| wanted.contains(u.as_str())))
        .map_or(0, |i| i + 1)
}

/// The newest top-level `timestamp` among `lines`.
fn newest_line_time<'a>(lines: impl Iterator<Item = &'a [u8]>) -> Option<SystemTime> {
    lines
        .filter_map(|line| {
            let (start, end) = top_level_string_span(line, "timestamp")?;
            let s: String = serde_json::from_slice(&line[start..end]).ok()?;
            registry::parse_timestamp(&s)
        })
        .max()
        .map(SystemTime::from)
}

/// Split lines `[from, ends.len())` into runs of at most `max` bytes; a line
/// longer than `max` on its own gets a run of its own.
fn chunk_lines(ends: &[usize], from: usize, max: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = from;
    while start < ends.len() {
        let base = if start == 0 { 0 } else { ends[start - 1] };
        let mut end = start + 1;
        while end < ends.len() && ends[end] - base <= max {
            end += 1;
        }
        out.push((start, end));
        start = end;
    }
    out
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while matches!(b.get(i), Some(b' ' | b'\t' | b'\r' | b'\n')) {
        i += 1;
    }
    i
}

/// Index just past the closing quote of the JSON string opening at `start`.
fn string_end(b: &[u8], start: usize) -> Option<usize> {
    let mut i = start + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// Index just past the JSON value starting at `start`.
fn value_end(b: &[u8], start: usize) -> Option<usize> {
    match b.get(start)? {
        b'"' => string_end(b, start),
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut i = start;
            while i < b.len() {
                match b[i] {
                    b'"' => {
                        i = string_end(b, i)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(i + 1);
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            None
        }
        _ => {
            let mut i = start;
            while i < b.len() && !matches!(b[i], b',' | b'}' | b']' | b' ' | b'\t' | b'\r' | b'\n')
            {
                i += 1;
            }
            (i > start).then_some(i)
        }
    }
}

/// Byte range (quotes included) of the string value of the top-level `key`
/// in a JSON object line.
///
/// A scanner rather than parse-and-reserialize: re-serializing would reorder
/// keys and re-escape text, so the line would no longer match the sender's
/// copy byte for byte, and nested objects (tool results) may carry their own
/// `cwd` that must stay as it is.
fn top_level_string_span(line: &[u8], key: &str) -> Option<(usize, usize)> {
    let mut i = skip_ws(line, 0);
    if line.get(i) != Some(&b'{') {
        return None;
    }
    i += 1;
    loop {
        i = skip_ws(line, i);
        if line.get(i) != Some(&b'"') {
            return None;
        }
        let key_end = string_end(line, i)?;
        let is_key = &line[i + 1..key_end - 1] == key.as_bytes();
        i = skip_ws(line, key_end);
        if line.get(i) != Some(&b':') {
            return None;
        }
        i = skip_ws(line, i + 1);
        let start = i;
        let end = value_end(line, start)?;
        if is_key {
            return (line.get(start) == Some(&b'"')).then_some((start, end));
        }
        i = skip_ws(line, end);
        if line.get(i) != Some(&b',') {
            return None;
        }
        i += 1;
    }
}

/// How to move a line's `cwd` from the sending machine onto this one.
struct CwdMap {
    from_root: String,
    from_platform: &'static str,
    to_root: String,
    to_platform: &'static str,
}

/// The line with its top-level `cwd` moved from `from_root` to `to_root`
/// (the part below the root kept, separators converted). `None` when there is
/// nothing to rewrite: no `cwd`, or one outside the sender's project.
fn rewrite_cwd(line: &[u8], map: &CwdMap) -> Option<Vec<u8>> {
    if map.from_platform == map.to_platform && map.from_root == map.to_root {
        return None;
    }
    let (start, end) = top_level_string_span(line, "cwd")?;
    let cwd: String = serde_json::from_slice(&line[start..end]).ok()?;
    let rel = relative_components(&cwd, &map.from_root, map.from_platform)?;
    let moved = join_components(&map.to_root, &rel, map.to_platform);
    if moved == cwd {
        return None;
    }
    let encoded = serde_json::to_string(&moved).ok()?;
    let mut out = Vec::with_capacity(line.len() + encoded.len());
    out.extend_from_slice(&line[..start]);
    out.extend_from_slice(encoded.as_bytes());
    out.extend_from_slice(&line[end..]);
    Some(out)
}

/// One incoming line, already rewritten for this machine.
struct Incoming {
    line: Vec<u8>,
    uuid: Option<String>,
    /// Index of the segment it came from.
    segment: usize,
}

/// Indices of the incoming lines to append.
///
/// Lines with a `uuid` are kept only when that uuid is new here. Lines
/// without one (mode, title, permission and cost markers) are compared by
/// hash, but only inside a stretch the receiver already has, i.e. when the
/// nearest uuid line of the same segment (before, else after) was a
/// duplicate. A new stretch keeps them even when an identical line exists:
/// markers repeat (`mode: normal` after a detour to `plan`), and dropping the
/// repeat would leave the older value as the session's latest. Stretches end
/// at segment boundaries because a re-upload after lost state starts a
/// segment of its own.
fn select_new_lines(
    incoming: &[Incoming],
    seen_uuids: &mut HashSet<String>,
    seen_hashes: &mut HashSet<[u8; 32]>,
) -> Vec<usize> {
    let dup: Vec<Option<bool>> = incoming
        .iter()
        .map(|l| l.uuid.as_ref().map(|u| !seen_uuids.insert(u.clone())))
        .collect();
    let mut prev: Vec<Option<bool>> = vec![None; incoming.len()];
    let mut last = None;
    for i in 0..incoming.len() {
        if i > 0 && incoming[i].segment != incoming[i - 1].segment {
            last = None;
        }
        prev[i] = last;
        if dup[i].is_some() {
            last = dup[i];
        }
    }
    let mut next: Vec<Option<bool>> = vec![None; incoming.len()];
    let mut last = None;
    for i in (0..incoming.len()).rev() {
        if i + 1 < incoming.len() && incoming[i].segment != incoming[i + 1].segment {
            last = None;
        }
        next[i] = last;
        if dup[i].is_some() {
            last = dup[i];
        }
    }
    let mut keep = Vec::new();
    for (i, line) in incoming.iter().enumerate() {
        match dup[i] {
            Some(is_dup) => {
                if !is_dup {
                    keep.push(i);
                }
            }
            None => {
                let known_stretch = prev[i].or(next[i]).unwrap_or(false);
                let hash = line_hash(&line.line);
                if known_stretch && seen_hashes.contains(&hash) {
                    continue;
                }
                seen_hashes.insert(hash);
                keep.push(i);
            }
        }
    }
    keep
}

fn gzip(raw: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut enc = GzEncoder::new(Vec::with_capacity(raw.len() / 4), Compression::default());
    enc.write_all(raw)?;
    enc.finish()
}

fn gunzip(gz: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    GzDecoder::new(gz)
        .take(MAX_SEGMENT_DECODED_BYTES + 1)
        .read_to_end(&mut out)?;
    if out.len() as u64 > MAX_SEGMENT_DECODED_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "segment inflates past the size limit",
        ));
    }
    Ok(out)
}

// ─────────────────────────────────────────────────────────────────────────────
// Files
// ─────────────────────────────────────────────────────────────────────────────

fn io_err(what: &str, path: &Path, e: std::io::Error) -> IdeError {
    IdeError::Other(format!("{what} {}: {e}", path.display()))
}

fn join_err(e: tokio::task::JoinError) -> IdeError {
    IdeError::Other(format!("transcript sync task failed: {e}"))
}

fn sibling_tmp(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".tcb-sync.tmp");
    path.with_file_name(name)
}

/// Write `bytes` to `path` via a synced temp file and a rename, so a crash
/// leaves either the old file or the new one.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = sibling_tmp(path);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

fn is_older_than(mtime: SystemTime, age: Duration) -> bool {
    SystemTime::now()
        .duration_since(mtime)
        .is_ok_and(|d| d > age)
}

/// Written within the live window, or stamped in the future (clock skew):
/// either way, not safe to merge into yet.
fn is_live(mtime: SystemTime) -> bool {
    SystemTime::now()
        .duration_since(mtime)
        .map_or(true, |d| d < LIVE_WINDOW)
}

/// Session ids a running Claude Code process on this machine has open, from
/// Claude Code's own registry of live sessions, `~/.claude/sessions/<pid>.json`
/// (`{"pid": .., "sessionId": ..}`). A file whose process is gone is left
/// over from a crash and does not count.
///
/// The transcript's mtime alone is not enough: a session waiting at the
/// prompt goes untouched for as long as the user is away, and merging another
/// machine's lines into it then forks the conversation, because the process
/// chains its next message to the last one it has in memory.
fn open_sessions(sessions_dir: &Path) -> HashSet<String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Entry {
        pid: u32,
        session_id: String,
    }
    let Ok(dir) = std::fs::read_dir(sessions_dir) else {
        return HashSet::new();
    };
    let entries: Vec<Entry> = dir
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok())
        .collect();
    if entries.is_empty() {
        return HashSet::new();
    }
    let pids: Vec<sysinfo::Pid> = entries.iter().map(|e| sysinfo::Pid::from_u32(e.pid)).collect();
    let mut system = sysinfo::System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&pids), true);
    entries
        .into_iter()
        .filter(|e| system.process(sysinfo::Pid::from_u32(e.pid)).is_some())
        .map(|e| e.session_id)
        .collect()
}

fn interrupted(why: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Interrupted, why.to_string())
}

/// When one session has copies in several slug dirs (resumed from another
/// folder), only the newest is pushed: that is the one Claude Code writes.
/// Pushing each would move the one push position back and forth between
/// them and re-upload a whole copy on every pass.
fn newest_copy_per_session(files: Vec<PathBuf>, state: &SyncState) -> Vec<PathBuf> {
    let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let mut best: BTreeMap<String, (PathBuf, Option<SystemTime>)> = BTreeMap::new();
    let mut other: Vec<PathBuf> = Vec::new();
    for file in files {
        let Some(sid) = session_id_of(&file).map(str::to_string) else {
            other.push(file);
            continue;
        };
        let t = mtime(&file);
        match best.get(&sid) {
            Some((_, bt)) if *bt >= t => {}
            _ => {
                best.insert(sid, (file, t));
            }
        }
    }
    let mut out = other;
    for (sid, (file, t)) in best {
        // A copy outside this batch that is newer still wins.
        let known = state
            .sessions
            .get(&sid)
            .and_then(|s| s.local_path.as_deref())
            .map(PathBuf::from)
            .filter(|p| *p != file);
        if known.as_deref().and_then(mtime).is_some_and(|kt| Some(kt) > t) {
            continue;
        }
        out.push(file);
    }
    out
}

struct LocalSession {
    sid: String,
    path: PathBuf,
    mtime: SystemTime,
    len: u64,
}

/// Every top-level `<slug>/<uuid>.jsonl` under `claude_projects_dir`.
fn scan_local_sessions(claude_projects_dir: &Path) -> Vec<LocalSession> {
    let mut out = Vec::new();
    let Ok(slugs) = std::fs::read_dir(claude_projects_dir) else {
        return out;
    };
    for slug in slugs.flatten() {
        if !slug.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Ok(files) = std::fs::read_dir(slug.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if !is_transcript_path_in(claude_projects_dir, &path) {
                continue;
            }
            let Ok(meta) = file.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let Some(sid) = session_id_of(&path).map(str::to_string) else {
                continue;
            };
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            out.push(LocalSession { sid, path, mtime, len: meta.len() });
        }
    }
    out
}

/// A downloaded segment ready to merge.
struct PulledSegment {
    name: String,
    raw: Vec<u8>,
    map: CwdMap,
    /// [`SessionMeta::source_mtime`] of the machine that uploaded it.
    source_mtime: Option<SystemTime>,
}

struct MergeResult {
    appended: usize,
    total_lines: usize,
    total_bytes: u64,
    /// [`tail_keys`] of the file as written.
    tail: Vec<String>,
    wrote: bool,
}

/// Append the new lines of `segments` to `target` (creating it if needed).
///
/// The whole file is rewritten through a temp file and a rename, so a crash
/// can never leave half a line behind, and the rename is refused when the
/// file changed since it was read (Claude Code resumed it meanwhile).
///
/// `pushed_len` is the length the push just before saw. Every line of the
/// result counts as pushed afterwards, so a file that has grown since then
/// (Claude Code wrote while the segments downloaded) is left for the next
/// pass rather than having its new lines marked as uploaded.
fn merge_into_file(
    target: &Path,
    segments: &[PulledSegment],
    pushed_len: Option<u64>,
) -> std::io::Result<MergeResult> {
    let (existing, signature) = match std::fs::metadata(target) {
        Ok(meta) => {
            // Stat before reading: an append landing between the two must
            // show up as a length mismatch, not ride along in the signature.
            let (len, mtime) = (meta.len(), meta.modified()?);
            if pushed_len.is_some_and(|p| p != len) {
                return Err(interrupted("transcript grew after its push; will retry"));
            }
            if is_live(mtime) {
                return Err(interrupted("transcript is in use; will retry"));
            }
            let bytes = std::fs::read(target)?;
            if bytes.len() as u64 != len {
                return Err(interrupted("transcript changed while reading; will retry"));
            }
            (bytes, Some((len, mtime)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if pushed_len.is_some() {
                return Err(interrupted("transcript vanished after its push; will retry"));
            }
            (Vec::new(), None)
        }
        Err(e) => return Err(e),
    };
    let ends = line_ends(&existing);
    let mut seen_uuids: HashSet<String> = HashSet::new();
    let mut seen_hashes: HashSet<[u8; 32]> = HashSet::new();
    for i in 0..ends.len() {
        let line = line_at(&existing, &ends, i);
        match line_uuid(line) {
            Some(u) => {
                seen_uuids.insert(u);
            }
            None => {
                seen_hashes.insert(line_hash(line));
            }
        }
    }

    let mut incoming: Vec<Incoming> = Vec::new();
    for (segment, seg) in segments.iter().enumerate() {
        let seg_ends = line_ends(&seg.raw);
        for i in 0..seg_ends.len() {
            let raw = line_at(&seg.raw, &seg_ends, i);
            let line = rewrite_cwd(raw, &seg.map).unwrap_or_else(|| raw.to_vec());
            let uuid = line_uuid(&line);
            incoming.push(Incoming { line, uuid, segment });
        }
    }
    let keep = select_new_lines(&incoming, &mut seen_uuids, &mut seen_hashes);
    if keep.is_empty() {
        return Ok(MergeResult {
            appended: 0,
            total_lines: ends.len(),
            total_bytes: ends.last().copied().unwrap_or(0) as u64,
            tail: tail_keys(&existing, &ends, ends.len()),
            wrote: false,
        });
    }
    let newest_incoming = newest_line_time(keep.iter().map(|i| incoming[*i].line.as_slice()));

    // A trailing partial line this old is a crashed write; terminate it so
    // the first appended line does not get glued onto it.
    let partial = existing.last().is_some_and(|b| *b != b'\n');
    let mut out = existing;
    if partial {
        out.push(b'\n');
    }
    for i in &keep {
        out.extend_from_slice(&incoming[*i].line);
        out.push(b'\n');
    }

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = sibling_tmp(target);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&out)?;
        f.sync_all()?;
    }
    let unchanged = match (signature, std::fs::metadata(target)) {
        (Some((len, mtime)), Ok(now)) => now.len() == len && now.modified()? == mtime,
        (None, Err(e)) => e.kind() == std::io::ErrorKind::NotFound,
        _ => false,
    };
    if !unchanged {
        let _ = std::fs::remove_file(&tmp);
        return Err(interrupted("transcript changed while merging; will retry"));
    }
    std::fs::rename(&tmp, target).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    // Dated by its newest line, as if it had been written here then, so
    // `claude --resume` orders it among the local sessions and Claude Code's
    // own cleanup ages it normally. `now` would make a new machine's first
    // pull put every session of the last month at the top.
    let now = SystemTime::now();
    let newest_source = segments.iter().filter_map(|s| s.source_mtime).max();
    let stamp = [signature.map(|(_, t)| t), newest_incoming, newest_source]
        .into_iter()
        .flatten()
        .max()
        .map_or(now, |t| t.min(now));
    std::fs::OpenOptions::new()
        .write(true)
        .open(target)?
        .set_modified(stamp)?;

    let out_ends = line_ends(&out);
    Ok(MergeResult {
        appended: keep.len(),
        total_lines: out_ends.len(),
        total_bytes: out.len() as u64,
        tail: tail_keys(&out, &out_ends, out_ends.len()),
        wrote: true,
    })
}

/// Whether every segment of `sid` on the server (any machine's) was uploaded
/// more than `age` ago. Unparseable times count as old.
fn newest_segment_is_older_than(
    remote: &BTreeMap<String, RemoteSession>,
    sid: &str,
    age: Duration,
) -> bool {
    let newest = remote
        .get(sid)
        .into_iter()
        .flat_map(|r| r.segments.values())
        .flat_map(|seqs| seqs.values())
        .filter_map(|lm| registry::parse_timestamp(lm))
        .max();
    newest.is_none_or(|t| is_older_than(SystemTime::from(t), age))
}

/// Another machine's segment that this machine has not applied yet.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pending {
    machine: String,
    seq: u64,
    /// Server upload time from the listing (RFC3339).
    last_modified: String,
}

/// Pending segments merged across machines by upload time, each machine's
/// own segments staying in sequence order. Time order keeps interleaved
/// work (Mac, then PC, then Mac) in the order it happened when a machine
/// takes in several machines' segments at once; ties fall back to
/// (machine, seq).
fn order_segments(mut pending: Vec<Pending>) -> Vec<Pending> {
    pending.sort_by(|a, b| (&a.machine, a.seq).cmp(&(&b.machine, b.seq)));
    let mut queues: BTreeMap<String, VecDeque<Pending>> = BTreeMap::new();
    for p in pending {
        queues.entry(p.machine.clone()).or_default().push_back(p);
    }
    let mut out = Vec::new();
    loop {
        let pick: Option<(Option<DateTime<Utc>>, String)> = queues
            .iter()
            .filter_map(|(m, q)| {
                q.front()
                    .map(|p| (registry::parse_timestamp(&p.last_modified), m.clone()))
            })
            .min();
        let Some((_, machine)) = pick else { break };
        if let Some(p) = queues.get_mut(&machine).and_then(VecDeque::pop_front) {
            out.push(p);
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// Passes
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct Stats {
    pushed_sessions: usize,
    pushed_lines: usize,
    pushed_segments: usize,
    pulled_sessions: usize,
    pulled_lines: usize,
    pulled_segments: usize,
}

/// State for one locked call: loaded once, saved as it changes.
struct Pass {
    machine: String,
    state: SyncState,
    resolver: LocalResolver,
    metas: MetaCache,
    tried: Tried,
    skipped: SkipMarks,
    registry: Option<Registry>,
    remote: BTreeMap<String, RemoteSession>,
    /// `remote` holds every session (one full listing was made this pass).
    full_listing: bool,
    listed: HashSet<String>,
    /// A pull left a session to try again soon: open or being written
    /// here, or failed for a reason that may pass.
    retry: bool,
    stats: Stats,
}

impl Pass {
    fn log_push(&self, what: &str) {
        let s = &self.stats;
        if s.pushed_segments > 0 {
            tracing::info!(
                target: TARGET,
                what,
                sessions = s.pushed_sessions,
                lines = s.pushed_lines,
                segments = s.pushed_segments,
                "pushed transcript lines"
            );
        }
    }

    /// Info when something arrived; the every-10-minutes "nothing new" case
    /// stays at debug so it does not flood mnemos.log.
    fn log_pull(&self) {
        let s = &self.stats;
        if s.pulled_segments > 0 {
            tracing::info!(
                target: TARGET,
                sessions = s.pulled_sessions,
                lines = s.pulled_lines,
                segments = s.pulled_segments,
                "pulled transcript lines"
            );
        } else {
            tracing::debug!(target: TARGET, "pulled no transcript lines");
        }
    }
}

/// What happened to one session in a push.
#[derive(Debug, PartialEq, Eq)]
enum PushOutcome {
    Skipped,
    UpToDate,
    Pushed,
}

/// What happened to one session in a pull.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PullOutcome {
    /// Merged, or there was nothing new to merge.
    Applied,
    /// Cannot be applied here as things stand: no meta, no matching
    /// project, no checkout of it, a cwd too long for Claude Code.
    Unavailable,
    /// Open or being written here: try again soon.
    Waiting,
}

/// The server refused an upload for its size: a 413, from the proxy in
/// front of the admin backend or from the blob route. The same bytes can
/// only be refused again. No other status says that much about the line
/// (a 400 or 422 that hit every upload would otherwise drop every line), so
/// on anything else the line stays for the next push.
fn refused_for_content(e: &IdeError) -> bool {
    matches!(e, IdeError::Http { status, .. } if *status == reqwest::StatusCode::PAYLOAD_TOO_LARGE)
}

/// Whether the next index poll should pull again although the index did
/// not change: the pass left a session waiting, or failed for a reason that
/// may pass. A failure that would only repeat waits for new segments or the
/// hourly full pull instead of costing a request every 30 s.
fn retry_soon(pass: &Pass, result: &Result<(), IdeError>) -> bool {
    pass.retry || result.as_ref().is_err_and(is_transient)
}

/// Remember that a pull dealt with `sid` up to `newest` (machine -> seq).
fn mark_tried(tried: &mut Tried, sid: &str, newest: BTreeMap<String, u64>) {
    for (machine, seq) in newest {
        let t = tried.entry((sid.to_string(), machine)).or_default();
        *t = (*t).max(seq);
    }
}

/// A stand-in for a transcript line the server will not take: the same
/// JSON with every string over [`STUB_MAX_STRING`] replaced by a note of its
/// size, and a base64 image or document block holding one replaced by a
/// text block saying so (cut data in it would be sent back to the API on
/// every turn of a resumed session, and refused). Every other key stays,
/// `uuid`, `parentUuid`, `timestamp` and `tool_use_id` included: Claude Code
/// resumes a session by walking each line's `parentUuid`, and the history
/// stops at a missing line. `None` when the line is not JSON or holds
/// nothing that long.
fn stub_line(line: &[u8]) -> Option<Vec<u8>> {
    let text = line.strip_suffix(b"\n").unwrap_or(line);
    let mut value: Value = serde_json::from_slice(text).ok()?;
    if !cut_long_strings(&mut value) {
        return None;
    }
    let mut out = serde_json::to_vec(&value).ok()?;
    out.push(b'\n');
    Some(out)
}

/// [`stub_line`]'s cut, in place. `true` when anything was cut.
fn cut_long_strings(value: &mut Value) -> bool {
    match value {
        Value::String(s) if s.len() > STUB_MAX_STRING => {
            *s = format!("[omitted by Mnemos sync: {} bytes]", s.len());
            true
        }
        Value::Array(items) => items.iter_mut().fold(false, |cut, v| cut_long_strings(v) | cut),
        Value::Object(map) => {
            let media = map
                .get("type")
                .and_then(Value::as_str)
                .filter(|t| matches!(*t, "image" | "document"))
                .map(str::to_string);
            let data_len = map
                .get("source")
                .and_then(|s| s.get("data"))
                .and_then(Value::as_str)
                .map_or(0, str::len);
            match media {
                Some(kind) if data_len > STUB_MAX_STRING => {
                    *value = serde_json::json!({
                        "type": "text",
                        "text": format!("[{kind} omitted by Mnemos sync: {data_len} bytes]"),
                    });
                    true
                }
                _ => map.values_mut().fold(false, |cut, v| cut_long_strings(v) | cut),
            }
        }
        _ => false,
    }
}

impl TranscriptSync {
    fn state_path(&self) -> PathBuf {
        self.store_dir.join(STATE_FILE)
    }

    async fn begin_pass(&self, inner: &mut Inner) -> Result<Pass, IdeError> {
        let machine = match &inner.machine_id {
            Some(m) => m.clone(),
            None => {
                let path = self.store_dir.join(MACHINE_ID_FILE);
                let id = tokio::task::spawn_blocking({
                    let store = self.store_dir.clone();
                    move || crate::machine::load_or_create_id(&store)
                })
                .await
                .map_err(join_err)?
                .map_err(|e| io_err("machine id", &path, e))?;
                inner.machine_id = Some(id.clone());
                id
            }
        };
        let state = self.load_state().await;
        Ok(Pass {
            machine,
            state,
            resolver: std::mem::take(&mut inner.resolver),
            metas: std::mem::take(&mut inner.metas),
            tried: std::mem::take(&mut inner.tried),
            skipped: std::mem::take(&mut inner.skipped),
            registry: None,
            remote: BTreeMap::new(),
            full_listing: false,
            listed: HashSet::new(),
            retry: false,
            stats: Stats::default(),
        })
    }

    fn end_pass(&self, inner: &mut Inner, mut pass: Pass) {
        inner.resolver = std::mem::take(&mut pass.resolver);
        inner.metas = std::mem::take(&mut pass.metas);
        inner.tried = std::mem::take(&mut pass.tried);
        inner.skipped = std::mem::take(&mut pass.skipped);
    }

    /// Whether `cwd` is inside `~/.claude` (the parent of the projects dir)
    /// or the Mnemos data dir (the parent of the store dir). Sessions run
    /// there are about this machine's own config and state, so they are
    /// neither pushed nor written here when another machine's maps there.
    /// A cwd with a `..` component counts as private too: it cannot be
    /// compared without resolving it, and it may lead into either.
    fn is_private_cwd(&self, cwd: &str, platform: &str) -> bool {
        // Tests build their fake homes and checkouts in temp dirs, so the temp rule is off there;
        // is_temp_cwd has its own test.
        if has_parent_dir(cwd, platform) || (!cfg!(test) && is_temp_cwd(cwd, platform)) {
            return true;
        }
        [self.claude_projects_dir.parent(), self.store_dir.parent()]
            .into_iter()
            .flatten()
            .any(|dir| relative_components(cwd, &dir.to_string_lossy(), platform).is_some())
    }

    /// `~/.claude/sessions`, next to the projects dir.
    fn sessions_dir(&self) -> Option<PathBuf> {
        self.claude_projects_dir.parent().map(|d| d.join("sessions"))
    }

    /// Session ids a running Claude Code process here has open.
    async fn open_here(&self) -> Result<HashSet<String>, IdeError> {
        let Some(dir) = self.sessions_dir() else {
            return Ok(HashSet::new());
        };
        tokio::task::spawn_blocking(move || open_sessions(&dir))
            .await
            .map_err(join_err)
    }

    /// Whether a running Claude Code process here has `sid` open.
    async fn is_open_here(&self, sid: &str) -> Result<bool, IdeError> {
        Ok(self.open_here().await?.contains(sid))
    }

    /// Whether this machine's copy of `sid` (the one the state last saw)
    /// was written within the live window.
    async fn live_here(pass: &Pass, sid: &str) -> bool {
        let Some(path) = pass.state.sessions.get(sid).and_then(|s| s.local_path.clone()) else {
            return false;
        };
        tokio::fs::metadata(&path)
            .await
            .and_then(|m| m.modified())
            .is_ok_and(is_live)
    }

    /// A missing or unreadable state file means "nothing pushed or applied
    /// yet". That is safe: sequence numbers come from the server listing and
    /// the reader skips lines it already has.
    async fn load_state(&self) -> SyncState {
        let path = self.state_path();
        match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!(target: TARGET, path = %path.display(), error = %e, "transcript sync state is unreadable; starting over");
                SyncState::default()
            }),
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(target: TARGET, path = %path.display(), error = %e, "cannot read transcript sync state; starting over");
                }
                SyncState::default()
            }
        }
    }

    async fn save_state(&self, state: &SyncState) -> Result<(), IdeError> {
        let path = self.state_path();
        let bytes = serde_json::to_vec_pretty(state)?;
        tokio::task::spawn_blocking({
            let path = path.clone();
            move || write_atomic(&path, &bytes)
        })
        .await
        .map_err(join_err)?
        .map_err(|e| io_err("write", &path, e))
    }

    async fn ensure_registry(&self, pass: &mut Pass) -> Result<(), IdeError> {
        if pass.registry.is_some() {
            return Ok(());
        }
        let resolver = std::mem::take(&mut pass.resolver);
        let fs = self.opts.fs.clone();
        let files = self.opts.registry_files.clone();
        let (resolver, registry) =
            tokio::task::spawn_blocking(move || load_registry(resolver, fs, files))
                .await
                .map_err(join_err)?;
        pass.resolver = resolver;
        pass.registry = Some(registry?);
        Ok(())
    }

    async fn full_listing(&self, pass: &mut Pass) -> Result<(), IdeError> {
        let items = self.blobs.list(REMOTE_PREFIX).await?;
        pass.remote = index_listing(&items);
        pass.full_listing = true;
        Ok(())
    }

    /// Make sure `pass.remote` knows what the server holds for `sid`.
    async fn ensure_remote(&self, pass: &mut Pass, sid: &str) -> Result<(), IdeError> {
        if pass.full_listing || pass.listed.contains(sid) {
            return Ok(());
        }
        let items = self.blobs.list(&session_prefix(sid)).await?;
        let entry = index_listing(&items).remove(sid).unwrap_or_default();
        pass.remote.insert(sid.to_string(), entry);
        pass.listed.insert(sid.to_string());
        Ok(())
    }

    /// `full`: list the whole store and look at every eligible session (the
    /// startup sweep). Otherwise only sessions that may have something to
    /// push, listed per session unless there are many
    /// ([`Self::sweep_changed`]).
    async fn sweep_pass(&self, pass: &mut Pass, full: bool) -> Result<(), IdeError> {
        let dir = self.claude_projects_dir.clone();
        let sessions = tokio::task::spawn_blocking(move || scan_local_sessions(&dir))
            .await
            .map_err(join_err)?;
        if full {
            self.ensure_registry(pass).await?;
            if pass.registry.as_ref().is_some_and(|r| !r.disabled.is_empty()) {
                self.full_listing(pass).await?;
                self.purge_opted_out(pass, &sessions).await;
            }
        }
        if !full {
            // Forget marks of files that are gone.
            let present: HashSet<&Path> = sessions.iter().map(|s| s.path.as_path()).collect();
            pass.skipped.retain(|p, _| present.contains(p.as_path()));
        }
        let eligible: Vec<LocalSession> = sessions
            .into_iter()
            .filter(|s| {
                let old = is_older_than(s.mtime, MAX_SESSION_AGE);
                if old {
                    tracing::debug!(target: TARGET, sid = %s.sid, "skip push: not modified in 30 days");
                }
                !old
            })
            .collect();
        if eligible.is_empty() {
            tracing::debug!(target: TARGET, "sweep: no recent transcripts");
            return Ok(());
        }
        let stats: HashMap<PathBuf, (u64, SystemTime)> = eligible
            .iter()
            .map(|s| (s.path.clone(), (s.len, s.mtime)))
            .collect();
        let mut files = newest_copy_per_session(
            eligible.into_iter().map(|s| s.path).collect(),
            &pass.state,
        );
        if full {
            if !pass.full_listing {
                self.full_listing(pass).await?;
            }
        } else {
            files = self.may_need_push(pass, files, &stats).await?;
            if files.is_empty() {
                tracing::debug!(target: TARGET, "sweep: nothing changed");
                return Ok(());
            }
            if files.len() > PER_SESSION_LISTING_MAX {
                self.full_listing(pass).await?;
            }
        }
        let mut first_err = None;
        for file in &files {
            if let Err(e) = self.push_file(pass, file).await {
                tracing::warn!(target: TARGET, path = %file.display(), error = %e, "transcript push failed");
                first_err.get_or_insert(e);
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    /// Delete what this machine uploaded of sessions in a project whose sync
    /// is now off here (gates review finding 4: the opt-out used to stop
    /// new uploads only, so other machines kept pulling the old ones). Runs
    /// on the startup sweep, over every local session the full listing shows
    /// this machine's blobs for, whatever its age. Only this machine's own
    /// meta and segments go: another machine's copy of the same session is
    /// that machine's to keep or opt out of. What other machines already
    /// pulled stays on them. The local state is left alone, so the session
    /// is not uploaded again while the project stays off, and after it is
    /// turned back on only lines written since go up. Failures are logged
    /// and retried at the next start.
    async fn purge_opted_out(&self, pass: &mut Pass, local: &[LocalSession]) {
        let Some(registry) = pass.registry.as_ref() else {
            return;
        };
        let platform = self.opts.fs.platform();
        let machine = pass.machine.clone();
        let mut doomed: BTreeSet<String> = BTreeSet::new();
        for session in local {
            if doomed.contains(&session.sid) {
                continue;
            }
            let ours = pass.remote.get(&session.sid).is_some_and(|r| {
                r.metas.contains_key(&machine) || r.segments.contains_key(&machine)
            });
            if !ours {
                continue;
            }
            let path = session.path.clone();
            let cwd = match tokio::task::spawn_blocking(move || read_first_cwd(&path)).await {
                Ok(cwd) => cwd,
                Err(e) => {
                    tracing::warn!(target: TARGET, error = %e, "opt-out purge: reading a transcript failed");
                    continue;
                }
            };
            if cwd.is_some_and(|cwd| registry.is_sync_disabled_cwd(&cwd, platform)) {
                doomed.insert(session.sid.clone());
            }
        }
        for sid in doomed {
            let Some(remote) = pass.remote.get(&sid) else {
                continue;
            };
            let paths = own_blob_paths(remote, &sid, &machine);
            let mut deleted = 0usize;
            for path in &paths {
                match self.blobs.delete(path).await {
                    Ok(()) => deleted += 1,
                    Err(e) => {
                        tracing::warn!(target: TARGET, %sid, %path, error = %e, "opt-out purge: delete failed; retried at the next start");
                        break;
                    }
                }
            }
            if deleted == paths.len() {
                if let Some(r) = pass.remote.get_mut(&sid) {
                    r.metas.remove(&machine);
                    r.segments.remove(&machine);
                }
            }
            tracing::info!(target: TARGET, %sid, deleted, "removed this machine's uploads of a session whose project sync is off here");
        }
    }

    /// The `files` a cheap sweep has to look at, judged on local facts
    /// alone: not while Claude Code is writing it (the watcher's push has
    /// it), not when its length is exactly what was pushed, and not when an
    /// earlier push skipped it and the file has not changed since.
    async fn may_need_push(
        &self,
        pass: &mut Pass,
        files: Vec<PathBuf>,
        stats: &HashMap<PathBuf, (u64, SystemTime)>,
    ) -> Result<Vec<PathBuf>, IdeError> {
        let changed: Vec<PathBuf> = files
            .into_iter()
            .filter(|f| {
                let Some((len, mtime)) = stats.get(f).copied() else {
                    return true;
                };
                if is_live(mtime) {
                    return false;
                }
                let st = session_id_of(f).and_then(|sid| pass.state.sessions.get(sid));
                !st.is_some_and(|st| st.pushed_lines > 0 && st.pushed_bytes == Some(len))
            })
            .collect();
        Ok(changed
            .into_iter()
            .filter(|f| {
                let (Some(mark), Some((len, mtime))) = (pass.skipped.get(f), stats.get(f)) else {
                    return true;
                };
                !(mark.len == *len && mark.mtime == *mtime)
            })
            .collect())
    }

    /// Upload the lines of `file` not yet on the server.
    async fn push_file(&self, pass: &mut Pass, file: &Path) -> Result<PushOutcome, IdeError> {
        self.push_file_seen(pass, file).await.map(|(outcome, _)| outcome)
    }

    /// Whether the server has this machine's meta for `sid`, as far as it
    /// matters: segments without their meta are useless to other machines.
    fn meta_ok(pass: &Pass, sid: &str) -> bool {
        let remote = pass.remote.get(sid);
        let has_own_segments = remote.is_some_and(|r| r.segments.contains_key(&pass.machine));
        remote.is_some_and(|r| r.metas.contains_key(&pass.machine)) || !has_own_segments
    }

    /// [`Self::push_file`], plus the length of the file as the push saw it
    /// (`None` when it does not exist). Lines past that length are not on
    /// the server, whatever the outcome.
    async fn push_file_seen(
        &self,
        pass: &mut Pass,
        file: &Path,
    ) -> Result<(PushOutcome, Option<u64>), IdeError> {
        let Some(sid) = session_id_of(file).filter(|s| is_session_id(s)).map(str::to_string)
        else {
            return Ok((PushOutcome::Skipped, None));
        };
        let meta = match tokio::fs::metadata(file).await {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(target: TARGET, %sid, "skip push: transcript is gone");
                return Ok((PushOutcome::Skipped, None));
            }
            Err(e) => return Err(io_err("stat", file, e)),
        };
        let seen = Some(meta.len());
        let mtime = meta.modified().map_err(|e| io_err("mtime", file, e))?;
        if is_older_than(mtime, MAX_SESSION_AGE) {
            tracing::debug!(target: TARGET, %sid, "skip push: not modified in 30 days");
            return Ok((PushOutcome::Skipped, seen));
        }
        let mut st = pass.state.sessions.get(&sid).cloned().unwrap_or_default();
        if st.pushed_lines > 0 && st.pushed_bytes == Some(meta.len()) {
            self.ensure_remote(pass, &sid).await?;
            if Self::meta_ok(pass, &sid) {
                return Ok((PushOutcome::UpToDate, seen));
            }
        }

        // Local reasons to skip come before any request: listing a session
        // that will not go up would cost a round trip for nothing.
        let skip_mark = SkipMark { len: meta.len(), mtime };
        let bytes = tokio::fs::read(file).await.map_err(|e| io_err("read", file, e))?;
        let seen = Some(bytes.len() as u64);
        let ends = line_ends(&bytes);
        let Some(cwd) = first_cwd(&bytes, &ends) else {
            tracing::debug!(target: TARGET, %sid, "skip push: no cwd in the transcript yet");
            pass.skipped.insert(file.to_path_buf(), skip_mark);
            return Ok((PushOutcome::Skipped, seen));
        };
        let platform = self.opts.fs.platform();
        if self.is_private_cwd(&cwd, platform) {
            tracing::debug!(target: TARGET, %sid, %cwd, "skip push: cwd is inside ~/.claude or the Mnemos data dir");
            pass.skipped.insert(file.to_path_buf(), skip_mark);
            return Ok((PushOutcome::Skipped, seen));
        }
        self.ensure_registry(pass).await?;
        // Per-project sync opt-out: a session in such a project never goes up.
        if pass.registry.as_ref().is_some_and(|r| r.is_sync_disabled_cwd(&cwd, platform)) {
            tracing::debug!(target: TARGET, %sid, "skip push: project sync is disabled on this machine");
            pass.skipped.insert(file.to_path_buf(), skip_mark);
            return Ok((PushOutcome::Skipped, seen));
        }
        let located = pass.registry.as_ref().and_then(|r| r.locate(&cwd, platform));
        let Some((anchor, root, rel)) = located else {
            tracing::debug!(target: TARGET, %sid, %cwd, "skip push: cwd is not an absolute path");
            pass.skipped.insert(file.to_path_buf(), skip_mark);
            return Ok((PushOutcome::Skipped, seen));
        };
        pass.skipped.remove(file);
        let code_root = match anchor {
            Anchor::CodeRoot => registry::path_file_name(&root, platform),
            _ => None,
        };
        let origin_home = code_root.as_ref().and_then(|_| {
            let home = pass.registry.as_ref()?.home.clone()?;
            relative_components(&root, &home, platform).map(|_| home)
        });
        let project = match &anchor {
            Anchor::Project(p) => Some(p.clone()),
            _ => None,
        };

        self.ensure_remote(pass, &sid).await?;
        let remote = pass.remote.get(&sid).cloned().unwrap_or_default();
        let meta_ok = Self::meta_ok(pass, &sid);
        let from = resume_line(&bytes, &ends, &st);
        if from != st.pushed_lines {
            tracing::debug!(
                target: TARGET,
                %sid,
                counted = st.pushed_lines,
                resume = from,
                "transcript was rewritten; pushing from the last line the server has"
            );
            st.pushed_lines = from;
            st.pushed_bytes = None;
        }
        st.local_path = Some(file.to_string_lossy().into_owned());
        if from == ends.len() && meta_ok {
            st.pushed_bytes = Some(ends.last().copied().unwrap_or(0) as u64);
            st.tail = tail_keys(&bytes, &ends, from);
            // Only a partial last line is left (a write in progress, or one
            // a crash cut off). A cheap sweep passes over it until the file
            // changes again.
            if st.pushed_bytes != Some(meta.len()) {
                pass.skipped.insert(file.to_path_buf(), skip_mark);
            }
            // A trailing partial line keeps the file longer than
            // `pushed_bytes`, so this path repeats while Claude writes; only
            // touch the state file when something actually moved.
            if pass.state.sessions.get(&sid) != Some(&st) {
                pass.state.sessions.insert(sid.clone(), st);
                self.save_state(&pass.state).await?;
            }
            return Ok((PushOutcome::UpToDate, seen));
        }

        // Meta first: segments are unusable to other machines without it,
        // and it costs nothing to rewrite.
        let session_meta = SessionMeta {
            session_id: sid.clone(),
            project_id: project.as_ref().map(|p| p.id.clone()),
            git_repo: project.as_ref().and_then(|p| p.git_repo.clone()),
            project_name: project.as_ref().map(|p| p.name.clone()),
            rel_cwd: rel.join("/"),
            origin_root: root,
            origin_platform: platform.to_string(),
            updated_at: Some(registry::format_timestamp(Utc::now())),
            source_mtime: Some(registry::format_timestamp(chrono::DateTime::<Utc>::from(mtime))),
            code_root,
            origin_home,
            home_rel: matches!(anchor, Anchor::Home),
            abs_cwd: matches!(anchor, Anchor::Absolute),
        };
        self.blobs
            .put(&meta_path(&sid, &pass.machine), serde_json::to_vec(&session_meta)?)
            .await?;
        pass.remote
            .entry(sid.clone())
            .or_default()
            .metas
            .insert(pass.machine.clone(), registry::format_timestamp(Utc::now()));

        // Never trust local state for the sequence number: a lost state file
        // would otherwise reuse (and overwrite) segments others applied.
        let mut seq = remote
            .segments
            .get(&pass.machine)
            .and_then(|m| m.keys().next_back())
            .map_or(1, |s| s + 1);
        let mut pushed_any = false;
        let mut failed = None;
        // `(first line, end line, as a stub)`. A run is sent as it is; a
        // single line the server refused goes again as a stub.
        let mut chunks: VecDeque<(usize, usize, bool)> =
            chunk_lines(&ends, from, self.opts.max_segment_bytes)
                .into_iter()
                .map(|(a, b)| (a, b, false))
                .collect();
        while let Some((a, b, as_stub)) = chunks.pop_front() {
            let start = if a == 0 { 0 } else { ends[a - 1] };
            let lines = &bytes[start..ends[b - 1]];
            let stub: Vec<u8>;
            let raw: &[u8] = if !as_stub {
                lines
            } else if let Some(s) = stub_line(lines) {
                stub = s;
                &stub
            } else {
                tracing::warn!(
                    target: TARGET,
                    %sid,
                    line = a + 1,
                    bytes = lines.len(),
                    "the server refused a transcript line with no long text to cut; skipping it"
                );
                st.pushed_lines = b;
                st.pushed_bytes = Some(ends[b - 1] as u64);
                continue;
            };
            let gz = match gzip(raw) {
                Ok(gz) => gz,
                Err(e) => {
                    failed = Some(io_err("gzip", file, e));
                    break;
                }
            };
            // `Some(why)`: the server will never take these bytes.
            let refused = if gz.len() > MAX_BLOB_BYTES {
                Some(format!("{} bytes gzipped, over the {MAX_BLOB_BYTES} any upload takes", gz.len()))
            } else {
                match self.blobs.put(&segment_path(&sid, &pass.machine, seq), gz).await {
                    Ok(()) => {
                        pass.remote
                            .entry(sid.clone())
                            .or_default()
                            .segments
                            .entry(pass.machine.clone())
                            .or_default()
                            .insert(seq, registry::format_timestamp(Utc::now()));
                        st.own_seq = st.own_seq.max(seq);
                        seq += 1;
                        pushed_any = true;
                        pass.stats.pushed_segments += 1;
                        pass.stats.pushed_lines += b - a;
                        None
                    }
                    Err(e) if refused_for_content(&e) => Some(e.to_string()),
                    Err(e) => {
                        failed = Some(e);
                        break;
                    }
                }
            };
            if let Some(why) = refused {
                if b - a > 1 {
                    // Some line in the run is one the server will not take.
                    // Send the run a line per segment, so only that line is
                    // left to deal with.
                    tracing::debug!(target: TARGET, %sid, lines = b - a, error = %why, "segment refused; sending its lines one by one");
                    for i in (a..b).rev() {
                        chunks.push_front((i, i + 1, false));
                    }
                    continue;
                }
                if !as_stub {
                    // The server will never take this line (a 413 from the
                    // proxy, most likely), and retrying it on every push
                    // would wedge the session behind it. Leaving it out
                    // would cut the other machine's resumed history short
                    // at the next line, so a stub goes in its place.
                    tracing::warn!(
                        target: TARGET,
                        %sid,
                        line = a + 1,
                        bytes = raw.len(),
                        error = %why,
                        "the server refused a transcript line; sending a stub in its place"
                    );
                    chunks.push_front((a, b, true));
                    continue;
                }
                tracing::warn!(
                    target: TARGET,
                    %sid,
                    line = a + 1,
                    bytes = raw.len(),
                    error = %why,
                    "the server refused a transcript line's stub too; skipping it"
                );
            }
            st.pushed_lines = b;
            st.pushed_bytes = Some(ends[b - 1] as u64);
        }
        st.tail = tail_keys(&bytes, &ends, st.pushed_lines);
        // Saved once per session, and also after a failed upload, so the
        // segments that did land are not sent again next time.
        pass.state.sessions.insert(sid.clone(), st);
        self.save_state(&pass.state).await?;
        if pushed_any {
            pass.stats.pushed_sessions += 1;
            tracing::debug!(target: TARGET, %sid, lines = ends.len() - from, "pushed transcript");
        }
        match failed {
            Some(e) => Err(e),
            None => Ok((PushOutcome::Pushed, seen)),
        }
    }

    /// [`Self::pull`]'s pass.
    async fn pull_pass(&self, pass: &mut Pass) -> Result<(), IdeError> {
        self.full_listing(pass).await?;
        let sids: Vec<String> = pass.remote.keys().cloned().collect();
        self.pull_sessions(pass, sids).await
    }

    /// [`Self::pull_indexed`]'s pass.
    async fn pull_indexed_pass(&self, pass: &mut Pass, index: &Value) -> Result<(), IdeError> {
        let targets = index_targets(index, &pass.machine, &pass.state, &pass.tried);
        if targets.is_empty() {
            tracing::debug!(target: TARGET, "index pull: nothing new from other machines");
            return Ok(());
        }
        // A session open or being written here takes no other machine's
        // lines, whatever they are. It waits without a request: left open
        // on one machine and continued on the other, it would otherwise be
        // listed again at every poll for as long as it stays open.
        let open = self.open_here().await?;
        let mut ready = Vec::new();
        for sid in targets {
            if open.contains(&sid) || Self::live_here(pass, &sid).await {
                tracing::debug!(target: TARGET, %sid, "index pull: session is in use here; waiting");
                pass.retry = true;
            } else {
                ready.push(sid);
            }
        }
        // Many at once (say, the first poll after a restart, with every
        // session this machine cannot apply named again): one listing of
        // the whole store costs less than one per session.
        if ready.len() > PER_SESSION_LISTING_MAX {
            self.full_listing(pass).await?;
        }
        let mut first_err = None;
        let mut listed = Vec::new();
        for sid in ready {
            match self.ensure_remote(pass, &sid).await {
                Ok(()) => listed.push(sid),
                Err(e) => {
                    tracing::warn!(target: TARGET, %sid, error = %e, "transcript listing failed");
                    pass.retry |= is_transient(&e);
                    first_err.get_or_insert(e);
                }
            }
        }
        let pulled = self.pull_sessions(pass, listed).await;
        match (first_err, pulled) {
            (Some(e), _) | (None, Err(e)) => Err(e),
            (None, Ok(())) => Ok(()),
        }
    }

    /// Other machines' segments of `sid` in `pass.remote` that this machine
    /// has not applied.
    fn pending_segments(pass: &Pass, sid: &str) -> Vec<Pending> {
        let Some(remote) = pass.remote.get(sid) else {
            return Vec::new();
        };
        let applied = pass.state.sessions.get(sid).map(|s| &s.applied_segments);
        remote
            .segments
            .iter()
            .filter(|(machine, _)| **machine != pass.machine)
            .flat_map(|(machine, seqs)| {
                seqs.iter().map(move |(seq, lm)| Pending {
                    machine: machine.clone(),
                    seq: *seq,
                    last_modified: lm.clone(),
                })
            })
            .filter(|p| !applied.is_some_and(|a| a.contains(&segment_name(&p.machine, p.seq))))
            .collect()
    }

    /// Bring in the pending segments of `sids` (already in `pass.remote`).
    /// A session that cannot be applied here, or failed for a reason that
    /// would only repeat, is remembered in `pass.tried`, so an index poll
    /// does not list it again until its machine uploads more. One left
    /// waiting (open or being written here) or failed for a reason that may
    /// pass sets `pass.retry`.
    async fn pull_sessions(&self, pass: &mut Pass, sids: Vec<String>) -> Result<(), IdeError> {
        let work: Vec<(String, Vec<Pending>)> = sids
            .into_iter()
            .filter_map(|sid| {
                let pending = Self::pending_segments(pass, &sid);
                (!pending.is_empty()).then_some((sid, pending))
            })
            .collect();
        if work.is_empty() {
            tracing::debug!(target: TARGET, "pull: no new segments from other machines");
            return Ok(());
        }
        self.ensure_registry(pass).await?;
        let dir = self.claude_projects_dir.clone();
        let local = tokio::task::spawn_blocking(move || scan_local_sessions(&dir))
            .await
            .map_err(join_err)?;
        let mut by_sid: HashMap<String, Vec<LocalSession>> = HashMap::new();
        for s in local {
            by_sid.entry(s.sid.clone()).or_default().push(s);
        }

        let mut first_err = None;
        for (sid, pending) in work {
            // The highest pending number per machine, for `tried`.
            let mut newest: BTreeMap<String, u64> = BTreeMap::new();
            for p in &pending {
                let n = newest.entry(p.machine.clone()).or_default();
                *n = (*n).max(p.seq);
            }
            let copies = by_sid.remove(&sid).unwrap_or_default();
            // A session nobody touched for a month is not worth bringing to
            // a machine that never had it (a new machine would otherwise take
            // in the whole history). Claude Code deletes transcripts this old
            // anyway.
            let outcome = if copies.is_empty()
                && newest_segment_is_older_than(&pass.remote, &sid, MAX_SESSION_AGE)
            {
                tracing::debug!(target: TARGET, %sid, "skip pull: session untouched for 30 days and not here");
                Ok(PullOutcome::Unavailable)
            } else {
                self.pull_session(pass, &sid, pending, copies).await
            };
            match outcome {
                Ok(PullOutcome::Waiting) => pass.retry = true,
                Ok(PullOutcome::Applied | PullOutcome::Unavailable) => {
                    mark_tried(&mut pass.tried, &sid, newest);
                }
                Err(e) => {
                    tracing::warn!(target: TARGET, %sid, error = %e, "transcript pull failed");
                    // Offline or a 5xx: the next poll tries again. Anything
                    // else (a segment that does not unpack, a merge the disk
                    // refuses) would only fail the same way, after
                    // downloading every pending segment again; it waits for
                    // its machine to upload more, or the hourly full pull.
                    if is_transient(&e) {
                        pass.retry = true;
                    } else {
                        mark_tried(&mut pass.tried, &sid, newest);
                    }
                    first_err.get_or_insert(e);
                }
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    async fn pull_session(
        &self,
        pass: &mut Pass,
        sid: &str,
        pending: Vec<Pending>,
        copies: Vec<LocalSession>,
    ) -> Result<PullOutcome, IdeError> {
        let remote = pass.remote.get(sid).cloned().unwrap_or_default();
        let machines: BTreeSet<String> = pending.iter().map(|p| p.machine.clone()).collect();
        let mut metas: BTreeMap<String, SessionMeta> = BTreeMap::new();
        for machine in &machines {
            let Some(listed_at) = remote.metas.get(machine) else {
                tracing::debug!(target: TARGET, %sid, %machine, "skip segments: their machine has no meta yet");
                continue;
            };
            let key = (sid.to_string(), machine.clone());
            if let Some((at, meta)) = pass.metas.get(&key) {
                if at == listed_at {
                    metas.insert(machine.clone(), meta.clone());
                    continue;
                }
            }
            let Some(bytes) = self.blobs.get(&meta_path(sid, machine)).await? else {
                tracing::debug!(target: TARGET, %sid, %machine, "skip segments: meta vanished");
                continue;
            };
            match serde_json::from_slice::<SessionMeta>(&bytes) {
                Ok(m) if !m.is_well_formed() => {
                    tracing::debug!(target: TARGET, %sid, %machine, "skip segments: meta names a path with `..` or no root");
                }
                Ok(m) => {
                    pass.metas.insert(key, (listed_at.clone(), m.clone()));
                    metas.insert(machine.clone(), m);
                }
                Err(e) => {
                    tracing::debug!(target: TARGET, %sid, %machine, error = %e, "skip segments: meta is unreadable");
                }
            }
        }
        let pending: Vec<Pending> =
            pending.into_iter().filter(|p| metas.contains_key(&p.machine)).collect();
        if pending.is_empty() {
            return Ok(PullOutcome::Unavailable);
        }

        let registry = pass.registry.as_ref().expect("registry loaded before pulling");
        let platform = self.opts.fs.platform();
        // Per-project sync opt-out: nothing comes down for such a project.
        if metas.values().any(|m| registry.is_sync_disabled_meta(m)) {
            tracing::debug!(target: TARGET, %sid, "skip pull: project sync is disabled on this machine");
            return Ok(PullOutcome::Unavailable);
        }
        // A project known here but with no checkout yet waits for one rather
        // than landing somewhere weaker.
        let found_without_root = metas
            .values()
            .any(|m| registry.find(m).is_some_and(|p| p.root.is_none()));
        let has_project_root = metas
            .values()
            .any(|m| registry.find(m).is_some_and(|p| p.root.is_some()));
        // Otherwise the best place any machine's meta gives: project, code
        // folder of the same name, home, then the unchanged cwd. Each meta is
        // tried on its own, so one with no place here never hides another's.
        let target_root = if found_without_root && !has_project_root {
            None
        } else {
            metas
                .values()
                .filter_map(|m| registry.local_root(m, platform).map(|place| (place, m)))
                .min_by_key(|((rank, _, _), _)| *rank)
                .map(|((_, root, root_platform), m)| (root, root_platform, m.clone()))
        };
        let Some((local_root, root_platform, lead_meta)) = target_root else {
            if found_without_root {
                tracing::debug!(target: TARGET, %sid, "skip pull: project has no checkout on this machine yet");
            } else {
                tracing::debug!(target: TARGET, %sid, "skip pull: no matching project, code folder or home on this machine");
            }
            return Ok(PullOutcome::Unavailable);
        };
        // Each machine's lines move onto where its own meta lives here; a
        // machine that wrote from a code folder while another wrote from the
        // project keeps its cwds under the code folder.
        let to_roots: BTreeMap<String, (String, &'static str)> = metas
            .iter()
            .map(|(m, meta)| {
                let root = registry
                    .local_root(meta, platform)
                    .map(|(_, root, p)| (root, p))
                    .unwrap_or_else(|| (local_root.clone(), root_platform));
                (m.clone(), root)
            })
            .collect();

        // Never a second copy: an existing file for this session wins over
        // the slug the other machine's cwd maps to.
        let rel: Vec<String> = lead_meta
            .rel_cwd
            .split('/')
            .filter(|s| !s.is_empty() && *s != ".")
            .map(str::to_string)
            .collect();
        let session_cwd = join_components(&local_root, &rel, root_platform);
        // A session tied to a code folder or home can still map into an
        // opted-out checkout here.
        if registry.is_sync_disabled_cwd(&session_cwd, root_platform) {
            tracing::debug!(target: TARGET, %sid, cwd = %session_cwd, "skip pull: cwd maps into a project whose sync is disabled here");
            return Ok(PullOutcome::Unavailable);
        }
        if self.is_private_cwd(&session_cwd, root_platform) || self.is_private_cwd(&session_cwd, platform) {
            tracing::debug!(target: TARGET, %sid, cwd = %session_cwd, "skip pull: cwd maps inside ~/.claude or the Mnemos data dir here");
            return Ok(PullOutcome::Unavailable);
        }
        let Some(slug) = registry::claude_project_slug(Path::new(&session_cwd)) else {
            tracing::debug!(target: TARGET, %sid, cwd = %session_cwd, "skip pull: cwd too long for a Claude project dir");
            return Ok(PullOutcome::Unavailable);
        };
        let expected = self.claude_projects_dir.join(&slug).join(format!("{sid}.jsonl"));
        // With copies in several slug dirs, the newest is the one Claude Code
        // writes (and the only one the push sends up).
        let existing = copies.iter().max_by_key(|c| c.mtime);
        let target = existing.map_or(expected.clone(), |c| c.path.clone());

        if self.is_open_here(sid).await? {
            tracing::debug!(target: TARGET, %sid, "skip pull: a Claude Code process here has the session open");
            return Ok(PullOutcome::Waiting);
        }
        let mut pushed_len = None;
        let mut new_file = true;
        if let Ok(meta) = tokio::fs::metadata(&target).await {
            new_file = false;
            if is_live(meta.modified().map_err(|e| io_err("mtime", &target, e))?) {
                tracing::debug!(target: TARGET, %sid, "skip pull: session is live on this machine");
                return Ok(PullOutcome::Waiting);
            }
            // Our own unpushed lines go up first; afterwards everything in the
            // file counts as pushed, so the merge must see exactly this file.
            let (_, seen) = self.push_file_seen(pass, &target).await?;
            pushed_len = seen;
        }

        let mut segments: Vec<PulledSegment> = Vec::new();
        for p in order_segments(pending) {
            let meta = &metas[&p.machine];
            let path = segment_path(sid, &p.machine, p.seq);
            let Some(gz) = self.blobs.get(&path).await? else {
                return Err(IdeError::Other(format!("segment {path} is listed but missing")));
            };
            let raw = gunzip(&gz).map_err(|e| IdeError::Other(format!("segment {path}: {e}")))?;
            segments.push(PulledSegment {
                name: segment_name(&p.machine, p.seq),
                raw,
                map: CwdMap {
                    from_root: meta.origin_root.clone(),
                    from_platform: meta.origin_platform(),
                    to_root: to_roots[&p.machine].0.clone(),
                    to_platform: to_roots[&p.machine].1,
                },
                source_mtime: meta
                    .source_mtime
                    .as_deref()
                    .and_then(registry::parse_timestamp)
                    .map(SystemTime::from),
            });
        }

        // A `claude --resume` may have started while the segments downloaded.
        if self.is_open_here(sid).await? {
            tracing::debug!(target: TARGET, %sid, "skip pull: the session was opened while downloading");
            return Ok(PullOutcome::Waiting);
        }
        let (result, segments) = tokio::task::spawn_blocking({
            let target = target.clone();
            move || (merge_into_file(&target, &segments, pushed_len), segments)
        })
        .await
        .map_err(join_err)?;
        let result = match result {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                tracing::debug!(target: TARGET, %sid, "skip pull: transcript changed while merging");
                return Ok(PullOutcome::Waiting);
            }
            Err(e) => return Err(io_err("merge into", &target, e)),
        };

        let mut st = pass.state.sessions.get(sid).cloned().unwrap_or_default();
        st.applied_segments.extend(segments.iter().map(|s| s.name.clone()));
        if result.wrote {
            st.pulled_here |= new_file;
            if result.appended > 0 {
                // Segments are in upload-time order: the last is the newest.
                st.pulled_from = segments.last().map(|s| s.map.from_platform.to_string());
                st.pulled_bytes = Some(result.total_bytes);
                st.pulled_at_ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_millis() as u64);
            }
            st.pushed_lines = result.total_lines;
            st.pushed_bytes = Some(result.total_bytes);
            st.tail = result.tail;
            st.local_path = Some(target.to_string_lossy().into_owned());
        }
        pass.state.sessions.insert(sid.to_string(), st);
        self.save_state(&pass.state).await?;

        pass.stats.pulled_segments += segments.len();
        if result.appended > 0 {
            pass.stats.pulled_sessions += 1;
            pass.stats.pulled_lines += result.appended;
        }
        tracing::debug!(
            target: TARGET,
            %sid,
            segments = segments.len(),
            appended = result.appended,
            path = %target.display(),
            "merged transcript segments"
        );
        Ok(PullOutcome::Applied)
    }
}

/// Whether `cwd` is in a system temp folder (agent worktrees, throwaway sessions): those never
/// sync, in either direction, so a machine does not fill the other with junk.
fn is_temp_cwd(cwd: &str, platform: &str) -> bool {
    let roots: &[&str] = match platform {
        "win32" => &[],
        "darwin" => &["/tmp", "/private/tmp", "/var/folders", "/private/var/folders", "/var/tmp"],
        _ => &["/tmp", "/var/tmp"],
    };
    if roots.iter().any(|r| relative_components(cwd, r, platform).is_some()) {
        return true;
    }
    if platform == "win32" {
        let lower = cwd.replace('/', "\\").to_ascii_lowercase();
        return lower.contains("\\appdata\\local\\temp")
            || lower.starts_with("c:\\windows\\temp")
            || lower.starts_with("\\\\?\\c:\\windows\\temp");
    }
    false
}

#[cfg(test)]
mod tests {

    #[test]
    fn temp_folders_never_count_as_syncable() {
        assert!(is_temp_cwd("/tmp/x", "darwin"));
        assert!(is_temp_cwd("/private/tmp/claude-501/wt", "darwin"));
        assert!(is_temp_cwd("/var/folders/dl/abc/T/x", "darwin"));
        assert!(is_temp_cwd("/tmp", "linux"));
        assert!(is_temp_cwd(r"C:\Users\bob\AppData\Local\Temp\wt", "win32"));
        assert!(is_temp_cwd(r"C:\Windows\Temp\x", "win32"));
        assert!(!is_temp_cwd("/Users/alice/Desktop/x", "darwin"));
        assert!(!is_temp_cwd("/tmpfoo/x", "darwin"));
        assert!(!is_temp_cwd(r"C:\code\temp-notes", "win32"));
    }
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;
    use std::sync::Mutex as StdMutex;

    const SID: &str = "0b6f3c1e-8a3b-4f7e-9d2a-1c2d3e4f5a6b";
    const SID2: &str = "7f1d2c3b-0a9e-4d8c-b7a6-5f4e3d2c1b0a";
    const MAC_ROOT: &str = "/Users/alice/code/tcbv2";
    const WIN_ROOT: &str = r"C:\code\mnemos";

    // ── Fakes ───────────────────────────────────────────────────────────────

    #[derive(Default)]
    struct MemState {
        blobs: BTreeMap<String, (Vec<u8>, String)>,
        clock: i64,
        puts: usize,
        gets: usize,
        /// Refuse every PUT once this many have succeeded.
        fail_after: Option<usize>,
        /// Upload times count from this unix time instead of now.
        backdate: Option<i64>,
        /// Every listing's prefix, in order.
        lists: Vec<String>,
        /// Refuse a PUT of more than this many bytes with this status, as
        /// the proxy in front of the admin backend answers 413.
        refuse_over: Option<(usize, u16)>,
    }

    /// The blob store both "machines" share.
    #[derive(Default, Clone)]
    struct MemBlobs(Arc<StdMutex<MemState>>);

    impl MemBlobs {
        fn puts(&self) -> usize {
            self.0.lock().unwrap().puts
        }
        fn fail_after(&self, n: Option<usize>) {
            self.0.lock().unwrap().fail_after = n;
        }
        fn gets(&self) -> usize {
            self.0.lock().unwrap().gets
        }
        fn backdate(&self, unix: Option<i64>) {
            self.0.lock().unwrap().backdate = unix;
        }
        fn refuse_over(&self, limit: Option<(usize, u16)>) {
            self.0.lock().unwrap().refuse_over = limit;
        }
        /// The listings made since the last call.
        fn take_lists(&self) -> Vec<String> {
            std::mem::take(&mut self.0.lock().unwrap().lists)
        }
        fn paths(&self) -> Vec<String> {
            self.0.lock().unwrap().blobs.keys().cloned().collect()
        }
        fn raw(&self, path: &str) -> Option<Vec<u8>> {
            self.0.lock().unwrap().blobs.get(path).map(|(b, _)| b.clone())
        }
        fn segments_of(&self, machine: &str) -> Vec<String> {
            self.paths()
                .into_iter()
                .filter(|p| p.ends_with(SEGMENT_SUFFIX) && p.contains(&format!("/{machine}-")))
                .collect()
        }
        fn segment_text(&self, path: &str) -> String {
            String::from_utf8(gunzip(&self.raw(path).unwrap()).unwrap()).unwrap()
        }
    }

    impl BlobStore for MemBlobs {
        fn put<'a>(&'a self, path: &'a str, bytes: Vec<u8>) -> BoxFuture<'a, Result<(), IdeError>> {
            let mut s = self.0.lock().unwrap();
            if s.fail_after.is_some_and(|n| s.puts >= n) {
                return Box::pin(async { Err(IdeError::Other("injected upload failure".into())) });
            }
            if let Some((limit, status)) = s.refuse_over.filter(|(limit, _)| bytes.len() > *limit) {
                let status = reqwest::StatusCode::from_u16(status).unwrap();
                let body = format!("{} bytes is over {limit}", bytes.len());
                return Box::pin(async move { Err(IdeError::Http { status, body }) });
            }
            s.clock += 1;
            s.puts += 1;
            // Relative to now (the pull skips sessions untouched for 30
            // days), and strictly increasing within a test.
            let base = s.backdate.unwrap_or_else(|| Utc::now().timestamp());
            let lm = registry::format_timestamp(Utc.timestamp_opt(base + s.clock, 0).unwrap());
            s.blobs.insert(path.to_string(), (bytes, lm));
            Box::pin(async { Ok(()) })
        }
        fn get<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Option<Vec<u8>>, IdeError>> {
            self.0.lock().unwrap().gets += 1;
            let out = self.raw(path);
            Box::pin(async move { Ok(out) })
        }
        fn list<'a>(&'a self, prefix: &'a str) -> BoxFuture<'a, Result<Vec<BlobItem>, IdeError>> {
            self.0.lock().unwrap().lists.push(prefix.to_string());
            let items = self
                .0
                .lock()
                .unwrap()
                .blobs
                .iter()
                .filter(|(k, _)| k.starts_with(prefix))
                .map(|(k, (b, lm))| BlobItem {
                    path: k.clone(),
                    size: b.len() as u64,
                    etag: hex_hash(b),
                    last_modified: lm.clone(),
                })
                .collect();
            Box::pin(async move { Ok(items) })
        }
        fn delete<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), IdeError>> {
            self.0.lock().unwrap().blobs.remove(path);
            Box::pin(async { Ok(()) })
        }
    }

    fn hex_hash(b: &[u8]) -> String {
        line_hash(b).iter().map(|x| format!("{x:02x}")).collect()
    }

    /// A machine's view of its checkouts: exactly `dirs` exist.
    struct FakeFs {
        platform: &'static str,
        dirs: Vec<String>,
        home: Option<String>,
    }

    impl FsEnv for FakeFs {
        fn platform(&self) -> &'static str {
            self.platform
        }
        fn is_dir(&self, path: &Path) -> bool {
            let s = path.to_string_lossy();
            self.dirs.iter().any(|d| registry::same_path(d, &s, self.platform))
        }
        fn home(&self) -> Option<PathBuf> {
            self.home.as_ref().map(PathBuf::from)
        }
        fn git_remote(&self, _: &Path) -> Option<String> {
            None
        }
        fn git_root(&self, _: &Path) -> Option<PathBuf> {
            None
        }
        fn list_dirs(&self, _: &Path) -> Vec<PathBuf> {
            Vec::new()
        }
    }

    struct Machine {
        sync: TranscriptSync,
        claude: PathBuf,
        store: PathBuf,
        _tmp: tempfile::TempDir,
    }

    fn record(id: &str, name: &str, platform: &str, root: &str) -> Value {
        json!({
            "id": id,
            "name": name,
            "path": root,
            "paths": { platform: root },
            "gitRepo": "acme/TCB",
            "createdAt": "2026-09-01T00:00:00Z",
            "updatedAt": "2026-09-01T00:00:00Z"
        })
    }

    fn machine(blobs: &MemBlobs, platform: &'static str, root: &str, records: Value) -> Machine {
        machine_capped(blobs, platform, root, records, MAX_SEGMENT_RAW_BYTES)
    }

    fn machine_capped(
        blobs: &MemBlobs,
        platform: &'static str,
        root: &str,
        records: Value,
        max_segment_bytes: usize,
    ) -> Machine {
        machine_with(blobs, platform, &[root], records, max_segment_bytes)
    }

    /// A machine whose filesystem has exactly `dirs` (its checkouts).
    fn machine_with(
        blobs: &MemBlobs,
        platform: &'static str,
        dirs: &[&str],
        records: Value,
        max_segment_bytes: usize,
    ) -> Machine {
        machine_at_home(blobs, platform, None, dirs, records, max_segment_bytes)
    }

    /// [`machine_with`], with a home directory, so code folders under it
    /// that are among `dirs` count.
    fn machine_at_home(
        blobs: &MemBlobs,
        platform: &'static str,
        home: Option<&str>,
        dirs: &[&str],
        records: Value,
        max_segment_bytes: usize,
    ) -> Machine {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        let claude = tmp.path().join("claude").join("projects");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::create_dir_all(&claude).unwrap();
        let projects = store.join("projects.json");
        std::fs::write(&projects, serde_json::to_vec_pretty(&json!({ "projects": records })).unwrap())
            .unwrap();
        let opts = Options {
            fs: Arc::new(FakeFs {
                platform,
                dirs: dirs.iter().map(|d| d.to_string()).collect(),
                home: home.map(str::to_string),
            }),
            registry_files: Some((projects, store.join("workspace_state.json"))),
            max_segment_bytes,
        };
        let sync = TranscriptSync::with_parts(Arc::new(blobs.clone()), store.clone(), claude.clone(), opts);
        Machine { sync, claude, store, _tmp: tmp }
    }

    impl Machine {
        fn session_path(&self, cwd: &str, sid: &str) -> PathBuf {
            let slug = registry::claude_project_slug(Path::new(cwd)).unwrap();
            self.claude.join(slug).join(format!("{sid}.jsonl"))
        }
        fn write(&self, path: &Path, text: &str) {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        fn append(&self, path: &Path, text: &str) {
            let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
            f.write_all(text.as_bytes()).unwrap();
        }
        fn machine_id(&self) -> String {
            std::fs::read_to_string(self.store.join(MACHINE_ID_FILE)).unwrap()
        }
        fn copies_of(&self, sid: &str) -> Vec<PathBuf> {
            scan_local_sessions(&self.claude)
                .into_iter()
                .filter(|s| s.sid == sid)
                .map(|s| s.path)
                .collect()
        }
    }

    /// Make a file look untouched for `secs` seconds.
    fn age(path: &Path, secs: u64) {
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(secs))
            .unwrap();
    }

    fn user(uuid: &str, cwd: &str, text: &str) -> String {
        format!(
            "{}\n",
            json!({"parentUuid": null, "cwd": cwd, "sessionId": SID, "type": "user",
                   "message": {"role": "user", "content": text}, "uuid": uuid})
        )
    }

    fn marker(kind: &str, value: &str) -> String {
        format!("{}\n", json!({"type": kind, kind: value, "sessionId": SID}))
    }

    fn parsed(path: &Path) -> Vec<Value> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn uuids(path: &Path) -> Vec<String> {
        parsed(path)
            .iter()
            .filter_map(|v| v["uuid"].as_str().map(str::to_string))
            .collect()
    }

    // ── End to end ──────────────────────────────────────────────────────────

    /// S8: the per-project sync opt-out (`syncDisabledProjectIds` in
    /// `workspace_state.json`) covers transcripts, in both directions.
    #[tokio::test]
    async fn sync_disabled_project_sessions_are_neither_pushed_nor_pulled() {
        let blobs = MemBlobs::default();
        let mac = machine(&blobs, "darwin", MAC_ROOT, json!([record("3092ffc6", "tcbv2", "darwin", MAC_ROOT)]));
        let pc = machine(&blobs, "win32", WIN_ROOT, json!([record("project-1727", "mnemos", "win32", WIN_ROOT)]));
        let mac_cwd = "/Users/alice/code/tcbv2/app";
        let mac_file = mac.session_path(mac_cwd, SID);
        mac.write(&mac_file, &[user("u1", mac_cwd, "hello"), user("u2", mac_cwd, "hi")].concat());

        // Off on the Mac: nothing goes up.
        let disable = |m: &Machine, id: &str| {
            std::fs::write(
                m.store.join("workspace_state.json"),
                serde_json::to_vec(&json!({ "syncDisabledProjectIds": [id] })).unwrap(),
            )
            .unwrap();
        };
        disable(&mac, "3092ffc6");
        mac.sync.sweep_push().await.unwrap();
        assert_eq!(blobs.puts(), 0, "an opted-out project's session never leaves the machine");

        // A Mac with sync on, and the PC opted out: it goes up but never
        // comes down.
        let mac = machine(&blobs, "darwin", MAC_ROOT, json!([record("3092ffc6", "tcbv2", "darwin", MAC_ROOT)]));
        let mac_file = mac.session_path(mac_cwd, SID);
        mac.write(&mac_file, &[user("u1", mac_cwd, "hello"), user("u2", mac_cwd, "hi")].concat());
        mac.sync.sweep_push().await.unwrap();
        assert!(blobs.puts() > 0);
        disable(&pc, "project-1727");
        pc.sync.pull().await.unwrap();
        assert!(pc.copies_of(SID).is_empty(), "an opted-out project's session is not pulled");
    }

    /// Gates review finding 4: turning a project's sync off removes what
    /// this machine already uploaded of its sessions (at the next start), so
    /// other machines stop pulling them, and nothing goes up again.
    #[tokio::test]
    async fn opting_out_removes_this_machines_uploads_of_the_project() {
        let blobs = MemBlobs::default();
        let mac = machine(&blobs, "darwin", MAC_ROOT, json!([record("3092ffc6", "tcbv2", "darwin", MAC_ROOT)]));
        let mac_cwd = "/Users/alice/code/tcbv2/app";
        let mac_file = mac.session_path(mac_cwd, SID);
        mac.write(&mac_file, &[user("u1", mac_cwd, "hello"), user("u2", mac_cwd, "hi")].concat());
        mac.sync.sweep_push().await.unwrap();
        let mac_id = mac.machine_id();
        assert_eq!(blobs.segments_of(&mac_id).len(), 1);
        assert!(blobs.raw(&meta_path(SID, &mac_id)).is_some());
        // Another machine's upload of the same session is not this one's.
        let other = segment_path(SID, "pcother", 1);
        BlobStore::put(&blobs, &other, b"other machine".to_vec()).await.unwrap();

        std::fs::write(
            mac.store.join("workspace_state.json"),
            serde_json::to_vec(&json!({ "syncDisabledProjectIds": ["3092ffc6"] })).unwrap(),
        )
        .unwrap();
        let puts = blobs.puts();
        // The next start's sweep.
        mac.sync.sweep_push().await.unwrap();
        assert!(blobs.segments_of(&mac_id).is_empty(), "{:?}", blobs.paths());
        assert!(blobs.raw(&meta_path(SID, &mac_id)).is_none());
        assert!(blobs.raw(&other).is_some(), "another machine's copy stays");
        assert_eq!(blobs.puts(), puts, "nothing goes up again");
    }

    #[tokio::test]
    async fn two_machines_exchange_a_session_both_ways() {
        let blobs = MemBlobs::default();
        // Different ids on purpose: the PC finds the Mac's project by gitRepo.
        let mac = machine(&blobs, "darwin", MAC_ROOT, json!([record("3092ffc6", "tcbv2", "darwin", MAC_ROOT)]));
        let pc = machine(&blobs, "win32", WIN_ROOT, json!([record("project-1727", "mnemos", "win32", WIN_ROOT)]));

        let mac_cwd = "/Users/alice/code/tcbv2/app";
        let mac_file = mac.session_path(mac_cwd, SID);
        let original = [marker("mode", "normal"), user("u1", mac_cwd, "hello"), user("u2", mac_cwd, "hi")].concat();
        mac.write(&mac_file, &original);
        mac.sync.sweep_push().await.unwrap();

        let mac_id = mac.machine_id();
        let meta: SessionMeta =
            serde_json::from_slice(&blobs.raw(&meta_path(SID, &mac_id)).unwrap()).unwrap();
        assert_eq!(meta.project_id.as_deref(), Some("3092ffc6"));
        assert_eq!(meta.git_repo.as_deref(), Some("acme/TCB"));
        assert_eq!(meta.rel_cwd, "app");
        assert_eq!(meta.origin_root, MAC_ROOT);
        assert_eq!(meta.origin_platform, "darwin");
        assert_eq!(blobs.segments_of(&mac_id), vec![segment_path(SID, &mac_id, 1)]);

        pc.sync.pull().await.unwrap();
        let pc_file = pc.claude.join("C--code-mnemos-app").join(format!("{SID}.jsonl"));
        assert_eq!(pc.copies_of(SID), vec![pc_file.clone()]);
        // Byte-identical apart from the rewritten cwd.
        let expected = original.replace(
            "\"/Users/alice/code/tcbv2/app\"",
            &serde_json::to_string(r"C:\code\mnemos\app").unwrap(),
        );
        assert_eq!(std::fs::read_to_string(&pc_file).unwrap(), expected);

        // Resume on the PC, sync back to the Mac.
        let pc_cwd = r"C:\code\mnemos\app";
        pc.append(&pc_file, &[user("u3", pc_cwd, "continue on windows"), marker("mode", "normal"), user("u4", pc_cwd, "done")].concat());
        pc.sync.push_changed(vec![pc_file.clone()]).await.unwrap();
        let pc_id = pc.machine_id();
        let pc_segments = blobs.segments_of(&pc_id);
        assert_eq!(pc_segments.len(), 1);
        assert_eq!(
            blobs.segment_text(&pc_segments[0]).lines().count(),
            3,
            "the PC uploads only the lines it wrote, not what it pulled"
        );

        age(&mac_file, 600);
        mac.sync.pull().await.unwrap();
        let lines = parsed(&mac_file);
        assert_eq!(uuids(&mac_file), ["u1", "u2", "u3", "u4"]);
        assert_eq!(lines.len(), 6, "the repeated mode marker from a new stretch is kept");
        for l in &lines {
            if let Some(cwd) = l["cwd"].as_str() {
                assert_eq!(cwd, mac_cwd);
            }
        }

        // Settled: nothing more to push from either side, nothing to pull.
        let puts = blobs.puts();
        let mac_bytes = std::fs::read(&mac_file).unwrap();
        let pc_bytes = std::fs::read(&pc_file).unwrap();
        mac.sync.push_changed(vec![mac_file.clone()]).await.unwrap();
        age(&pc_file, 600);
        pc.sync.pull().await.unwrap();
        pc.sync.sweep_push().await.unwrap();
        age(&mac_file, 600);
        mac.sync.pull().await.unwrap();
        assert_eq!(blobs.puts(), puts);
        assert_eq!(std::fs::read(&mac_file).unwrap(), mac_bytes);
        assert_eq!(std::fs::read(&pc_file).unwrap(), pc_bytes);
    }

    #[tokio::test]
    async fn duplicates_are_dropped_by_uuid_and_an_existing_copy_is_reused() {
        let blobs = MemBlobs::default();
        let mac = machine(&blobs, "darwin", MAC_ROOT, json!([record("3092ffc6", "tcbv2", "darwin", MAC_ROOT)]));
        let pc = machine(&blobs, "win32", WIN_ROOT, json!([record("3092ffc6", "tcbv2", "win32", WIN_ROOT)]));

        let mac_cwd = "/Users/alice/code/tcbv2/app";
        let mac_file = mac.session_path(mac_cwd, SID);
        mac.write(&mac_file, &[user("u1", mac_cwd, "a"), user("u2", mac_cwd, "b")].concat());
        mac.sync.sweep_push().await.unwrap();

        // The PC already holds the session, under the root's slug dir (copied
        // by hand, say), with u1 in it.
        let pc_file = pc.session_path(WIN_ROOT, SID);
        pc.write(&pc_file, &user("u1", WIN_ROOT, "a"));
        age(&pc_file, 600);
        pc.sync.pull().await.unwrap();

        assert_eq!(pc.copies_of(SID), vec![pc_file.clone()], "no second copy under the app slug");
        assert_eq!(uuids(&pc_file), ["u1", "u2"]);
        // Its own unpushed line went up first.
        assert_eq!(blobs.segments_of(&pc.machine_id()).len(), 1);

        // The PC's segment holds u1, which the Mac already has.
        let before = std::fs::read(&mac_file).unwrap();
        age(&mac_file, 600);
        mac.sync.pull().await.unwrap();
        assert_eq!(std::fs::read(&mac_file).unwrap(), before);
    }

    #[tokio::test]
    async fn segments_stay_under_four_mib_raw() {
        let blobs = MemBlobs::default();
        let mac = machine(&blobs, "darwin", MAC_ROOT, json!([record("3092ffc6", "tcbv2", "darwin", MAC_ROOT)]));
        let pc = machine(&blobs, "win32", WIN_ROOT, json!([record("3092ffc6", "tcbv2", "win32", WIN_ROOT)]));

        let big = "x".repeat(100 * 1024);
        let text: String = (0..95).map(|i| user(&format!("u{i}"), MAC_ROOT, &big)).collect();
        assert!(text.len() > 2 * MAX_SEGMENT_RAW_BYTES);
        let mac_file = mac.session_path(MAC_ROOT, SID);
        mac.write(&mac_file, &text);
        mac.sync.sweep_push().await.unwrap();

        let segments = blobs.segments_of(&mac.machine_id());
        assert_eq!(segments.len(), 3);
        let mut total = 0;
        for s in &segments {
            let raw = blobs.segment_text(s);
            assert!(raw.len() <= MAX_SEGMENT_RAW_BYTES, "{s}: {} bytes", raw.len());
            assert!(raw.ends_with('\n'));
            assert!(blobs.raw(s).unwrap().len() < raw.len() / 10, "segments are gzipped");
            total += raw.lines().count();
        }
        assert_eq!(total, 95);

        pc.sync.pull().await.unwrap();
        let pc_file = pc.session_path(WIN_ROOT, SID);
        assert_eq!(uuids(&pc_file).len(), 95);
    }

    #[tokio::test]
    async fn a_partial_trailing_line_is_not_uploaded() {
        let blobs = MemBlobs::default();
        let mac = machine(&blobs, "darwin", MAC_ROOT, json!([record("3092ffc6", "tcbv2", "darwin", MAC_ROOT)]));
        let file = mac.session_path(MAC_ROOT, SID);
        let third = user("u3", MAC_ROOT, "third");
        let (head, tail) = third.split_at(20);
        mac.write(&file, &[user("u1", MAC_ROOT, "a"), user("u2", MAC_ROOT, "b"), head.to_string()].concat());
        mac.sync.push_changed(vec![file.clone()]).await.unwrap();

        let id = mac.machine_id();
        assert_eq!(blobs.segment_text(&segment_path(SID, &id, 1)).lines().count(), 2);

        mac.append(&file, tail);
        mac.sync.push_changed(vec![file.clone()]).await.unwrap();
        assert_eq!(blobs.segment_text(&segment_path(SID, &id, 2)), third);
    }

    #[tokio::test]
    async fn a_failed_upload_keeps_the_segments_that_landed() {
        let blobs = MemBlobs::default();
        // A cap below two lines: one line per segment.
        let mac = machine_capped(&blobs, "darwin", MAC_ROOT, json!([record("3092ffc6", "tcbv2", "darwin", MAC_ROOT)]), 200);
        let file = mac.session_path(MAC_ROOT, SID);
        let lines: Vec<String> = (0..6).map(|i| user(&format!("u{i}"), MAC_ROOT, "x")).collect();
        assert!(lines.iter().all(|l| l.len() <= 200 && l.len() * 2 > 200));
        mac.write(&file, &lines.concat());

        // Meta and two segments land, the third upload fails.
        blobs.fail_after(Some(3));
        assert!(mac.sync.push_changed(vec![file.clone()]).await.is_err());
        blobs.fail_after(None);
        mac.sync.push_changed(vec![file.clone()]).await.unwrap();

        let segments = blobs.segments_of(&mac.machine_id());
        assert_eq!(segments.len(), 6);
        let uploaded: String = segments.iter().map(|s| blobs.segment_text(s)).collect();
        assert_eq!(uploaded, lines.concat(), "every line exactly once, in order");
    }

    #[tokio::test]
    async fn lost_local_state_never_overwrites_segments() {
        let blobs = MemBlobs::default();
        let mac = machine(&blobs, "darwin", MAC_ROOT, json!([record("3092ffc6", "tcbv2", "darwin", MAC_ROOT)]));
        let pc = machine(&blobs, "win32", WIN_ROOT, json!([record("3092ffc6", "tcbv2", "win32", WIN_ROOT)]));

        let file = mac.session_path(MAC_ROOT, SID);
        mac.write(&file, &[marker("mode", "normal"), user("u1", MAC_ROOT, "a"), user("u2", MAC_ROOT, "b")].concat());
        mac.sync.sweep_push().await.unwrap();
        let id = mac.machine_id();
        let first = blobs.raw(&segment_path(SID, &id, 1)).unwrap();

        std::fs::remove_file(mac.store.join(STATE_FILE)).unwrap();
        mac.append(&file, &user("u3", MAC_ROOT, "c"));
        mac.sync.push_changed(vec![file.clone()]).await.unwrap();

        assert_eq!(blobs.raw(&segment_path(SID, &id, 1)).unwrap(), first, "segment 1 untouched");
        assert_eq!(blobs.segment_text(&segment_path(SID, &id, 2)).lines().count(), 4, "full re-upload as segment 2");

        pc.sync.pull().await.unwrap();
        let pc_file = pc.session_path(WIN_ROOT, SID);
        assert_eq!(uuids(&pc_file), ["u1", "u2", "u3"]);
        assert_eq!(parsed(&pc_file).len(), 4, "the re-sent mode marker is dropped too");
    }

    #[tokio::test]
    async fn a_live_session_is_left_alone_until_idle() {
        let blobs = MemBlobs::default();
        let mac = machine(&blobs, "darwin", MAC_ROOT, json!([record("3092ffc6", "tcbv2", "darwin", MAC_ROOT)]));
        let pc = machine(&blobs, "win32", WIN_ROOT, json!([record("3092ffc6", "tcbv2", "win32", WIN_ROOT)]));

        let mac_file = mac.session_path(MAC_ROOT, SID);
        mac.write(&mac_file, &[user("u1", MAC_ROOT, "a"), user("u2", MAC_ROOT, "b")].concat());
        mac.sync.sweep_push().await.unwrap();

        let pc_file = pc.session_path(WIN_ROOT, SID);
        pc.write(&pc_file, &user("u1", WIN_ROOT, "a"));
        pc.sync.pull().await.unwrap();
        assert_eq!(uuids(&pc_file), ["u1"], "written seconds ago: live, untouched");
        assert!(pc.sync.wants_index_retry(), "the index poll comes back for it");

        age(&pc_file, 120);
        pc.sync.pull().await.unwrap();
        assert_eq!(uuids(&pc_file), ["u1", "u2"]);
        assert!(!pc.sync.wants_index_retry());
    }

    #[tokio::test]
    async fn only_recent_sessions_are_pushed() {
        let blobs = MemBlobs::default();
        let mac = machine_at_home(
            &blobs,
            "darwin",
            Some("/Users/alice"),
            &[MAC_ROOT],
            json!([record("3092ffc6", "tcbv2", "darwin", MAC_ROOT)]),
            MAX_SEGMENT_RAW_BYTES,
        );
        let old = mac.session_path(MAC_ROOT, SID2);
        mac.write(&old, &user("u2", MAC_ROOT, "last month"));
        age(&old, 31 * 24 * 60 * 60);
        let desktop = "/Users/alice/Desktop/old";
        let old_desktop = mac.session_path(desktop, SID);
        mac.write(&old_desktop, &user("u1", desktop, "last month too"));
        age(&old_desktop, 31 * 24 * 60 * 60);

        mac.sync.sweep_push().await.unwrap();
        mac.sync.push_changed(vec![old, old_desktop]).await.unwrap();
        assert!(blobs.paths().is_empty(), "{:?}", blobs.paths());
    }

    #[tokio::test]
    async fn sessions_inside_claude_or_the_mnemos_data_dir_never_sync() {
        let blobs = MemBlobs::default();
        // Real temp paths, so the host's own path rules.
        let platform = registry::current_platform();
        let here = machine_at_home(&blobs, platform, None, &[], json!([]), MAX_SEGMENT_RAW_BYTES);
        let other = machine_at_home(&blobs, platform, None, &[], json!([]), MAX_SEGMENT_RAW_BYTES);

        // `claude` started in `~/.claude/skills` and in the data dir.
        let in_claude = here.claude.parent().unwrap().join("skills").to_string_lossy().into_owned();
        let in_data = here.store.parent().unwrap().join("logs").to_string_lossy().into_owned();
        let a = here.session_path(&in_claude, SID);
        let b = here.session_path(&in_data, SID2);
        here.write(&a, &user("u1", &in_claude, "config"));
        here.write(&b, &user("u2", &in_data, "state"));
        here.sync.sweep_push().await.unwrap();
        here.sync.push_changed(vec![a, b]).await.unwrap();
        assert!(blobs.paths().is_empty(), "{:?}", blobs.paths());

        // Nor is another machine's session written here when its cwd
        // lands in this machine's `~/.claude`.
        let sid3 = "3c2b1a09-8f7e-4d6c-a5b4-9e8d7c6b5a49";
        let into_here = here.claude.parent().unwrap().join("x").to_string_lossy().into_owned();
        let from_other = other.session_path(&into_here, sid3);
        other.write(&from_other, &user("v1", &into_here, "elsewhere there"));
        other.sync.sweep_push().await.unwrap();
        assert!(!blobs.segments_of(&other.machine_id()).is_empty());
        here.sync.pull().await.unwrap();
        assert!(here.copies_of(sid3).is_empty());
    }

    #[tokio::test]
    async fn a_session_without_a_local_checkout_waits() {
        let blobs = MemBlobs::default();
        let mac = machine(&blobs, "darwin", MAC_ROOT, json!([record("3092ffc6", "tcbv2", "darwin", MAC_ROOT)]));
        // The PC knows the project but has no checkout of it yet.
        let pc = machine(&blobs, "win32", r"C:\elsewhere", json!([record("3092ffc6", "tcbv2", "darwin", MAC_ROOT)]));

        mac.write(&mac.session_path(MAC_ROOT, SID), &user("u1", MAC_ROOT, "a"));
        mac.sync.sweep_push().await.unwrap();
        pc.sync.pull().await.unwrap();
        assert!(pc.copies_of(SID).is_empty());
        let state = std::fs::read_to_string(pc.store.join(STATE_FILE)).unwrap_or_default();
        assert!(!state.contains("jsonl.gz"), "nothing marked applied: {state}");
    }

    #[tokio::test]
    async fn a_session_in_a_code_folder_follows_the_folder_name() {
        let blobs = MemBlobs::default();
        let mac = machine_at_home(&blobs, "darwin", Some("/Users/alice"), &["/Users/alice/code"], json!([]), MAX_SEGMENT_RAW_BYTES);
        let pc = machine_at_home(&blobs, "win32", Some(r"C:\Users\alice"), &[r"C:\code"], json!([]), MAX_SEGMENT_RAW_BYTES);

        let top = "/Users/alice/code";
        let nested = "/Users/alice/code/scratch/x";
        let mac_top = mac.session_path(top, SID);
        let mac_nested = mac.session_path(nested, SID2);
        mac.write(&mac_top, &[user("u1", top, "hello"), user("u2", top, "hi")].concat());
        mac.write(&mac_nested, &user("n1", nested, "nested"));
        mac.sync.sweep_push().await.unwrap();

        let mac_id = mac.machine_id();
        let meta: SessionMeta = serde_json::from_slice(&blobs.raw(&meta_path(SID, &mac_id)).unwrap()).unwrap();
        assert_eq!(meta.project_id, None);
        assert_eq!(meta.git_repo, None);
        assert_eq!(meta.project_name, None);
        assert_eq!(meta.code_root.as_deref(), Some("code"));
        assert_eq!(meta.rel_cwd, "");
        assert_eq!(meta.origin_root, top);
        let meta: SessionMeta = serde_json::from_slice(&blobs.raw(&meta_path(SID2, &mac_id)).unwrap()).unwrap();
        assert_eq!(meta.rel_cwd, "scratch/x");

        pc.sync.pull().await.unwrap();
        let pc_top = pc.claude.join("C--code").join(format!("{SID}.jsonl"));
        assert_eq!(pc.copies_of(SID), vec![pc_top.clone()]);
        let cwds = |path: &Path| -> Vec<String> {
            parsed(path).iter().filter_map(|l| l["cwd"].as_str().map(str::to_string)).collect()
        };
        assert_eq!(cwds(&pc_top), [r"C:\code", r"C:\code"]);
        let pc_nested = pc.claude.join("C--code-scratch-x").join(format!("{SID2}.jsonl"));
        assert_eq!(pc.copies_of(SID2), vec![pc_nested.clone()]);
        assert_eq!(cwds(&pc_nested), [r"C:\code\scratch\x"]);

        // Continued on the PC, back to the Mac under its own code folder.
        pc.append(&pc_top, &user("u3", r"C:\code", "from windows"));
        pc.sync.push_changed(vec![pc_top.clone()]).await.unwrap();
        let pc_meta: SessionMeta =
            serde_json::from_slice(&blobs.raw(&meta_path(SID, &pc.machine_id())).unwrap()).unwrap();
        assert_eq!(pc_meta.code_root.as_deref(), Some("code"));
        assert_eq!(pc_meta.origin_root, r"C:\code");
        age(&mac_top, 600);
        mac.sync.pull().await.unwrap();
        assert_eq!(uuids(&mac_top), ["u1", "u2", "u3"]);
        assert_eq!(cwds(&mac_top), [top, top, top]);
    }

    #[tokio::test]
    async fn a_project_beats_the_code_folder_around_it() {
        let blobs = MemBlobs::default();
        let mac = machine_at_home(
            &blobs,
            "darwin",
            Some("/Users/alice"),
            &[MAC_ROOT, "/Users/alice/code"],
            json!([record("3092ffc6", "tcbv2", "darwin", MAC_ROOT)]),
            MAX_SEGMENT_RAW_BYTES,
        );
        let cwd = "/Users/alice/code/tcbv2/app";
        mac.write(&mac.session_path(cwd, SID), &user("u1", cwd, "a"));
        mac.sync.sweep_push().await.unwrap();
        let meta: SessionMeta =
            serde_json::from_slice(&blobs.raw(&meta_path(SID, &mac.machine_id())).unwrap()).unwrap();
        assert_eq!(meta.project_id.as_deref(), Some("3092ffc6"));
        assert_eq!(meta.code_root, None);
        assert_eq!(meta.origin_root, MAC_ROOT);
        assert_eq!(meta.rel_cwd, "app");
    }

    #[tokio::test]
    async fn a_code_folder_session_falls_back_to_home_without_that_folder_here() {
        let blobs = MemBlobs::default();
        let mac = machine_at_home(&blobs, "darwin", Some("/Users/alice"), &["/Users/alice/projects"], json!([]), MAX_SEGMENT_RAW_BYTES);
        // The PC has `C:\dev` but no `projects` folder anywhere.
        let pc = machine_at_home(&blobs, "win32", Some(r"C:\Users\bob"), &[r"C:\dev"], json!([]), MAX_SEGMENT_RAW_BYTES);

        let cwd = "/Users/alice/projects/foo";
        mac.write(&mac.session_path(cwd, SID), &user("u1", cwd, "a"));
        mac.sync.sweep_push().await.unwrap();
        let meta = meta_of(&blobs, SID, &mac);
        assert_eq!(meta.code_root.as_deref(), Some("projects"));
        assert_eq!(meta.origin_home.as_deref(), Some("/Users/alice"));
        pc.sync.pull().await.unwrap();
        let pc_file = pc.claude.join("C--Users-bob-projects-foo").join(format!("{SID}.jsonl"));
        assert_eq!(pc.copies_of(SID), vec![pc_file.clone()]);
        assert_eq!(cwds(&pc_file), [r"C:\Users\bob\projects\foo"]);
    }

    #[tokio::test]
    async fn a_drive_root_code_folder_session_keeps_its_cwd_without_that_folder_here() {
        let blobs = MemBlobs::default();
        let pc = machine_at_home(&blobs, "win32", Some(r"C:\Users\bob"), &[r"C:\src"], json!([]), MAX_SEGMENT_RAW_BYTES);
        let mac = machine_at_home(&blobs, "darwin", Some("/Users/alice"), &["/Users/alice/code"], json!([]), MAX_SEGMENT_RAW_BYTES);

        let cwd = r"C:\src\x";
        pc.write(&pc.session_path(cwd, SID), &user("u1", cwd, "a"));
        pc.sync.sweep_push().await.unwrap();
        let meta = meta_of(&blobs, SID, &pc);
        assert_eq!(meta.code_root.as_deref(), Some("src"));
        assert_eq!(meta.origin_home, None);
        mac.sync.pull().await.unwrap();
        let mac_file = mac.claude.join("C--src-x").join(format!("{SID}.jsonl"));
        assert_eq!(mac.copies_of(SID), vec![mac_file.clone()]);
        assert_eq!(cwds(&mac_file), [cwd]);
    }

    #[tokio::test]
    async fn an_absolute_meta_leads_where_a_home_meta_has_no_home() {
        let blobs = MemBlobs::default();
        let mac = machine_at_home(&blobs, "darwin", Some("/Users/alice"), &[], json!([]), MAX_SEGMENT_RAW_BYTES);
        // Two machines without a home: one sends the session as absolute,
        // the other has only the Mac's home meta and that one.
        let other = machine_at_home(&blobs, "linux", None, &[], json!([]), MAX_SEGMENT_RAW_BYTES);
        let reader = machine_at_home(&blobs, "linux", None, &[], json!([]), MAX_SEGMENT_RAW_BYTES);

        let cwd = "/Users/alice/Desktop/foo";
        mac.write(&mac.session_path(cwd, SID), &user("u1", cwd, "a"));
        mac.sync.sweep_push().await.unwrap();
        assert!(meta_of(&blobs, SID, &mac).home_rel);
        other.write(&other.session_path(cwd, SID), &user("o1", cwd, "b"));
        other.sync.sweep_push().await.unwrap();
        assert!(meta_of(&blobs, SID, &other).abs_cwd);

        reader.sync.pull().await.unwrap();
        let file = reader.claude.join("-Users-alice-Desktop-foo").join(format!("{SID}.jsonl"));
        assert_eq!(reader.copies_of(SID), vec![file.clone()]);
        let mut got = uuids(&file);
        got.sort();
        assert_eq!(got, ["o1", "u1"]);
    }

    #[tokio::test]
    async fn a_meta_with_dot_dot_is_never_written() {
        let blobs = MemBlobs::default();
        let mac = machine_at_home(&blobs, "darwin", Some("/Users/alice"), &[], json!([]), MAX_SEGMENT_RAW_BYTES);
        let pc = machine_at_home(&blobs, "win32", Some(r"C:\Users\bob"), &[], json!([]), MAX_SEGMENT_RAW_BYTES);
        let cwd = "/Users/alice/Desktop/foo";
        mac.write(&mac.session_path(cwd, SID), &user("u1", cwd, "a"));
        mac.sync.sweep_push().await.unwrap();
        let mut meta = meta_of(&blobs, SID, &mac);
        meta.rel_cwd = "../bob/.claude".into();
        blobs
            .put(&meta_path(SID, &mac.machine_id()), serde_json::to_vec(&meta).unwrap())
            .await
            .unwrap();
        pc.sync.pull().await.unwrap();
        assert!(pc.copies_of(SID).is_empty());
    }

    #[test]
    fn a_meta_naming_dot_dot_or_a_relative_root_is_not_well_formed() {
        let base = SessionMeta {
            session_id: SID.into(),
            project_id: None,
            git_repo: None,
            project_name: None,
            rel_cwd: "Desktop/foo".into(),
            origin_root: "/Users/alice".into(),
            origin_platform: "darwin".into(),
            updated_at: None,
            source_mtime: None,
            code_root: None,
            origin_home: None,
            home_rel: true,
            abs_cwd: false,
        };
        assert!(base.is_well_formed());
        let bad = [
            SessionMeta { rel_cwd: "../alice/.claude".into(), ..base.clone() },
            SessionMeta { rel_cwd: r"a\..\b".into(), ..base.clone() },
            SessionMeta { origin_root: "Users/alice".into(), ..base.clone() },
            SessionMeta { origin_root: String::new(), ..base.clone() },
            SessionMeta {
                home_rel: false,
                abs_cwd: true,
                rel_cwd: String::new(),
                origin_root: "/Users/alice/x/../.claude".into(),
                ..base.clone()
            },
            SessionMeta {
                home_rel: false,
                code_root: Some("..".into()),
                origin_root: "/Users/alice/code".into(),
                ..base.clone()
            },
            SessionMeta {
                home_rel: false,
                code_root: Some("code".into()),
                origin_root: "/Users/alice/code".into(),
                origin_home: Some("/Users/alice/..".into()),
                ..base.clone()
            },
        ];
        for m in bad {
            assert!(!m.is_well_formed(), "{m:?}");
        }
        assert_eq!(relative_components("/Users/alice/x/../.claude", "/Users/alice/x", "darwin"), None);
    }

    #[tokio::test]
    async fn a_pulled_session_is_marked_so_checkout_scans_skip_it() {
        let blobs = MemBlobs::default();
        let mac = machine_at_home(&blobs, "darwin", Some("/Users/alice"), &[], json!([]), MAX_SEGMENT_RAW_BYTES);
        let pc = machine_at_home(&blobs, "win32", Some(r"C:\Users\bob"), &[], json!([]), MAX_SEGMENT_RAW_BYTES);
        let cwd = "/Users/alice/Desktop/foo";
        mac.write(&mac.session_path(cwd, SID), &user("u1", cwd, "a"));
        let pc_cwd = r"C:\Users\bob\Documents";
        pc.write(&pc.session_path(pc_cwd, SID2), &user("v1", pc_cwd, "b"));
        mac.sync.sweep_push().await.unwrap();
        pc.sync.pull().await.unwrap();
        assert_eq!(pulled_session_ids(&pc.store), HashSet::from([SID.to_string()]));
        // Its own sessions are not marked, on either machine.
        mac.sync.pull().await.unwrap();
        assert!(!pulled_session_ids(&mac.store).contains(SID));
    }

    /// The length a pull left and the file's real length, for one session.
    fn origin_and_len(store: &Path, sid: &str) -> (Option<PulledOrigin>, u64) {
        let state: SyncState =
            serde_json::from_slice(&std::fs::read(store.join(STATE_FILE)).unwrap()).unwrap();
        let local = state.sessions[sid].local_path.clone().expect("pulled file path");
        let len = std::fs::metadata(local).unwrap().len();
        (pulled_origins(store).get(sid).cloned(), len)
    }

    #[tokio::test]
    async fn a_pull_records_which_platform_the_newest_turns_came_from() {
        let blobs = MemBlobs::default();
        let mac = machine_at_home(&blobs, "darwin", Some("/Users/alice"), &[], json!([]), MAX_SEGMENT_RAW_BYTES);
        let pc = machine_at_home(&blobs, "win32", Some(r"C:\Users\bob"), &[], json!([]), MAX_SEGMENT_RAW_BYTES);
        let cwd = "/Users/alice/projects/foo";
        mac.write(&mac.session_path(cwd, SID), &user("u1", cwd, "a"));
        mac.sync.sweep_push().await.unwrap();
        pc.sync.pull().await.unwrap();

        let (origin, len) = origin_and_len(&pc.store, SID);
        let origin = origin.expect("a pull that appended records its origin");
        assert_eq!(origin.platform, "darwin");
        assert_eq!(origin.bytes, len, "nothing ran on the PC since the pull");
        // Dated by when the pull ran here, not by the file's mtime (which a
        // merge sets to the newest line's time on the other machine).
        let at = origin.at.expect("a pull records when it ran");
        assert!(at.elapsed().unwrap_or_default() < Duration::from_secs(60));
        // The machine that wrote the session records nothing for it.
        assert!(!pulled_origins(&mac.store).contains_key(SID));
    }

    #[test]
    fn a_state_file_from_before_pull_origins_still_parses() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(STATE_FILE),
            r#"{"sessions":{"s1":{"pushedLines":3,"appliedSegments":["m-1"],"pulledHere":true},"s2":{"pushedLines":1}}}"#,
        )
        .unwrap();
        assert_eq!(pulled_session_ids(dir.path()), HashSet::from(["s1".to_string()]));
        assert!(pulled_origins(dir.path()).is_empty());
    }

    #[test]
    fn cached_pull_origins_follow_the_state_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(pulled_origins_cached(dir.path()).is_empty());
        let one = r#"{"sessions":{"s1":{"pushedLines":1,"pulledFrom":"win32","pulledBytes":10}}}"#;
        std::fs::write(dir.path().join(STATE_FILE), one).unwrap();
        assert_eq!(pulled_origins_cached(dir.path())["s1"].bytes, 10);
        // A later pull rewrites the file.
        let two = r#"{"sessions":{"s1":{"pushedLines":2,"pulledFrom":"win32","pulledBytes":2000}}}"#;
        std::fs::write(dir.path().join(STATE_FILE), two).unwrap();
        assert_eq!(pulled_origins_cached(dir.path())["s1"].bytes, 2000);
        // Another store dir is never served from the cache.
        let other = tempfile::tempdir().unwrap();
        assert!(pulled_origins_cached(other.path()).is_empty());
        std::fs::remove_file(dir.path().join(STATE_FILE)).unwrap();
        assert!(pulled_origins_cached(dir.path()).is_empty());
    }

    fn cwds(path: &Path) -> Vec<String> {
        parsed(path).iter().filter_map(|l| l["cwd"].as_str().map(str::to_string)).collect()
    }

    fn meta_of(blobs: &MemBlobs, sid: &str, m: &Machine) -> SessionMeta {
        serde_json::from_slice(&blobs.raw(&meta_path(sid, &m.machine_id())).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn a_desktop_session_follows_home_both_ways() {
        let blobs = MemBlobs::default();
        // No code folders anywhere: home is all that ties the machines.
        let mac = machine_at_home(&blobs, "darwin", Some("/Users/alice"), &[], json!([]), MAX_SEGMENT_RAW_BYTES);
        let pc = machine_at_home(&blobs, "win32", Some(r"C:\Users\bob"), &[], json!([]), MAX_SEGMENT_RAW_BYTES);
        let linux = machine_at_home(&blobs, "linux", Some("/home/x"), &[], json!([]), MAX_SEGMENT_RAW_BYTES);

        let mac_cwd = "/Users/alice/Desktop/foo";
        let mac_file = mac.session_path(mac_cwd, SID);
        mac.write(&mac_file, &[user("u1", mac_cwd, "hello"), user("u2", mac_cwd, "hi")].concat());
        mac.sync.sweep_push().await.unwrap();
        let meta = meta_of(&blobs, SID, &mac);
        assert!(meta.home_rel && !meta.abs_cwd);
        assert_eq!((meta.project_id, meta.git_repo, meta.project_name), (None, None, None));
        assert_eq!(meta.code_root, None);
        assert_eq!(meta.rel_cwd, "Desktop/foo");
        assert_eq!(meta.origin_root, "/Users/alice");

        pc.sync.pull().await.unwrap();
        let pc_file = pc.claude.join("C--Users-bob-Desktop-foo").join(format!("{SID}.jsonl"));
        assert_eq!(pc.copies_of(SID), vec![pc_file.clone()]);
        let pc_cwd = r"C:\Users\bob\Desktop\foo";
        assert_eq!(cwds(&pc_file), [pc_cwd, pc_cwd]);
        linux.sync.pull().await.unwrap();
        let linux_file = linux.claude.join("-home-x-Desktop-foo").join(format!("{SID}.jsonl"));
        assert_eq!(cwds(&linux_file), ["/home/x/Desktop/foo", "/home/x/Desktop/foo"]);

        // Continued on the PC, back to the Mac onto its own home.
        pc.append(&pc_file, &user("u3", pc_cwd, "from windows"));
        pc.sync.push_changed(vec![pc_file.clone()]).await.unwrap();
        let pc_meta = meta_of(&blobs, SID, &pc);
        assert!(pc_meta.home_rel);
        assert_eq!(pc_meta.origin_root, r"C:\Users\bob");
        assert_eq!(pc_meta.rel_cwd, "Desktop/foo");
        age(&mac_file, 600);
        mac.sync.pull().await.unwrap();
        assert_eq!(uuids(&mac_file), ["u1", "u2", "u3"]);
        assert_eq!(cwds(&mac_file), [mac_cwd, mac_cwd, mac_cwd]);
    }

    #[tokio::test]
    async fn home_itself_and_deep_folders_under_it_map_onto_the_other_home() {
        let blobs = MemBlobs::default();
        let mac = machine_at_home(&blobs, "darwin", Some("/Users/alice"), &[], json!([]), MAX_SEGMENT_RAW_BYTES);
        let pc = machine_at_home(&blobs, "win32", Some(r"C:\Users\bob"), &[], json!([]), MAX_SEGMENT_RAW_BYTES);

        let home = "/Users/alice";
        let deep = "/Users/alice/Documents/a/b";
        mac.write(&mac.session_path(home, SID), &user("u1", home, "at home"));
        mac.write(&mac.session_path(deep, SID2), &user("n1", deep, "deep"));
        mac.sync.sweep_push().await.unwrap();
        let meta = meta_of(&blobs, SID, &mac);
        assert!(meta.home_rel);
        assert_eq!(meta.rel_cwd, "");
        assert_eq!(meta_of(&blobs, SID2, &mac).rel_cwd, "Documents/a/b");

        pc.sync.pull().await.unwrap();
        let at_home = pc.claude.join("C--Users-bob").join(format!("{SID}.jsonl"));
        assert_eq!(pc.copies_of(SID), vec![at_home.clone()]);
        assert_eq!(cwds(&at_home), [r"C:\Users\bob"]);
        let nested = pc.claude.join("C--Users-bob-Documents-a-b").join(format!("{SID2}.jsonl"));
        assert_eq!(pc.copies_of(SID2), vec![nested.clone()]);
        assert_eq!(cwds(&nested), [r"C:\Users\bob\Documents\a\b"]);
    }

    #[tokio::test]
    async fn a_project_or_a_code_folder_beats_home() {
        let blobs = MemBlobs::default();
        let work = "/Users/alice/work/tcbv2";
        let mac = machine_at_home(
            &blobs,
            "darwin",
            Some("/Users/alice"),
            &[work, "/Users/alice/code"],
            json!([record("3092ffc6", "tcbv2", "darwin", work)]),
            MAX_SEGMENT_RAW_BYTES,
        );
        let in_project = "/Users/alice/work/tcbv2/app";
        let in_code = "/Users/alice/code/x";
        mac.write(&mac.session_path(in_project, SID), &user("u1", in_project, "a"));
        mac.write(&mac.session_path(in_code, SID2), &user("v1", in_code, "b"));
        mac.sync.sweep_push().await.unwrap();

        let meta = meta_of(&blobs, SID, &mac);
        assert_eq!(meta.project_id.as_deref(), Some("3092ffc6"));
        assert!(!meta.home_rel && !meta.abs_cwd);
        assert_eq!((meta.origin_root.as_str(), meta.rel_cwd.as_str()), (work, "app"));
        let meta = meta_of(&blobs, SID2, &mac);
        assert_eq!(meta.code_root.as_deref(), Some("code"));
        assert!(!meta.home_rel && !meta.abs_cwd);
        assert_eq!((meta.origin_root.as_str(), meta.rel_cwd.as_str()), ("/Users/alice/code", "x"));
    }

    #[tokio::test]
    async fn a_session_outside_home_keeps_its_cwd_unchanged() {
        let blobs = MemBlobs::default();
        let mac = machine_at_home(&blobs, "darwin", Some("/Users/alice"), &[], json!([]), MAX_SEGMENT_RAW_BYTES);
        let pc = machine_at_home(&blobs, "win32", Some(r"C:\Users\bob"), &[], json!([]), MAX_SEGMENT_RAW_BYTES);

        let tmp = "/tmp/scratch";
        let mac_file = mac.session_path(tmp, SID);
        mac.write(&mac_file, &[user("u1", tmp, "a"), user("u2", "/tmp/scratch/sub", "b")].concat());
        mac.sync.sweep_push().await.unwrap();
        let meta = meta_of(&blobs, SID, &mac);
        assert!(meta.abs_cwd && !meta.home_rel);
        assert_eq!(meta.code_root, None);
        assert_eq!((meta.origin_root.as_str(), meta.rel_cwd.as_str()), (tmp, ""));

        // Listed and readable on the PC under that cwd's slug, untouched.
        pc.sync.pull().await.unwrap();
        let pc_file = pc.claude.join("-tmp-scratch").join(format!("{SID}.jsonl"));
        assert_eq!(pc.copies_of(SID), vec![pc_file.clone()]);
        assert_eq!(std::fs::read(&pc_file).unwrap(), std::fs::read(&mac_file).unwrap());

        // Continued there and back: still the same cwd on both sides.
        pc.append(&pc_file, &user("u3", tmp, "c"));
        pc.sync.push_changed(vec![pc_file.clone()]).await.unwrap();
        let pc_meta = meta_of(&blobs, SID, &pc);
        assert!(pc_meta.abs_cwd);
        assert_eq!(pc_meta.origin_root, tmp);
        age(&mac_file, 600);
        mac.sync.pull().await.unwrap();
        assert_eq!(uuids(&mac_file), ["u1", "u2", "u3"]);
        assert_eq!(cwds(&mac_file), [tmp, "/tmp/scratch/sub", tmp]);

        // A PC drive outside home reaches the Mac the same way.
        let d = r"D:\work";
        pc.write(&pc.session_path(d, SID2), &user("w1", d, "d"));
        pc.sync.sweep_push().await.unwrap();
        mac.sync.pull().await.unwrap();
        let mac_d = mac.claude.join("D--work").join(format!("{SID2}.jsonl"));
        assert_eq!(mac.copies_of(SID2), vec![mac_d.clone()]);
        assert_eq!(cwds(&mac_d), [d]);
    }

    #[tokio::test]
    async fn a_home_session_waits_on_a_machine_without_a_home() {
        let blobs = MemBlobs::default();
        let mac = machine_at_home(&blobs, "darwin", Some("/Users/alice"), &[], json!([]), MAX_SEGMENT_RAW_BYTES);
        let pc = machine_at_home(&blobs, "win32", None, &[], json!([]), MAX_SEGMENT_RAW_BYTES);
        let cwd = "/Users/alice/Desktop";
        mac.write(&mac.session_path(cwd, SID), &user("u1", cwd, "a"));
        mac.sync.sweep_push().await.unwrap();
        pc.sync.pull().await.unwrap();
        assert!(pc.copies_of(SID).is_empty());
        let state = std::fs::read_to_string(pc.store.join(STATE_FILE)).unwrap_or_default();
        assert!(!state.contains("jsonl.gz"), "nothing marked applied: {state}");
    }

    #[test]
    fn a_meta_without_a_code_folder_still_parses_and_old_readers_find_no_project() {
        let old = json!({
            "sessionId": SID,
            "projectId": "3092ffc6",
            "gitRepo": "acme/TCB",
            "projectName": "tcbv2",
            "relCwd": "app",
            "originRoot": MAC_ROOT,
            "originPlatform": "darwin"
        });
        let meta: SessionMeta = serde_json::from_value(old).unwrap();
        assert_eq!(meta.code_root, None);
        assert!(!meta.home_rel && !meta.abs_cwd);
        let text = serde_json::to_string(&meta).unwrap();
        assert!(!text.contains("codeRoot") && !text.contains("homeRel") && !text.contains("absCwd"));

        // What an old client sees of a code-folder meta: no project field,
        // so not even a project named like the folder matches it.
        let new = SessionMeta {
            project_id: None,
            git_repo: None,
            project_name: None,
            rel_cwd: String::new(),
            origin_root: "/Users/alice/code".into(),
            code_root: Some("code".into()),
            ..meta
        };
        let json = serde_json::to_value(&new).unwrap();
        assert_eq!(json["codeRoot"], "code");
        let registry = Registry {
            projects: vec![RootedProject {
                id: "p".into(),
                name: "code".into(),
                git_repo: None,
                root: Some(r"C:\code".into()),
            }],
            tombstones: Default::default(),
            code_roots: Vec::new(),
            home: None,
            disabled: HashSet::new(),
        };
        assert!(registry.find(&serde_json::from_value(json).unwrap()).is_none());
    }

    #[test]
    fn old_readers_skip_home_and_absolute_metas() {
        let base = SessionMeta {
            session_id: SID.into(),
            project_id: None,
            git_repo: None,
            project_name: None,
            rel_cwd: "Desktop/foo".into(),
            origin_root: "/Users/alice".into(),
            origin_platform: "darwin".into(),
            updated_at: None,
            source_mtime: None,
            code_root: None,
            origin_home: None,
            home_rel: true,
            abs_cwd: false,
        };
        let abs = SessionMeta {
            rel_cwd: String::new(),
            origin_root: "/tmp/scratch".into(),
            home_rel: false,
            abs_cwd: true,
            ..base.clone()
        };
        // A machine with a home, a code folder and a project named like
        // the folders involved.
        let registry = Registry {
            projects: ["alice", "foo", "scratch", "Desktop"]
                .into_iter()
                .map(|n| RootedProject {
                    id: n.into(),
                    name: n.into(),
                    git_repo: None,
                    root: Some(format!(r"C:\{n}")),
                })
                .collect(),
            tombstones: Default::default(),
            code_roots: vec![r"C:\code".into()],
            home: Some(r"C:\Users\bob".into()),
            disabled: HashSet::new(),
        };
        for (meta, field) in [(base, "homeRel"), (abs, "absCwd")] {
            let json = serde_json::to_value(&meta).unwrap();
            assert_eq!(json[field], true);
            assert!(json.get("codeRoot").is_none());
            assert!(json["projectId"].is_null() && json["gitRepo"].is_null() && json["projectName"].is_null());
            // An old client's struct has no such field: serde drops it, and
            // what is left names no project and no code folder, so its pull
            // finds nowhere to put the session and writes nothing.
            let mut old_view: SessionMeta = serde_json::from_value(json).unwrap();
            old_view.home_rel = false;
            old_view.abs_cwd = false;
            assert!(registry.find(&old_view).is_none());
            assert_eq!(registry.local_root(&old_view, "win32"), None);
            // This build places it.
            assert!(registry.local_root(&meta, "win32").is_some());
        }
    }

    #[tokio::test]
    async fn a_rewritten_tail_is_pushed_from_the_last_line_the_server_has() {
        let blobs = MemBlobs::default();
        let mac = machine(&blobs, "darwin", MAC_ROOT, json!([record("3092ffc6", "tcbv2", "darwin", MAC_ROOT)]));
        let pc = machine(&blobs, "win32", WIN_ROOT, json!([record("3092ffc6", "tcbv2", "win32", WIN_ROOT)]));
        let file = mac.session_path(MAC_ROOT, SID);
        let line = |i: usize| user(&format!("u{i}"), MAC_ROOT, "x");
        mac.write(&file, &(1..=5).map(line).collect::<String>());
        mac.sync.push_changed(vec![file.clone()]).await.unwrap();

        // Claude Code retracts u4 (truncates there, rewrites u5 back) and
        // then appends u6 and u7: five lines again plus two.
        let rewritten = [line(1), line(2), line(3), line(5), line(6), line(7)].concat();
        mac.write(&file, &rewritten);
        mac.sync.push_changed(vec![file.clone()]).await.unwrap();
        let id = mac.machine_id();
        let second = blobs.segment_text(&segment_path(SID, &id, 2));
        assert_eq!(second, [line(6), line(7)].concat(), "u6 must not be skipped");

        // Retraction without an append: nothing to send (it was a full
        // re-send before).
        mac.write(&file, &[line(1), line(2), line(3), line(5), line(6)].concat());
        mac.sync.push_changed(vec![file.clone()]).await.unwrap();
        assert_eq!(blobs.segments_of(&id).len(), 2);

        pc.sync.pull().await.unwrap();
        let pc_file = pc.session_path(WIN_ROOT, SID);
        assert_eq!(uuids(&pc_file), ["u1", "u2", "u3", "u4", "u5", "u6", "u7"]);
    }

    #[test]
    fn resume_line_follows_uuids_and_gives_up_safely() {
        let text = [user("a", MAC_ROOT, "1"), user("b", MAC_ROOT, "2"), user("c", MAC_ROOT, "3")].concat();
        let bytes = text.as_bytes();
        let ends = line_ends(bytes);
        let st = SessionState {
            pushed_lines: 3,
            tail: tail_keys(bytes, &ends, 3),
            ..Default::default()
        };
        assert_eq!(resume_line(bytes, &ends, &st), 3);
        // Old state without keys: the count, or everything if it shrank.
        let legacy = SessionState { pushed_lines: 2, ..Default::default() };
        assert_eq!(resume_line(bytes, &ends, &legacy), 2);
        let legacy = SessionState { pushed_lines: 9, ..Default::default() };
        assert_eq!(resume_line(bytes, &ends, &legacy), 0);
        // None of the pushed lines is left: send everything.
        let other = [user("x", MAC_ROOT, "1"), user("y", MAC_ROOT, "2")].concat();
        let other_ends = line_ends(other.as_bytes());
        assert_eq!(resume_line(other.as_bytes(), &other_ends, &st), 0);
    }

    #[tokio::test]
    async fn a_session_open_in_claude_code_here_is_not_merged_into() {
        let blobs = MemBlobs::default();
        let mac = machine(&blobs, "darwin", MAC_ROOT, json!([record("3092ffc6", "tcbv2", "darwin", MAC_ROOT)]));
        let pc = machine(&blobs, "win32", WIN_ROOT, json!([record("3092ffc6", "tcbv2", "win32", WIN_ROOT)]));
        mac.write(&mac.session_path(MAC_ROOT, SID), &[user("u1", MAC_ROOT, "a"), user("u2", MAC_ROOT, "b")].concat());
        mac.sync.sweep_push().await.unwrap();

        // Idle at the prompt for ten minutes, but its process is alive (this
        // test process stands in for it).
        let pc_file = pc.session_path(WIN_ROOT, SID);
        pc.write(&pc_file, &user("u1", WIN_ROOT, "a"));
        age(&pc_file, 600);
        let sessions = pc.claude.parent().unwrap().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let entry = sessions.join(format!("{}.json", std::process::id()));
        std::fs::write(&entry, json!({"pid": std::process::id(), "sessionId": SID}).to_string()).unwrap();
        pc.sync.pull().await.unwrap();
        assert_eq!(uuids(&pc_file), ["u1"]);

        // A leftover from a crashed process does not count.
        std::fs::write(&entry, json!({"pid": u32::MAX - 7, "sessionId": SID}).to_string()).unwrap();
        pc.sync.pull().await.unwrap();
        assert_eq!(uuids(&pc_file), ["u1", "u2"]);
    }

    #[test]
    fn a_merge_never_marks_lines_the_push_did_not_see() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join(format!("{SID}.jsonl"));
        std::fs::write(&file, user("u1", MAC_ROOT, "a")).unwrap();
        age(&file, 600);
        let seen = std::fs::metadata(&file).unwrap().len();
        // Claude Code appends after the push read the file.
        let mut f = std::fs::OpenOptions::new().append(true).open(&file).unwrap();
        f.write_all(user("u2", MAC_ROOT, "b").as_bytes()).unwrap();
        drop(f);
        age(&file, 600);
        let segment = PulledSegment {
            name: "m-000001.jsonl.gz".into(),
            raw: user("u9", MAC_ROOT, "remote").into_bytes(),
            map: CwdMap {
                from_root: MAC_ROOT.into(),
                from_platform: "darwin",
                to_root: MAC_ROOT.into(),
                to_platform: "darwin",
            },
            source_mtime: None,
        };
        let err = merge_into_file(&file, std::slice::from_ref(&segment), Some(seen)).err().unwrap();
        assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);
        assert_eq!(uuids(&file), ["u1", "u2"], "left alone");
        // With the length it has now, it goes through.
        let now_len = std::fs::metadata(&file).unwrap().len();
        let result = merge_into_file(&file, &[segment], Some(now_len)).unwrap();
        assert_eq!(result.total_lines, 3);
        assert_eq!(uuids(&file), ["u1", "u2", "u9"]);
    }

    #[tokio::test]
    async fn a_pulled_session_is_dated_by_its_source_file_not_just_its_lines() {
        let blobs = MemBlobs::default();
        let mac = machine(&blobs, "darwin", MAC_ROOT, json!([record("3092ffc6", "tcbv2", "darwin", MAC_ROOT)]));
        let pc = machine(&blobs, "win32", WIN_ROOT, json!([record("3092ffc6", "tcbv2", "win32", WIN_ROOT)]));
        // The newest timestamped line is older than the file itself: Claude
        // Code appends marker lines (last-prompt, mode) that carry no time.
        // Relative to now: sessions older than MAX_SESSION_AGE are never
        // pulled, so fixed dates turned this test red the day they aged out.
        let line_at = (Utc::now() - chrono::Duration::days(45))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let stamped = format!(
            "{}\n{}\n",
            json!({"cwd": MAC_ROOT, "uuid": "u1", "type": "user", "timestamp": line_at}),
            json!({"type": "last-prompt", "lastPrompt": "x"})
        );
        let mac_file = mac.session_path(MAC_ROOT, SID);
        mac.write(&mac_file, &stamped);
        let source_at = Utc::now() - chrono::Duration::days(2);
        let source = SystemTime::from(
            registry::parse_timestamp(&source_at.format("%Y-%m-%dT%H:%M:%SZ").to_string()).unwrap(),
        );
        std::fs::OpenOptions::new().write(true).open(&mac_file).unwrap().set_modified(source).unwrap();
        mac.sync.sweep_push().await.unwrap();
        pc.sync.pull().await.unwrap();
        let pc_file = pc.session_path(WIN_ROOT, SID);
        let mtime = std::fs::metadata(&pc_file).unwrap().modified().unwrap();
        assert_eq!(mtime, source, "dated by the source file, not by its 45-day-old last line");
    }

    #[tokio::test]
    async fn old_sessions_stay_off_a_machine_that_never_had_them_and_metas_are_cached() {
        let blobs = MemBlobs::default();
        let mac = machine(&blobs, "darwin", MAC_ROOT, json!([record("3092ffc6", "tcbv2", "darwin", MAC_ROOT)]));
        let pc = machine(&blobs, "win32", WIN_ROOT, json!([record("3092ffc6", "tcbv2", "win32", WIN_ROOT)]));
        // Uploaded 40 days ago.
        blobs.backdate(Some(Utc::now().timestamp() - 40 * 24 * 60 * 60));
        mac.write(&mac.session_path(MAC_ROOT, SID), &user("u1", MAC_ROOT, "old"));
        mac.sync.sweep_push().await.unwrap();
        blobs.backdate(None);
        // And one the PC cannot apply (no checkout of its project here).
        let elsewhere = "/Users/alice/code/other";
        let mut other = record("o1", "other", "darwin", elsewhere);
        other["gitRepo"] = json!("acme/Other");
        let mac2 = machine(&blobs, "darwin", elsewhere, json!([other]));
        mac2.write(&mac2.session_path(elsewhere, SID2), &user("v1", elsewhere, "x"));
        mac2.sync.sweep_push().await.unwrap();

        pc.sync.pull().await.unwrap();
        assert!(pc.copies_of(SID).is_empty(), "40 days old and never here");
        assert!(pc.copies_of(SID2).is_empty(), "no checkout here");
        let gets = blobs.gets();
        pc.sync.pull().await.unwrap();
        assert_eq!(blobs.gets(), gets, "an unchanged meta is not downloaded again");
    }

    #[tokio::test]
    async fn only_the_newest_copy_of_a_session_is_pushed() {
        let blobs = MemBlobs::default();
        let mac = machine(&blobs, "darwin", MAC_ROOT, json!([record("3092ffc6", "tcbv2", "darwin", MAC_ROOT)]));
        let stale = mac.session_path(MAC_ROOT, SID);
        let active = mac.session_path("/Users/alice/code/tcbv2/app", SID);
        let head: String = (1..=3).map(|i| user(&format!("u{i}"), MAC_ROOT, "x")).collect();
        mac.write(&stale, &head);
        age(&stale, 3600);
        mac.write(&active, &[head.clone(), user("u4", MAC_ROOT, "x")].concat());
        mac.sync.sweep_push().await.unwrap();
        let puts = blobs.puts();
        for _ in 0..2 {
            mac.sync.sweep_push().await.unwrap();
            mac.sync.push_changed(vec![stale.clone(), active.clone()]).await.unwrap();
        }
        assert_eq!(blobs.puts(), puts, "no copy is re-uploaded on every pass");
        let uploaded: String = blobs
            .segments_of(&mac.machine_id())
            .iter()
            .map(|s| blobs.segment_text(s))
            .collect();
        assert_eq!(uploaded.lines().count(), 4);
    }

    // ── Session index ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn uploads_stay_owed_to_the_index_until_it_is_written() {
        let blobs = MemBlobs::default();
        let mac = machine(&blobs, "darwin", MAC_ROOT, json!([record("3092ffc6", "tcbv2", "darwin", MAC_ROOT)]));
        let file = mac.session_path(MAC_ROOT, SID);
        assert_eq!(mac.sync.index_owed().await.unwrap(), None, "nothing uploaded yet");

        mac.write(&file, &user("u1", MAC_ROOT, "a"));
        mac.sync.push_changed(vec![file.clone()]).await.unwrap();
        let id = mac.machine_id();
        let owed = mac.sync.index_owed().await.unwrap().unwrap();
        assert_eq!(owed[SID][&id]["seq"], 1);

        // The index write failed (or never ran): the segments are on the
        // server regardless, and the next push owes the newer number.
        mac.append(&file, &user("u2", MAC_ROOT, "b"));
        mac.sync.push_changed(vec![file.clone()]).await.unwrap();
        assert_eq!(blobs.segments_of(&id).len(), 2);
        let owed = mac.sync.index_owed().await.unwrap().unwrap();
        assert_eq!(owed[SID][&id]["seq"], 2);
        assert_eq!(owed.as_object().unwrap().len(), 1);

        mac.sync.index_written(&owed).await.unwrap();
        assert_eq!(mac.sync.index_owed().await.unwrap(), None);
        // Kept across restarts: it lives in the state file.
        let state = std::fs::read_to_string(mac.store.join(STATE_FILE)).unwrap();
        assert!(state.contains("\"indexedSeq\": 2"), "{state}");
    }

    #[tokio::test]
    async fn an_index_pull_lists_only_the_sessions_it_names() {
        let blobs = MemBlobs::default();
        let mac = machine_with(
            &blobs,
            "darwin",
            &[MAC_ROOT, "/Users/alice/code/other"],
            json!([record("3092ffc6", "tcbv2", "darwin", MAC_ROOT), {
                "id": "o1", "name": "other", "path": "/Users/alice/code/other",
                "paths": {"darwin": "/Users/alice/code/other"}, "gitRepo": "acme/Other",
                "createdAt": "2026-09-01T00:00:00Z"
            }]),
            MAX_SEGMENT_RAW_BYTES,
        );
        // The PC has tcbv2 checked out, but not `other`.
        let pc = machine(&blobs, "win32", WIN_ROOT, json!([record("3092ffc6", "tcbv2", "win32", WIN_ROOT)]));
        mac.write(&mac.session_path(MAC_ROOT, SID), &user("u1", MAC_ROOT, "a"));
        let other = "/Users/alice/code/other";
        mac.write(&mac.session_path(other, SID2), &user("v1", other, "x"));
        mac.sync.sweep_push().await.unwrap();
        let index = mac.sync.index_owed().await.unwrap().unwrap();
        blobs.take_lists();

        pc.sync.pull_indexed(&index).await.unwrap();
        let pc_file = pc.session_path(WIN_ROOT, SID);
        assert_eq!(uuids(&pc_file), ["u1"]);
        assert!(pc.copies_of(SID2).is_empty(), "no checkout of its project here");
        let mut lists = blobs.take_lists();
        lists.sort();
        let mut want = vec![session_prefix(SID), session_prefix(SID2)];
        want.sort();
        assert_eq!(lists, want, "each named session by its own prefix, never the whole store");
        assert!(!pc.sync.wants_index_retry());

        // The same index again: one session applied, the other already
        // dealt with at that number. No request at all.
        pc.sync.pull_indexed(&index).await.unwrap();
        assert!(blobs.take_lists().is_empty());

        // The Mac adds to the session the PC cannot apply: listed again, once.
        mac.append(&mac.session_path(other, SID2), &user("v2", other, "y"));
        mac.sync.push_changed(vec![mac.session_path(other, SID2)]).await.unwrap();
        let newer = mac.sync.index_owed().await.unwrap().unwrap();
        blobs.take_lists();
        pc.sync.pull_indexed(&newer).await.unwrap();
        assert_eq!(blobs.take_lists(), vec![session_prefix(SID2)]);

        // An index entry of this machine's own, or a bad name, is no target.
        let own = json!({ SID: { pc.machine_id(): {"seq": 99, "at": "2026-09-24T10:00:00Z"} },
                          "not-a-session": { "m": {"seq": 1, "at": "2026-09-24T10:00:00Z"} } });
        pc.sync.pull_indexed(&own).await.unwrap();
        assert!(blobs.take_lists().is_empty());
    }

    #[tokio::test]
    async fn an_index_pull_comes_back_for_a_session_in_use_here() {
        let blobs = MemBlobs::default();
        let mac = machine(&blobs, "darwin", MAC_ROOT, json!([record("3092ffc6", "tcbv2", "darwin", MAC_ROOT)]));
        let pc = machine(&blobs, "win32", WIN_ROOT, json!([record("3092ffc6", "tcbv2", "win32", WIN_ROOT)]));
        mac.write(&mac.session_path(MAC_ROOT, SID), &[user("u1", MAC_ROOT, "a"), user("u2", MAC_ROOT, "b")].concat());
        mac.sync.sweep_push().await.unwrap();
        let index = mac.sync.index_owed().await.unwrap().unwrap();

        // Written on the PC seconds ago: live, so not merged into yet.
        let pc_file = pc.session_path(WIN_ROOT, SID);
        pc.write(&pc_file, &user("u1", WIN_ROOT, "a"));
        pc.sync.pull_indexed(&index).await.unwrap();
        assert_eq!(uuids(&pc_file), ["u1"]);
        assert!(pc.sync.wants_index_retry(), "the next poll must try again without a new index");

        age(&pc_file, 120);
        pc.sync.pull_indexed(&index).await.unwrap();
        assert_eq!(uuids(&pc_file), ["u1", "u2"]);
        assert!(!pc.sync.wants_index_retry());
    }

    #[tokio::test]
    async fn an_index_pull_makes_no_request_for_a_session_open_here() {
        let blobs = MemBlobs::default();
        let mac = machine(&blobs, "darwin", MAC_ROOT, json!([record("3092ffc6", "tcbv2", "darwin", MAC_ROOT)]));
        let pc = machine(&blobs, "win32", WIN_ROOT, json!([record("3092ffc6", "tcbv2", "win32", WIN_ROOT)]));
        mac.write(&mac.session_path(MAC_ROOT, SID), &[user("u1", MAC_ROOT, "a"), user("u2", MAC_ROOT, "b")].concat());
        mac.sync.sweep_push().await.unwrap();
        let index = mac.sync.index_owed().await.unwrap().unwrap();

        // Left open in Claude Code on the PC, idle at the prompt (this test
        // process stands in for it), and continued on the Mac.
        let pc_file = pc.session_path(WIN_ROOT, SID);
        pc.write(&pc_file, &user("u1", WIN_ROOT, "a"));
        age(&pc_file, 600);
        let sessions = pc.claude.parent().unwrap().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let entry = sessions.join(format!("{}.json", std::process::id()));
        std::fs::write(&entry, json!({"pid": std::process::id(), "sessionId": SID}).to_string()).unwrap();
        blobs.take_lists();
        for _ in 0..3 {
            pc.sync.pull_indexed(&index).await.unwrap();
            assert!(pc.sync.wants_index_retry(), "the next poll comes back for it");
        }
        assert!(blobs.take_lists().is_empty(), "no listing while it is open here");
        assert_eq!(uuids(&pc_file), ["u1"]);

        // Closed: listed once and merged.
        std::fs::remove_file(&entry).unwrap();
        pc.sync.pull_indexed(&index).await.unwrap();
        assert_eq!(blobs.take_lists(), vec![session_prefix(SID)]);
        assert_eq!(uuids(&pc_file), ["u1", "u2"]);
        assert!(!pc.sync.wants_index_retry());
    }

    #[tokio::test]
    async fn a_session_that_fails_to_apply_waits_for_new_segments() {
        let blobs = MemBlobs::default();
        let mac = machine(&blobs, "darwin", MAC_ROOT, json!([record("3092ffc6", "tcbv2", "darwin", MAC_ROOT)]));
        let pc = machine(&blobs, "win32", WIN_ROOT, json!([record("3092ffc6", "tcbv2", "win32", WIN_ROOT)]));
        mac.write(&mac.session_path(MAC_ROOT, SID), &user("u1", MAC_ROOT, "a"));
        mac.sync.sweep_push().await.unwrap();
        let mac_id = mac.machine_id();
        // A second segment that does not unpack.
        blobs.put(&segment_path(SID, &mac_id, 2), b"not gzip".to_vec()).await.unwrap();
        let now = registry::format_timestamp(Utc::now());
        let index = json!({ SID: { mac_id.as_str(): {"seq": 2, "at": now} } });

        blobs.take_lists();
        assert!(pc.sync.pull_indexed(&index).await.is_err());
        assert_eq!(blobs.take_lists(), vec![session_prefix(SID)]);
        assert!(!pc.sync.wants_index_retry(), "it would only fail the same way");
        // The same index again: no listing, nothing downloaded again.
        let gets = blobs.gets();
        pc.sync.pull_indexed(&index).await.unwrap();
        assert!(blobs.take_lists().is_empty());
        assert_eq!(blobs.gets(), gets);
        // The hourly full pull still goes back for it.
        assert!(pc.sync.pull().await.is_err());
        assert_eq!(blobs.take_lists(), vec![REMOTE_PREFIX.to_string()]);
    }

    #[tokio::test]
    async fn an_index_pull_naming_many_sessions_lists_the_store_once() {
        let blobs = MemBlobs::default();
        let mac = machine(&blobs, "darwin", MAC_ROOT, json!([record("3092ffc6", "tcbv2", "darwin", MAC_ROOT)]));
        let pc = machine(&blobs, "win32", WIN_ROOT, json!([record("3092ffc6", "tcbv2", "win32", WIN_ROOT)]));
        let sids: Vec<String> = (0..=PER_SESSION_LISTING_MAX)
            .map(|i| format!("{i:08x}-0000-4000-8000-{i:012x}"))
            .collect();
        for sid in &sids {
            mac.write(&mac.session_path(MAC_ROOT, sid), &user("u1", MAC_ROOT, "a"));
        }
        mac.sync.sweep_push().await.unwrap();
        let index = mac.sync.index_owed().await.unwrap().unwrap();
        assert_eq!(index.as_object().unwrap().len(), PER_SESSION_LISTING_MAX + 1);

        blobs.take_lists();
        pc.sync.pull_indexed(&index).await.unwrap();
        assert_eq!(blobs.take_lists(), vec![REMOTE_PREFIX.to_string()], "one listing, not one per session");
        for sid in &sids {
            assert_eq!(pc.copies_of(sid).len(), 1, "{sid}");
        }
    }

    // ── Periodic sweep ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn a_cheap_sweep_costs_nothing_until_a_file_changes() {
        let blobs = MemBlobs::default();
        let tcb = record("3092ffc6", "tcbv2", "darwin", MAC_ROOT);
        let mac = machine_at_home(&blobs, "darwin", Some("/Users/alice"), &[MAC_ROOT], json!([tcb]), MAX_SEGMENT_RAW_BYTES);
        let in_project = mac.session_path(MAC_ROOT, SID);
        mac.write(&in_project, &user("u1", MAC_ROOT, "a"));
        // A session that has written no cwd yet is skipped.
        let pending = mac.session_path("/Users/alice/Desktop", SID2);
        mac.write(&pending, &marker("mode", "normal"));
        age(&in_project, 120);
        age(&pending, 120);
        mac.sync.sweep_push().await.unwrap();
        let id = mac.machine_id();
        assert_eq!(blobs.segments_of(&id).len(), 1, "only the session with a cwd");

        // Nothing changed: no listing, no upload.
        blobs.take_lists();
        let puts = blobs.puts();
        mac.sync.sweep_changed().await.unwrap();
        assert!(blobs.take_lists().is_empty());
        assert_eq!(blobs.puts(), puts);

        // A session written this very minute is the watcher push's to send.
        mac.append(&in_project, &user("u2", MAC_ROOT, "b"));
        mac.sync.sweep_changed().await.unwrap();
        assert_eq!(blobs.puts(), puts);
        // Once idle, a sweep sends what the watcher missed.
        age(&in_project, 120);
        mac.sync.sweep_changed().await.unwrap();
        assert_eq!(blobs.segments_of(&id).len(), 2);
        assert_eq!(blobs.take_lists(), vec![session_prefix(SID)], "listed by its own prefix");

        // The skipped session gets its cwd: the next sweep sends it.
        let desktop = "/Users/alice/Desktop";
        mac.append(&pending, &user("v1", desktop, "x"));
        age(&pending, 120);
        mac.sync.sweep_changed().await.unwrap();
        let uploaded: Vec<String> = blobs
            .segments_of(&id)
            .into_iter()
            .filter(|p| p.contains(SID2))
            .collect();
        assert_eq!(uploaded.len(), 1);
        assert_eq!(blobs.segment_text(&uploaded[0]), [marker("mode", "normal"), user("v1", desktop, "x")].concat());
        assert!(meta_of(&blobs, SID2, &mac).home_rel);
    }

    // ── Refused uploads ─────────────────────────────────────────────────────

    /// `kib` KiB of text that gzip cannot shrink much.
    fn noise(seed: &str, kib: usize) -> String {
        (0..kib * 16)
            .map(|i| hex_hash(format!("{seed}{i}").as_bytes()))
            .collect::<String>()
            .chars()
            .take(kib * 1024)
            .collect()
    }

    /// A line of `kib` KiB that gzip cannot shrink much.
    fn noisy_line(uuid: &str, kib: usize) -> String {
        user(uuid, MAC_ROOT, &noise(uuid, kib))
    }

    #[tokio::test]
    async fn a_refused_line_goes_up_as_a_stub_that_keeps_the_chain() {
        let blobs = MemBlobs::default();
        let mac = machine(&blobs, "darwin", MAC_ROOT, json!([record("3092ffc6", "tcbv2", "darwin", MAC_ROOT)]));
        let pc = machine(&blobs, "win32", WIN_ROOT, json!([record("3092ffc6", "tcbv2", "win32", WIN_ROOT)]));
        // A tool result carrying a big text and a screenshot, between the
        // turn that asked for it and the answer that follows it.
        let big = noise("big", 80);
        let result = json!({
            "parentUuid": "u1", "cwd": MAC_ROOT, "sessionId": SID, "type": "user", "uuid": "r1",
            "timestamp": "2026-09-24T10:00:02.000Z",
            "message": {"role": "user", "content": [{
                "type": "tool_result", "tool_use_id": "toolu_1", "content": [
                    {"type": "text", "text": big},
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": big}}
                ]
            }]},
            "toolUseResult": {"stdout": big}
        });
        let answer = json!({
            "parentUuid": "r1", "cwd": MAC_ROOT, "sessionId": SID, "type": "assistant", "uuid": "a1",
            "message": {"role": "assistant", "content": "done"}
        });
        let file = mac.session_path(MAC_ROOT, SID);
        mac.write(&file, &[user("u1", MAC_ROOT, "a"), format!("{result}\n"), format!("{answer}\n")].concat());
        blobs.refuse_over(Some((4096, 413)));
        mac.sync.push_changed(vec![file.clone()]).await.unwrap();

        pc.sync.pull().await.unwrap();
        let pc_file = pc.session_path(WIN_ROOT, SID);
        assert_eq!(uuids(&pc_file), ["u1", "r1", "a1"], "no gap in the parentUuid chain");
        let lines = parsed(&pc_file);
        let stub = &lines[1];
        assert_eq!(stub["parentUuid"], "u1");
        assert_eq!(stub["timestamp"], "2026-09-24T10:00:02.000Z");
        let block = &stub["message"]["content"][0];
        assert_eq!(block["type"], "tool_result");
        assert_eq!(block["tool_use_id"], "toolu_1");
        let note = format!("[omitted by Mnemos sync: {} bytes]", big.len());
        assert_eq!(block["content"][0], json!({"type": "text", "text": note}));
        // Cut base64 would be sent back to the API on every resumed turn.
        assert_eq!(
            block["content"][1],
            json!({"type": "text", "text": format!("[image omitted by Mnemos sync: {} bytes]", big.len())})
        );
        assert_eq!(stub["toolUseResult"]["stdout"], note);
        assert_eq!(lines[2]["parentUuid"], "r1");

        // Settled: nothing is sent again.
        let puts = blobs.puts();
        mac.sync.push_changed(vec![file.clone()]).await.unwrap();
        assert_eq!(blobs.puts(), puts);
    }

    #[tokio::test]
    async fn a_refused_line_with_nothing_to_cut_is_skipped_not_retried_forever() {
        let blobs = MemBlobs::default();
        // One line per segment.
        let mac = machine_capped(&blobs, "darwin", MAC_ROOT, json!([record("3092ffc6", "tcbv2", "darwin", MAC_ROOT)]), 200);
        let pc = machine(&blobs, "win32", WIN_ROOT, json!([record("3092ffc6", "tcbv2", "win32", WIN_ROOT)]));
        let file = mac.session_path(MAC_ROOT, SID);
        mac.write(&file, &[user("u1", MAC_ROOT, "a"), noisy_line("big", 32), user("u3", MAC_ROOT, "c")].concat());
        blobs.refuse_over(Some((4096, 413)));

        mac.sync.push_changed(vec![file.clone()]).await.unwrap();
        let id = mac.machine_id();
        let uploaded: Vec<String> = blobs.segments_of(&id).iter().map(|s| blobs.segment_text(s)).collect();
        assert_eq!(uploaded, vec![user("u1", MAC_ROOT, "a"), user("u3", MAC_ROOT, "c")]);
        // Settled: the next push has nothing to send, and does not try the
        // refused line again.
        let puts = blobs.puts();
        mac.sync.push_changed(vec![file.clone()]).await.unwrap();
        assert_eq!(blobs.puts(), puts);

        pc.sync.pull().await.unwrap();
        assert_eq!(uuids(&pc.session_path(WIN_ROOT, SID)), ["u1", "u3"]);
    }

    #[tokio::test]
    async fn a_refused_segment_is_resent_line_by_line() {
        let blobs = MemBlobs::default();
        // All three lines fit one segment; the server refuses it for the
        // big one.
        let mac = machine(&blobs, "darwin", MAC_ROOT, json!([record("3092ffc6", "tcbv2", "darwin", MAC_ROOT)]));
        let file = mac.session_path(MAC_ROOT, SID);
        mac.write(&file, &[user("u1", MAC_ROOT, "a"), noisy_line("big", 32), user("u3", MAC_ROOT, "c")].concat());
        blobs.refuse_over(Some((4096, 413)));
        mac.sync.push_changed(vec![file.clone()]).await.unwrap();
        let id = mac.machine_id();
        let uploaded: Vec<String> = blobs.segments_of(&id).iter().map(|s| blobs.segment_text(s)).collect();
        assert_eq!(uploaded, vec![user("u1", MAC_ROOT, "a"), user("u3", MAC_ROOT, "c")]);
    }

    #[tokio::test]
    async fn a_refusal_for_other_reasons_keeps_the_line_for_the_next_push() {
        let blobs = MemBlobs::default();
        let mac = machine_capped(&blobs, "darwin", MAC_ROOT, json!([record("3092ffc6", "tcbv2", "darwin", MAC_ROOT)]), 200);
        let file = mac.session_path(MAC_ROOT, SID);
        let big = noisy_line("big", 32);
        mac.write(&file, &[user("u1", MAC_ROOT, "a"), big.clone(), user("u3", MAC_ROOT, "c")].concat());
        // Auth, rate limits, server errors and any 4xx but 413 say nothing
        // certain about the line.
        for status in [400, 401, 403, 404, 409, 422, 429, 500] {
            blobs.refuse_over(Some((4096, status)));
            assert!(mac.sync.push_changed(vec![file.clone()]).await.is_err(), "{status}");
        }
        blobs.refuse_over(None);
        mac.sync.push_changed(vec![file.clone()]).await.unwrap();
        let id = mac.machine_id();
        let uploaded: String = blobs.segments_of(&id).iter().map(|s| blobs.segment_text(s)).collect();
        assert_eq!(uploaded, [user("u1", MAC_ROOT, "a"), big, user("u3", MAC_ROOT, "c")].concat());
    }

    // ── Pieces ──────────────────────────────────────────────────────────────

    #[test]
    fn transcript_paths_are_top_level_sessions_only() {
        let dir = PathBuf::from(if cfg!(windows) { r"C:\Users\h\.claude\projects" } else { "/home/h/.claude/projects" });
        let sync = TranscriptSync::with_parts(Arc::new(MemBlobs::default()), dir.clone(), dir.clone(), Options::real());
        let slug = dir.join("C--code-mnemos");
        assert!(sync.is_transcript_path(&slug.join(format!("{SID}.jsonl"))));
        assert!(!sync.is_transcript_path(&slug.join(SID).join("subagents").join(format!("{SID2}.jsonl"))));
        assert!(!sync.is_transcript_path(&slug.join(SID).join(format!("{SID2}.jsonl"))));
        assert!(!sync.is_transcript_path(&slug.join("agent-1234abcd.jsonl")));
        assert!(!sync.is_transcript_path(&slug.join(format!("{SID}.jsonl.tcb-sync.tmp"))));
        assert!(!sync.is_transcript_path(&slug.join("memory").join("MEMORY.md")));
        assert!(!sync.is_transcript_path(&dir.join(format!("{SID}.jsonl"))));
    }

    #[test]
    fn remote_names_round_trip() {
        let machine = "3f2a9c0e5b7d4e1f8a6b9c0d1e2f3a4b";
        let seg = segment_path(SID, machine, 7);
        assert_eq!(seg, format!("claude-sessions/{SID}/{machine}-000007.jsonl.gz"));
        assert!(crate::client::validate_blob_path(&seg).is_ok());
        assert!(crate::client::validate_blob_path(&meta_path(SID, machine)).is_ok());
        match parse_blob_path(&seg) {
            Some((sid, BlobName::Segment { machine: m, seq })) => {
                assert_eq!((sid.as_str(), m.as_str(), seq), (SID, machine, 7));
            }
            _ => panic!("segment did not parse"),
        }
        // A dashed machine id still splits at the last dash.
        match parse_blob_path(&segment_path(SID, "a-b-c", 1234567)) {
            Some((_, BlobName::Segment { machine: m, seq })) => assert_eq!((m.as_str(), seq), ("a-b-c", 1234567)),
            _ => panic!("dashed machine id did not parse"),
        }
        assert!(matches!(parse_blob_path(&meta_path(SID, machine)), Some((_, BlobName::Meta { .. }))));
        assert!(parse_blob_path("claude-sessions/not-a-uuid/m-000001.jsonl.gz").is_none());
        assert!(parse_blob_path(&format!("claude-sessions/{SID}/m-x1.jsonl.gz")).is_none());
        assert!(parse_blob_path(&format!("other/{SID}/m.meta.json")).is_none());
    }

    #[test]
    fn cwd_rewrite_touches_only_the_top_level_field() {
        let to_win = CwdMap {
            from_root: MAC_ROOT.into(),
            from_platform: "darwin",
            to_root: WIN_ROOT.into(),
            to_platform: "win32",
        };
        // Nested `cwd` first, top-level one after it; the key order and the
        // rest of the text must survive.
        let line = br#"{"toolUseResult":{"cwd":"/Users/alice/code/tcbv2"},"type":"user","cwd":"/Users/alice/code/tcbv2/src/deep","uuid":"u1","note":"a \"quoted\" }{ text"}"#;
        let out = String::from_utf8(rewrite_cwd(line, &to_win).unwrap()).unwrap();
        assert_eq!(
            out,
            r#"{"toolUseResult":{"cwd":"/Users/alice/code/tcbv2"},"type":"user","cwd":"C:\\code\\mnemos\\src\\deep","uuid":"u1","note":"a \"quoted\" }{ text"}"#
        );
        // macOS paths compare case-insensitively; the root itself maps to the root.
        let out = rewrite_cwd(br#"{"cwd":"/users/ALICE/code/TCBV2"}"#, &to_win).unwrap();
        assert_eq!(out, br#"{"cwd":"C:\\code\\mnemos"}"#);
        // Outside the project, or no top-level cwd: left alone.
        assert!(rewrite_cwd(br#"{"cwd":"/Users/alice/Desktop"}"#, &to_win).is_none());
        assert!(rewrite_cwd(br#"{"cwd":"/Users/alice/code/tcbv2-other"}"#, &to_win).is_none());
        assert!(rewrite_cwd(br#"{"x":{"cwd":"/Users/alice/code/tcbv2"}}"#, &to_win).is_none());
        assert!(rewrite_cwd(b"not json", &to_win).is_none());

        let to_mac = CwdMap {
            from_root: WIN_ROOT.into(),
            from_platform: "win32",
            to_root: MAC_ROOT.into(),
            to_platform: "darwin",
        };
        let out = rewrite_cwd(br#"{"type":"user","cwd":"c:/Code/Mnemos\\a\\b"}"#, &to_mac).unwrap();
        assert_eq!(out, br#"{"type":"user","cwd":"/Users/alice/code/tcbv2/a/b"}"#);
    }

    #[test]
    fn markers_repeat_in_new_stretches_but_not_in_known_ones() {
        let inc = |segment: usize, l: &str| Incoming {
            line: l.as_bytes().to_vec(),
            uuid: line_uuid(l.as_bytes()),
            segment,
        };
        let normal = r#"{"type":"mode","mode":"normal"}"#;
        let plan = r#"{"type":"mode","mode":"plan"}"#;
        let mut uuids: HashSet<String> = ["u1".to_string()].into();
        let mut hashes: HashSet<[u8; 32]> = [line_hash(normal.as_bytes())].into();
        let incoming = [
            inc(0, normal),             // before u1, which is known: dropped
            inc(0, r#"{"uuid":"u1"}"#), // known
            inc(0, r#"{"uuid":"u2"}"#), // new
            inc(0, plan),               // new stretch: kept
            inc(0, normal),             // repeat of a known marker in a new stretch: kept
            inc(0, r#"{"uuid":"u2"}"#), // repeated within the batch: dropped
            // A re-upload in its own segment: the marker belongs with the
            // known u1 that follows it, not with the new u2 before it.
            inc(1, plan),
            inc(1, r#"{"uuid":"u1"}"#),
        ];
        assert_eq!(select_new_lines(&incoming, &mut uuids, &mut hashes), vec![2, 3, 4]);
    }

    #[test]
    fn segment_order_follows_upload_time_within_machine_order() {
        let p = |machine: &str, seq: u64, s: i64| Pending {
            machine: machine.to_string(),
            seq,
            last_modified: registry::format_timestamp(
                Utc.timestamp_opt(1_790_000_000 + s, 0).unwrap(),
            ),
        };
        let pending = vec![p("b", 1, 2), p("a", 2, 3), p("a", 1, 1), p("b", 2, 4)];
        let order: Vec<(String, u64)> = order_segments(pending)
            .into_iter()
            .map(|p| (p.machine, p.seq))
            .collect();
        assert_eq!(
            order,
            vec![("a".to_string(), 1), ("b".to_string(), 1), ("a".to_string(), 2), ("b".to_string(), 2)]
        );
        // Same timestamps: (machine, seq).
        let same = vec![p("b", 1, 0), p("a", 2, 0), p("a", 1, 0)];
        let order: Vec<(String, u64)> =
            order_segments(same).into_iter().map(|p| (p.machine, p.seq)).collect();
        assert_eq!(order, vec![("a".to_string(), 1), ("a".to_string(), 2), ("b".to_string(), 1)]);
    }

    #[test]
    fn chunks_respect_the_cap_and_never_split_a_line() {
        // Lines of 3, 3, 10, 2 bytes.
        let ends = [3, 6, 16, 18];
        assert_eq!(chunk_lines(&ends, 0, 6), vec![(0, 2), (2, 3), (3, 4)]);
        assert_eq!(chunk_lines(&ends, 1, 100), vec![(1, 4)]);
        assert!(chunk_lines(&ends, 4, 6).is_empty());
    }

    #[test]
    fn public_api_is_send_and_sync() {
        fn send_sync<T: Send + Sync>() {}
        fn send<T: Send>(_: T) {}
        send_sync::<TranscriptSync>();
        let sync = TranscriptSync::with_parts(
            Arc::new(MemBlobs::default()),
            PathBuf::from("store"),
            PathBuf::from("projects"),
            Options::real(),
        );
        send(sync.push_changed(Vec::new()));
        send(sync.sweep_push());
        send(sync.pull());
        let client = SyncClient::new(crate::auth::AdminAuth::with_base_url("https://admin.example.test"));
        send(TranscriptSync::new(client, PathBuf::new(), PathBuf::new()).pull());
    }
}

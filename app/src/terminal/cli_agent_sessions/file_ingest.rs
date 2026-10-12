//! File-based ingestion of CLI agent events (Option D fallback transport).
//!
//! On Windows the Claude Code hook fires reliably and produces well-formed
//! OSC-777 payloads, but it has no writable handle to the ConPTY (all of
//! `/dev/tty`, `CONOUT$`, `/dev/conout`, `/dev/console`, and stdout fail), so
//! the OSC transport never reaches Mnemos. This module provides an additive
//! fallback: each terminal exports a unique per-terminal NDJSON file path via
//! `MNEMOS_CLI_AGENT_EVENT_FILE`, the hook appends one JSON event per line, and
//! a filesystem watcher tails each file and feeds complete lines into the
//! *same* parse pipeline the OSC listener uses (`parse_event`, then
//! `CLIAgentSessionsModel::apply_ingested_event`).
//!
//! The OSC path is left intact. Where it also works (macOS, Linux) the hook
//! delivers every event twice, over OSC 777 and then through this file about
//! 100 ms later. The copies are not harmless: re-applying a `Stop` or
//! `PermissionRequest` re-emits `StatusChanged` and sends a second desktop
//! notification. So `apply_ingested_event` drops file events for any terminal
//! whose current session has already received an event over OSC.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use mnemos_ui::{Entity, EntityId, ModelContext, ModelHandle, SingletonEntity};
use uuid::Uuid;
use watcher::{BulkFilesystemWatcher, BulkFilesystemWatcherEvent};

use super::event::{parse_event, CLI_AGENT_NOTIFICATION_SENTINEL};
use super::CLIAgentSessionsModel;

/// Env var naming the per-terminal NDJSON file the hook appends events to.
/// Read by the patched `warp-notify.sh` (see `plugin_manager::claude`).
pub const EVENT_FILE_ENV_VAR: &str = "MNEMOS_CLI_AGENT_EVENT_FILE";

/// Debounce window for the event-file watcher. Short, since these files are
/// tiny and latency-sensitive (the panel should reflect agent state promptly).
const EVENT_FILE_WATCHER_DEBOUNCE_MILLIS: u64 = 100;

/// Directory (under the Mnemos data dir) holding per-terminal event files.
pub fn event_files_dir() -> PathBuf {
    mnemos_core::paths::data_dir().join("cli-agent-events")
}

/// Returns the event-file path for a freshly generated token.
fn event_file_for_token(token: &str) -> PathBuf {
    event_files_dir().join(format!("{token}.ndjson"))
}

/// Generates a unique token and its event-file path for a new terminal.
/// Returns `(token, path)`; the caller exports `path` via [`EVENT_FILE_ENV_VAR`]
/// and registers `token` against the terminal's view id once it's known.
pub fn new_token_and_path() -> (String, PathBuf) {
    let token = Uuid::new_v4().to_string();
    let path = event_file_for_token(&token);
    (token, path)
}

/// Deletes any stale event files left over from a previous run (e.g. a crash
/// or SIGKILL where per-terminal cleanup never ran). Called once at app init;
/// nothing references any of these files at boot, so a full sweep is safe.
pub fn sweep_stale_event_files() {
    let dir = event_files_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        // Dir doesn't exist yet (first run) or is unreadable — nothing to do.
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "ndjson") {
            if let Err(err) = std::fs::remove_file(&path) {
                log::warn!(
                    "Failed to sweep stale CLI agent event file {}: {err}",
                    path.display()
                );
            }
        }
    }
}

/// Per-file tail state: how many bytes we've already consumed, plus any
/// trailing bytes from an incomplete (still-being-written) final line.
#[derive(Default)]
struct TailState {
    /// Byte offset we've read up to. A fresh registration starts at 0 so any
    /// lines already present when the watch begins are read on the first event.
    offset: u64,
    /// Bytes after the last newline we've seen — an under-construction line.
    partial: Vec<u8>,
}

/// Singleton that watches per-terminal CLI-agent event files and routes parsed
/// events to [`CLIAgentSessionsModel`], keyed by terminal view id.
///
/// Mirrors the shape of `MnemosManagedPathsWatcher`: owns a
/// [`BulkFilesystemWatcher`], subscribes to its events, and resolves changed
/// paths back to the terminal they belong to via the token table.
pub struct CLIAgentEventFileIngestor {
    watcher: ModelHandle<BulkFilesystemWatcher>,
    /// token (file stem) -> terminal view id.
    tokens: HashMap<String, EntityId>,
    /// Per-file tail state, keyed by the canonicalized event-file path.
    tails: HashMap<PathBuf, TailState>,
    /// Whether the shared events directory is being watched yet.
    dir_registered: bool,
}

impl Entity for CLIAgentEventFileIngestor {
    type Event = ();
}

impl SingletonEntity for CLIAgentEventFileIngestor {}

impl CLIAgentEventFileIngestor {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let watcher = ctx.add_model(|ctx| {
            BulkFilesystemWatcher::new(
                Duration::from_millis(EVENT_FILE_WATCHER_DEBOUNCE_MILLIS),
                ctx,
            )
        });
        ctx.subscribe_to_model(&watcher, Self::handle_fs_event);
        // Mnemos IDE: keep the untokened-event switch the terminal model reads current.
        {
            use super::event_settings::CliAgentEventSettings;
            use settings::Setting as _;
            let settings = CliAgentEventSettings::handle(ctx);
            super::event::set_accept_untokened_events(
                *settings.as_ref(ctx).accept_untokened_cli_agent_events.value(),
            );
            ctx.subscribe_to_model(&settings, |_, _, ctx| {
                super::event::set_accept_untokened_events(
                    *CliAgentEventSettings::as_ref(ctx)
                        .accept_untokened_cli_agent_events
                        .value(),
                );
            });
        }
        Self {
            watcher,
            tokens: HashMap::new(),
            tails: HashMap::new(),
            dir_registered: false,
        }
    }

    #[cfg(test)]
    pub fn new_for_test(ctx: &mut ModelContext<Self>) -> Self {
        // Tests don't exercise the real watcher, but we still need a registered
        // singleton (and a non-panicking watcher handle) so callers that look it
        // up via `handle(ctx)` work.
        let watcher = ctx.add_model(|_| BulkFilesystemWatcher::new_for_test());
        Self {
            watcher,
            tokens: HashMap::new(),
            tails: HashMap::new(),
            dir_registered: false,
        }
    }

    /// Associates a freshly generated token with the terminal view it belongs
    /// to and begins watching the shared events directory (idempotently).
    ///
    /// Called from `TerminalManager::create_model` once the view id is known.
    /// The event file is created empty here (0600, Mnemos IDE), but a hook may
    /// recreate it, so we watch the directory rather than the file.
    pub fn register(&mut self, token: String, terminal_view_id: EntityId, ctx: &mut ModelContext<Self>) {
        // Mnemos IDE: OSC 777 events carrying this token are trusted from any
        // pane of this app (see `check_cli_agent_title`).
        super::event::register_live_token(&token);
        let event_file = event_file_for_token(&token);
        self.tokens.insert(token, terminal_view_id);
        self.ensure_dir_watched(ctx);
        precreate_event_file(&event_file);
    }

    /// Drops the token mapping, stops tracking the file, and deletes it.
    ///
    /// Called from the terminal pane detach path — but ONLY for non-`Moved`
    /// detaches. A moved session re-registers its (still-running) terminal in
    /// the new tab and keeps writing to the same file, so the file must
    /// survive a move.
    pub fn unregister(&mut self, terminal_view_id: EntityId) {
        let Some(token) = self
            .tokens
            .iter()
            .find(|(_, id)| **id == terminal_view_id)
            .map(|(token, _)| token.clone())
        else {
            return;
        };
        self.tokens.remove(&token);
        super::event::unregister_live_token(&token);

        let path = event_file_for_token(&token);
        self.tails.remove(&canonical_or_self(&path));
        if let Err(err) = std::fs::remove_file(&path) {
            if err.kind() != std::io::ErrorKind::NotFound {
                log::warn!(
                    "Failed to remove CLI agent event file {}: {err}",
                    path.display()
                );
            }
        }
    }

    fn ensure_dir_watched(&mut self, ctx: &mut ModelContext<Self>) {
        if self.dir_registered {
            return;
        }
        let dir = event_files_dir();
        if let Err(err) = std::fs::create_dir_all(&dir) {
            log::warn!(
                "Failed to create CLI agent events directory {}: {err}",
                dir.display()
            );
            return;
        }
        // Mnemos IDE: the file names are the per-terminal tokens that
        // authenticate agent events, so only this user may list them.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if let Err(err) =
                std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            {
                log::warn!(
                    "Failed to restrict CLI agent events directory {}: {err}",
                    dir.display()
                );
            }
        }
        let registration = self.watcher.update(ctx, |watcher, _ctx| {
            watcher.register_path(
                &dir,
                notify_debouncer_full::notify::WatchFilter::accept_all(),
                notify_debouncer_full::notify::RecursiveMode::NonRecursive,
            )
        });
        ctx.spawn(registration, move |_, result, _ctx| {
            if let Err(err) = result {
                log::warn!(
                    "Failed to start watching CLI agent events directory {}: {err}",
                    dir.display()
                );
            }
        });
        self.dir_registered = true;
    }

    fn handle_fs_event(
        &mut self,
        event: &BulkFilesystemWatcherEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        // The watcher only reports changed *paths*, not contents, so for each
        // touched event file we read everything appended since our last offset.
        for path in event.added_or_updated_iter() {
            self.ingest_path(path, ctx);
        }
    }

    /// Resolves a changed path to its terminal, reads newly appended bytes from
    /// the saved offset, and feeds each complete line into the parse pipeline.
    fn ingest_path(&mut self, path: &Path, ctx: &mut ModelContext<Self>) {
        let Some(token) = path.file_stem().and_then(|s| s.to_str()) else {
            return;
        };
        let Some(&terminal_view_id) = self.tokens.get(token) else {
            // Not one of ours (or already unregistered).
            return;
        };

        let key = canonical_or_self(path);
        let offset = self.tails.get(&key).map(|t| t.offset).unwrap_or(0);

        let mut file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(err) => {
                if err.kind() != std::io::ErrorKind::NotFound {
                    log::warn!("Failed to open CLI agent event file {}: {err}", path.display());
                }
                return;
            }
        };

        // Guard against truncation/rotation: if the file shrank below our
        // offset, restart from the beginning.
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        let seek_to = if len < offset { 0 } else { offset };
        if file.seek(SeekFrom::Start(seek_to)).is_err() {
            return;
        }

        let mut fresh = Vec::new();
        if file.read_to_end(&mut fresh).is_err() {
            return;
        }
        let new_offset = seek_to + fresh.len() as u64;

        // Prepend any partial line carried over from the previous read, then
        // append the freshly read bytes.
        let tail = self.tails.entry(key).or_default();
        let mut buf = std::mem::take(&mut tail.partial);
        buf.extend_from_slice(&fresh);

        // Split into complete lines; retain the trailing (possibly partial) one.
        let mut lines: Vec<String> = Vec::new();
        let mut last_newline = 0usize;
        for (i, b) in buf.iter().enumerate() {
            if *b == b'\n' {
                let line = &buf[last_newline..i];
                if let Ok(s) = std::str::from_utf8(line) {
                    let trimmed = s.trim();
                    if !trimmed.is_empty() {
                        lines.push(trimmed.to_owned());
                    }
                }
                last_newline = i + 1;
            }
        }

        // Persist the new offset and the trailing partial back into the same
        // entry we're already holding.
        tail.offset = new_offset;
        tail.partial = buf[last_newline..].to_vec();

        for line in lines {
            // Reuse the exact OSC parse pipeline. The hook writes the same JSON
            // body it would have sent over OSC 777, so we pass the native
            // sentinel as the title.
            let Some(parsed) = parse_event(Some(CLI_AGENT_NOTIFICATION_SENTINEL), &line) else {
                continue;
            };
            // The offset above has already advanced, so a line dropped here as
            // an OSC duplicate is never re-read.
            CLIAgentSessionsModel::handle(ctx).update(ctx, |sessions_model, ctx| {
                sessions_model.apply_ingested_event(terminal_view_id, &parsed, ctx);
            });
        }
    }
}

/// Mnemos IDE: creates the (empty) event file owner-only (0600) before any hook
/// appends to it, so it never exists with the hook's default umask. The events dir
/// is already 0700; this is belt and braces. An existing file is left alone.
fn precreate_event_file(path: &Path) {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(err) => log::warn!(
            "Failed to create CLI agent event file {}: {err}",
            path.display()
        ),
    }
}

/// Canonicalizes a path for use as a map key so watcher-reported paths and our
/// own constructed paths compare equal despite e.g. `\\?\` prefixes or casing.
/// Falls back to the path as-is if canonicalization fails (e.g. file gone).
fn canonical_or_self(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

// SPDX-License-Identifier: AGPL-3.0-only
//! Enumerate and cheaply describe Claude Code session transcripts.
//!
//! Transcripts live at `~/.claude/projects/<slug>/<uuid>.jsonl`, where `<slug>`
//! is the absolute working directory with every character outside
//! `[A-Za-z0-9-]` replaced by `-`. That leaves a **leading dash** for POSIX
//! paths (`/Users/alice/code/tcbv2` -> `-Users-alice-code-tcbv2`), which is why
//! nothing here shells out — a naive glob would read that as a flag.
//!
//! The files are append-only NDJSON and routinely run to **megabytes**, so we
//! never read one whole. [`SessionStore::list_for_cwd`] reads a bounded head of
//! each file and pulls out four things:
//!
//! | field | source record | notes |
//! |---|---|---|
//! | title | first `{"type":"ai-title","aiTitle":…}` | Claude's own summary; best label |
//! | prompt | first non-meta `{"type":"user"}` | fallback label |
//! | cwd | any record's `cwd` | authoritative — the slug is lossy |
//! | branch | any record's `gitBranch` | shown as a pill |
//!
//! Big records (`assistant`, `attachment`, `file-history-*`) are skipped
//! without being handed to `serde_json` at all: a substring scan lifts the
//! `"type"` value first, and only small, interesting records get parsed. The
//! result is cached by `(mtime, len)` so re-listing an unchanged directory is
//! a `stat` per file.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Hard cap on sessions listed for one directory. The list is a sidebar
/// section, not an archive browser; beyond this it stops being scannable and
/// starts costing real layout time (a plain column re-renders every row every
/// frame).
pub const MAX_SESSIONS: usize = 40;

/// Never read more than this from the head of a transcript.
const HEAD_BYTES_CAP: u64 = 512 * 1024;
/// …nor more than this many lines, whichever comes first.
const HEAD_LINES_CAP: usize = 600;
/// Records larger than this are never handed to `serde_json`. Genuine `user`
/// records are small; anything bigger is a paste blob we don't want to parse.
const MAX_PARSE_LINE: usize = 64 * 1024;
/// Soft cap for a label. Real ellipsis happens at layout; this just stops a
/// pathological multi-KB prompt from being carried around.
const LABEL_MAX_CHARS: usize = 120;

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("could not resolve the home directory")]
    MissingHomeDir,
    #[error("no Claude session directory for this working directory: {0}")]
    NoProjectDir(PathBuf),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// One Claude Code session, described well enough to label and resume it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionMeta {
    /// Session UUID — the file stem, and the argument to `claude --resume`.
    pub id: String,
    /// Absolute path of the `.jsonl` transcript.
    pub path: PathBuf,
    /// Working directory the session ran in, read from the transcript rather
    /// than reversed out of the slug (which loses `/` vs `-` and non-ASCII).
    pub cwd: Option<PathBuf>,
    /// Git branch recorded on the session's records, when there was one.
    pub git_branch: Option<String>,
    /// Best available label: Claude's `ai-title`, else the first user prompt,
    /// else the short id.
    pub title: String,
    /// True when `title` came from an `ai-title` record rather than a prompt.
    pub title_is_ai: bool,
    /// Transcript mtime — the list sorts on this, newest first.
    pub modified: SystemTime,
    /// Transcript size in bytes. Stands in for a message count, which can't be
    /// had without reading the whole file.
    pub size_bytes: u64,
}

impl SessionMeta {
    /// First 8 characters of the UUID — enough to tell sessions apart by eye.
    pub fn short_id(&self) -> &str {
        short_prefix(&self.id)
    }
}

/// First 8 characters of `s`, sliced on a char boundary.
fn short_prefix(s: &str) -> &str {
    let end = s.char_indices().nth(8).map_or(s.len(), |(i, _)| i);
    &s[..end]
}

/// `~/.claude`, honouring `CLAUDE_CONFIG_DIR` the same way the CLI does.
fn claude_config_dir() -> Result<PathBuf, SessionError> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        if !dir.is_empty() {
            return Ok(PathBuf::from(dir));
        }
    }
    dirs::home_dir()
        .map(|home| home.join(".claude"))
        .ok_or(SessionError::MissingHomeDir)
}

/// `~/.claude/projects` — the directory holding one slug-named folder per
/// working directory Claude Code has ever run in.
pub fn claude_projects_dir() -> Result<PathBuf, SessionError> {
    Ok(claude_config_dir()?.join("projects"))
}

/// Claude Code's folder name under `~/.claude/projects` for an absolute
/// path: every character outside `[A-Za-z0-9]` becomes `-` (two for a
/// character outside the Basic Multilingual Plane, as Claude Code counts
/// UTF-16 units), and a trailing separator is ignored. This is
/// [`mnemos_ide_core::paths::claude_project_dir_name`], the one rule the sync
/// code files pulled transcripts under, so a listed folder is always the
/// folder a pull wrote.
///
/// `/Users/alice/code/tcbv2`        -> `-Users-alice-code-tcbv2`
/// `/Users/alice/code/Admin/.claude` -> `-Users-alice-code-Admin--claude`
/// `C:\Users\alice\code`            -> `C--Users-alice-code`
///
/// A path whose name would pass Claude Code's 200 character limit has no
/// plain folder; it gets the unbounded name, which simply matches nothing.
pub fn project_slug(path: &Path) -> String {
    slug_if_plain(path).unwrap_or_else(|| {
        path.to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect()
    })
}

/// [`project_slug`], or `None` when Claude Code would not use a plain name.
fn slug_if_plain(path: &Path) -> Option<String> {
    mnemos_ide_core::paths::claude_project_dir_name(path)
}

/// Whether `path` is `root` or lies under it, compared by whole components
/// (`/code/foo` is not under `/code/fo`). Case-insensitive on Windows, where
/// `C:\Code` and `c:\code` are the same folder.
pub fn is_within(path: &Path, root: &Path) -> bool {
    if path.starts_with(root) {
        return true;
    }
    if !cfg!(windows) {
        return false;
    }
    let mut path_parts = path.components();
    for root_part in root.components() {
        let Some(part) = path_parts.next() else {
            return false;
        };
        let (a, b) = (part.as_os_str().to_string_lossy(), root_part.as_os_str().to_string_lossy());
        if !a.eq_ignore_ascii_case(&b) {
            return false;
        }
    }
    true
}

/// True when `id` is safe to pass to `claude --resume` on a shell command
/// line — i.e. it looks like the UUID Claude Code names transcripts after.
///
/// The id originates as a *filename stem*, so anything in the transcript
/// directory would otherwise end up interpolated into a command. Nothing
/// legitimate is rejected: Claude Code always names sessions by UUID.
pub fn is_safe_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// Lists sessions for a working directory, caching parsed metadata by
/// `(mtime, len)` so an unchanged transcript is never re-read.
#[derive(Clone, Debug, Default)]
pub struct SessionStore {
    /// Override for `~/.claude/projects`. Set in tests.
    projects_dir: Option<PathBuf>,
    cache: HashMap<PathBuf, CacheEntry>,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    modified: SystemTime,
    size_bytes: u64,
    meta: SessionMeta,
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Point the store at an explicit `projects` directory instead of
    /// `~/.claude/projects`.
    pub fn with_projects_dir(dir: PathBuf) -> Self {
        Self {
            projects_dir: Some(dir),
            cache: HashMap::new(),
        }
    }

    fn projects_dir(&self) -> Result<PathBuf, SessionError> {
        match &self.projects_dir {
            Some(dir) => Ok(dir.clone()),
            None => claude_projects_dir(),
        }
    }

    /// Sessions recorded for `cwd`, newest first, capped at [`MAX_SESSIONS`].
    ///
    /// Returns `NoProjectDir` when Claude Code has never run there; that's a
    /// normal empty state, not a failure.
    pub fn list_for_cwd(&mut self, cwd: &Path) -> Result<Vec<SessionMeta>, SessionError> {
        let dir = self.projects_dir()?.join(project_slug(cwd));
        if !dir.is_dir() {
            return Err(SessionError::NoProjectDir(dir));
        }
        let mut out = self.list_dir(&dir)?;
        sort_newest_first(&mut out);
        out.truncate(MAX_SESSIONS);
        Ok(out)
    }

    /// Sessions started in `root` or anywhere under it, newest first, at most
    /// `cap`.
    ///
    /// Claude Code files a session under the folder it was launched in, so a
    /// session started in `root/app` lives in `slug(root) + "-app"`. This reads
    /// the root's own folder plus every folder whose name starts with
    /// `slug(root) + "-"`. Because the slug is lossy (`foo-bar`, `foo.bar` and
    /// `foo/bar` all become `foo-bar`), a session from one of those folders is
    /// kept only when its recorded `cwd` lies under `root`, compared by whole
    /// path components. A session with no recorded `cwd` counts only in the
    /// root's own folder.
    ///
    /// A session id found in two folders (a session moved between machines
    /// can leave a copy in each) is listed once, the newest copy winning:
    /// that is the one Claude Code writes and the sync pushes.
    ///
    /// No folder at all is an empty list, not an error.
    pub fn list_for_root(&mut self, root: &Path, cap: usize) -> Result<Vec<SessionMeta>, SessionError> {
        let projects = self.projects_dir()?;
        let Some(slug) = slug_if_plain(root) else {
            return Ok(Vec::new());
        };
        let prefix = format!("{slug}-");
        let entries = match std::fs::read_dir(&projects) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut dirs: Vec<(PathBuf, bool)> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let exact = name == slug;
            if !exact && !name.starts_with(&prefix) {
                continue;
            }
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                dirs.push((entry.path(), exact));
            }
        }

        let mut by_id: HashMap<String, SessionMeta> = HashMap::new();
        for (dir, exact) in dirs {
            for meta in self.list_dir(&dir)? {
                let belongs = match &meta.cwd {
                    Some(cwd) => is_within(cwd, root),
                    None => exact,
                };
                if !belongs {
                    continue;
                }
                match by_id.get(&meta.id) {
                    Some(have) if have.modified >= meta.modified => {}
                    _ => {
                        by_id.insert(meta.id.clone(), meta);
                    }
                }
            }
        }
        let mut out: Vec<SessionMeta> = by_id.into_values().collect();
        sort_newest_first(&mut out);
        out.truncate(cap);
        Ok(out)
    }

    /// Every transcript directly in `dir`, unsorted, described from the cache
    /// when its `(mtime, len)` is unchanged.
    fn list_dir(&mut self, dir: &Path) -> Result<Vec<SessionMeta>, SessionError> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    tracing::debug!(target: "mnemos_ide_sessions", "skipping unreadable entry: {e}");
                    continue;
                }
            };
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(file_meta) = entry.metadata() else {
                continue;
            };
            if !file_meta.is_file() {
                continue;
            }
            let modified = file_meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let size_bytes = file_meta.len();

            if let Some(hit) = self.cache.get(&path) {
                if hit.modified == modified && hit.size_bytes == size_bytes {
                    out.push(hit.meta.clone());
                    continue;
                }
            }

            match read_session_head(&path, modified, size_bytes) {
                Ok(meta) => {
                    self.cache.insert(
                        path,
                        CacheEntry {
                            modified,
                            size_bytes,
                            meta: meta.clone(),
                        },
                    );
                    out.push(meta);
                }
                Err(e) => {
                    tracing::debug!(target: "mnemos_ide_sessions", "skipping {}: {e}", path.display());
                }
            }
        }
        Ok(out)
    }

    /// Like [`Self::list_for_cwd`], but walks up from `start` until it finds a
    /// directory Claude Code has transcripts for, and reports which one it
    /// matched.
    ///
    /// The terminal's `pwd` is whatever the user last `cd`'d to, while the
    /// slug is keyed on the directory `claude` was *launched* in — usually the
    /// repo root. Without this, opening the picker after `cd src` would show
    /// an empty list for a repo full of sessions.
    pub fn list_for_cwd_or_ancestor(
        &mut self,
        start: &Path,
    ) -> Result<(PathBuf, Vec<SessionMeta>), SessionError> {
        let mut first_error = None;
        for dir in start.ancestors() {
            match self.list_for_cwd(dir) {
                // An existing-but-empty directory still counts as a match:
                // that's the answer for this project, not a reason to climb
                // into the parent's sessions.
                Ok(sessions) => return Ok((dir.to_path_buf(), sessions)),
                Err(e) => {
                    if first_error.is_none() {
                        first_error = Some(e);
                    }
                }
            }
        }
        Err(first_error.unwrap_or_else(|| SessionError::NoProjectDir(start.to_path_buf())))
    }

    /// Drop cached metadata. Only needed if a transcript is rewritten in place
    /// without its mtime or length changing.
    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }
}

/// Newest first. Ties (same mtime) fall back to id so the order is stable
/// across refreshes rather than following readdir order.
fn sort_newest_first(sessions: &mut [SessionMeta]) {
    sessions.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.id.cmp(&b.id)));
}

/// Reads a bounded head of one transcript and builds its [`SessionMeta`].
pub fn read_session_head(
    path: &Path,
    modified: SystemTime,
    size_bytes: u64,
) -> Result<SessionMeta, SessionError> {
    let id = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let file = File::open(path)?;
    let mut reader = BufReader::new(file.take(HEAD_BYTES_CAP));

    let mut ai_title: Option<String> = None;
    let mut prompt: Option<String> = None;
    let mut cwd: Option<PathBuf> = None;
    let mut git_branch: Option<String> = None;

    let mut line = String::new();
    for _ in 0..HEAD_LINES_CAP {
        line.clear();
        // Non-UTF8 bytes in the middle of the head shouldn't kill the whole
        // record — bail out with whatever we already collected.
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // `cwd` / `gitBranch` ride along on almost every record, including the
        // huge ones. Lift them with a substring scan so we never parse those.
        if cwd.is_none() {
            cwd = json_str_field(trimmed, "cwd").map(PathBuf::from);
        }
        if git_branch.is_none() {
            git_branch = json_str_field(trimmed, "gitBranch").filter(|s| !s.is_empty());
        }

        match json_str_field(trimmed, "type").as_deref() {
            Some("ai-title") if ai_title.is_none() => {
                // A long session accumulates several of these as the topic
                // drifts. We deliberately keep the *first*: it describes what
                // the session was opened to do, which is what you scan a
                // picker for — and taking the last one would mean reading the
                // whole head budget of every transcript on every refresh.
                if let Some(t) = json_str_field(trimmed, "aiTitle") {
                    let t = clean_label(&t);
                    if !t.is_empty() {
                        ai_title = Some(t);
                    }
                }
            }
            Some("user") if prompt.is_none() && trimmed.len() <= MAX_PARSE_LINE => {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    if value.get("isMeta").and_then(|v| v.as_bool()) == Some(true) {
                        continue;
                    }
                    if let Some(text) = user_message_text(&value) {
                        prompt = prompt_label(&text);
                    }
                }
            }
            _ => {}
        }

        if ai_title.is_some() && cwd.is_some() && git_branch.is_some() {
            break;
        }
    }

    let title_is_ai = ai_title.is_some();
    let title = ai_title
        .or(prompt)
        .unwrap_or_else(|| format!("session {}", short_prefix(&id)));

    Ok(SessionMeta {
        id,
        path: path.to_path_buf(),
        cwd,
        git_branch,
        title,
        title_is_ai,
        modified,
        size_bytes,
    })
}

/// Flattens a `user` record's `message.content` into plain text. Content is
/// either a bare string or an array of typed blocks; only `text` blocks
/// contribute.
fn user_message_text(value: &serde_json::Value) -> Option<String> {
    let content = value.get("message")?.get("content")?;
    if let Some(s) = content.as_str() {
        return Some(s.to_owned());
    }
    let parts: Vec<&str> = content
        .as_array()?
        .iter()
        .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

/// Turns a raw first prompt into a label, or `None` when the record is
/// scaffolding rather than something the user typed.
///
/// Slash commands arrive as an XML-ish envelope
/// (`<command-name>/prime</command-name><command-args>…`), and the *args* are
/// the interesting part — `"/prime"` alone describes nothing.
fn prompt_label(raw: &str) -> Option<String> {
    let text = raw.trim();
    if text.is_empty() {
        return None;
    }
    // Injected by the CLI ahead of local-command output; never user intent.
    if text.starts_with("<local-command-caveat>") {
        return None;
    }

    let name = tag_body(text, "command-name").map(|n| n.trim().trim_start_matches('/').to_owned());
    let args = tag_body(text, "command-args").map(|a| a.trim().to_owned());
    match (name, args) {
        (Some(name), Some(args)) if !args.is_empty() => Some(clean_label(&format!("/{name} {args}"))),
        (Some(name), _) if !name.is_empty() => Some(clean_label(&format!("/{name}"))),
        _ => {
            let cleaned = clean_label(text);
            (!cleaned.is_empty()).then_some(cleaned)
        }
    }
}

/// Inner text of `<tag>…</tag>`, if present.
fn tag_body<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(&text[start..end])
}

/// Collapses whitespace and truncates to [`LABEL_MAX_CHARS`].
fn clean_label(text: &str) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= LABEL_MAX_CHARS {
        return flat;
    }
    let mut out: String = flat.chars().take(LABEL_MAX_CHARS.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

/// Pulls a top-level JSON string field out of a record without parsing it.
///
/// Deliberately dumb: it finds `"<name>":` and, if a string follows, decodes
/// that string with `serde_json` so escapes are handled correctly. A match
/// nested inside another object is possible in principle, but the fields we
/// use this for (`type`, `cwd`, `gitBranch`, `aiTitle`) are top-level in
/// Claude Code's records, and the first occurrence wins.
fn json_str_field(line: &str, name: &str) -> Option<String> {
    let needle = format!("\"{name}\":");
    let mut from = 0usize;
    while let Some(rel) = line[from..].find(&needle) {
        let after = from + rel + needle.len();
        let rest = line[after..].trim_start();
        if !rest.starts_with('"') {
            // Non-string value (or a key that merely ends with `name`).
            from = after;
            continue;
        }
        let offset = line.len() - rest.len();
        if let Some(end) = json_string_end(line, offset) {
            if let Ok(decoded) = serde_json::from_str::<String>(&line[offset..=end]) {
                return Some(decoded);
            }
        }
        from = after;
    }
    None
}

/// Index of the closing quote of the JSON string starting at `start`
/// (which must be the opening quote), honouring backslash escapes.
fn json_string_end(line: &str, start: usize) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return Some(i),
            _ => i += 1,
        }
    }
    None
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod model_tests;

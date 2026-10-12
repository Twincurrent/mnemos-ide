// SPDX-License-Identifier: AGPL-3.0-only
//! The synced project registry (`projects.json`, sync key `projects`): pure
//! merge / normalize rules shared by the app, `mnemos-ide-projects` and `mnemos-ide-sync`.
//!
//! Everything works on `serde_json::Value` rather than typed structs so that
//! fields this build does not model (written by v1, the admin frontend or a
//! newer build) round-trip untouched through a merge. No I/O.
//!
//! Why a real merge instead of last-writer-wins: the registry is edited on
//! several machines (a Mac and a Windows PC) and the old whole-blob sync let
//! the last writer erase records the other machine had added. The rules here
//! never drop a record just because one side lacks it; removal needs a
//! tombstone in `deletedProjects`.
//!
//! Record shape (only the fields these rules read):
//!
//! ```jsonc
//! {
//!   "id": "...", "name": "...",
//!   "path": "<path on the machine that registered it>",   // legacy
//!   "paths": { "darwin": "...", "win32": "...", "linux": "..." },
//!   "gitRepo": "owner/repo",
//!   "open": true, "openChangedAt": "RFC3339",
//!   "createdAt": "RFC3339", "updatedAt": "RFC3339"
//! }
//! ```
//!
//! Timestamps are RFC3339 strings. Anything missing or unparseable counts as
//! the oldest possible time, so a malformed clock can never win a tie-break.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Map, Value};

/// Top-level key holding the record array.
pub const PROJECTS_KEY: &str = "projects";
/// Top-level key holding `{ "<id>": { "deletedAt", "replacedBy"? } }`.
pub const DELETED_KEY: &str = "deletedProjects";
/// The only keys `paths` uses. Same names as Node's `process.platform`, which
/// is what v1 and `mnemos-ide-sync` already wrote.
pub const PLATFORMS: [&str; 3] = ["darwin", "win32", "linux"];

/// `replacedBy` chains longer than this are treated as broken (or a cycle).
const MAX_REPLACED_HOPS: usize = 16;
/// Claude Code refuses to use a project dir name longer than this; we skip
/// such paths rather than invent a name Claude would never read.
const MAX_SLUG_LEN: usize = 200;

// ─────────────────────────────────────────────────────────────────────────────
// Platforms and path strings
// ─────────────────────────────────────────────────────────────────────────────

/// The `paths` slot this build writes: `"darwin"`, `"win32"` or `"linux"`.
pub fn current_platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "win32"
    } else {
        "linux"
    }
}

/// Which platform a bare path belongs to, judged by its shape alone: a drive
/// letter or UNC prefix is Windows, `/Users/...` is macOS, any other absolute
/// `/...` path is Linux. Relative or empty paths have no platform.
///
/// Legacy records only carry `path` (the registering machine's path), so this
/// is how we tell whether that path is usable on the current machine.
pub fn platform_of_path(path: &str) -> Option<&'static str> {
    let b = path.as_bytes();
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
    {
        return Some("win32");
    }
    if path.starts_with("\\\\") {
        return Some("win32");
    }
    if path.starts_with("/Users/") {
        return Some("darwin");
    }
    if path.starts_with('/') {
        return Some("linux");
    }
    None
}

fn is_win(platform: &str) -> bool {
    platform == "win32"
}

fn is_sep_for(platform: &str) -> impl Fn(char) -> bool + '_ {
    move |c: char| c == '/' || (is_win(platform) && c == '\\')
}

/// Comparison key for a path on `platform`: separators unified (Windows),
/// trailing separators trimmed, and case folded where the platform's default
/// filesystem is case-insensitive (Windows and macOS).
fn path_key(path: &str, platform: &str) -> String {
    let win = is_win(platform);
    let mut s = if win {
        strip_verbatim(&path.replace('/', "\\"))
    } else {
        path.to_string()
    };
    let sep = if win { '\\' } else { '/' };
    while s.len() > 1 && s.ends_with(sep) {
        s.pop();
    }
    if win || platform == "darwin" {
        s.to_lowercase()
    } else {
        s
    }
}

/// A Windows path without its verbatim prefix: `\\?\C:\x` -> `C:\x` and
/// `\\?\UNC\srv\share` -> `\\srv\share` (what `canonicalize` returns on
/// Windows). Anything else is returned unchanged.
pub fn strip_verbatim(path: &str) -> String {
    let Some(rest) = path.strip_prefix(r"\\?\") else {
        return path.to_string();
    };
    match rest.get(..4) {
        Some(unc) if unc.eq_ignore_ascii_case(r"UNC\") => format!(r"\\{}", &rest[4..]),
        _ => rest.to_string(),
    }
}

/// Whether two path strings name the same directory on `platform`. Trailing
/// separators never matter; case only matters on Linux.
pub fn same_path(a: &str, b: &str, platform: &str) -> bool {
    path_key(a, platform) == path_key(b, platform)
}

/// Whether `path` is a filesystem root on `platform` (`/`, `C:\`, `\`).
pub fn is_fs_root(path: &str, platform: &str) -> bool {
    let key = path_key(path, platform);
    if is_win(platform) {
        let b = key.as_bytes();
        (b.len() == 2 && b[0].is_ascii_alphabetic() && b[1] == b':') || key == "\\"
    } else {
        key == "/" || key.is_empty()
    }
}

/// Parent directory of `path` using `platform`'s separators. String based so
/// a test on Windows can walk Mac paths (and vice versa) exactly as the other
/// machine would. `None` at a filesystem root.
pub fn path_parent(path: &str, platform: &str) -> Option<String> {
    if path.is_empty() || is_fs_root(path, platform) {
        return None;
    }
    let is_sep = is_sep_for(platform);
    let trimmed = path.trim_end_matches(&is_sep);
    if trimmed.is_empty() {
        return None;
    }
    let idx = trimmed.rfind(&is_sep)?;
    let parent = &trimmed[..idx];
    let at_root = parent.is_empty()
        || (is_win(platform) && parent.len() == 2 && parent.as_bytes()[1] == b':');
    if at_root {
        // Keep the separator so the root stays a root (`/`, `C:\`).
        return Some(trimmed[..=idx].to_string());
    }
    Some(parent.to_string())
}

/// Last component of `path` on `platform`. `None` for roots and empty paths.
pub fn path_file_name(path: &str, platform: &str) -> Option<String> {
    if is_fs_root(path, platform) {
        return None;
    }
    let is_sep = is_sep_for(platform);
    let trimmed = path.trim_end_matches(&is_sep);
    let name = match trimmed.rfind(&is_sep) {
        Some(idx) => &trimmed[idx + 1..],
        None => trimmed,
    };
    if name.is_empty() || name.ends_with(':') {
        None
    } else {
        Some(name.to_string())
    }
}

/// `base` joined with `name` using `platform`'s separator.
pub fn path_join(base: &str, name: &str, platform: &str) -> String {
    let sep = if is_win(platform) { '\\' } else { '/' };
    if base.ends_with(is_sep_for(platform)) {
        format!("{base}{name}")
    } else {
        format!("{base}{sep}{name}")
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Git remotes and Claude project slugs
// ─────────────────────────────────────────────────────────────────────────────

/// `owner/repo` for a git remote URL, matching the `gitRepo` shape the admin
/// frontend writes into `projects.json`: the last two path segments, `.git`
/// stripped. `git@github.com:acme/Admin.git` -> `acme/Admin`;
/// `https://dev.azure.com/o/p/_git/globex` -> `_git/globex`.
pub fn normalize_git_remote(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    let parts: Vec<&str> = trimmed
        .split(['/', ':'])
        .filter(|s| !s.is_empty())
        .collect();
    if parts.len() < 2 {
        return None;
    }
    Some(format!("{}/{}", parts[parts.len() - 2], parts[parts.len() - 1]))
}

/// Compare key for a `gitRepo` value. GitHub owner/repo names are
/// case-insensitive, and v1 wrote whatever case the remote URL had.
pub fn git_repo_key(repo: &str) -> String {
    repo.trim().to_lowercase()
}

/// Claude Code's directory name under `~/.claude/projects/` for a working
/// directory: every character outside `[A-Za-z0-9]` becomes `-`.
///
/// Claude Code does this with a JavaScript regex over UTF-16 code units, so a
/// character outside the Basic Multilingual Plane becomes two dashes; we
/// match that. Returns `None` when the result is longer than 200 characters,
/// where Claude Code stops using the plain name.
pub fn claude_project_slug(path: &Path) -> Option<String> {
    let raw = path.to_string_lossy();
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else {
            for _ in 0..c.len_utf16() {
                out.push('-');
            }
        }
    }
    if out.is_empty() || out.len() > MAX_SLUG_LEN {
        None
    } else {
        Some(out)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Timestamps
// ─────────────────────────────────────────────────────────────────────────────

/// Parse an RFC3339 timestamp. `None` means "oldest" to every rule here.
pub fn parse_timestamp(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s.trim())
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// The one timestamp format this crate writes. Same as chrono's serde output
/// for `DateTime<Utc>`, so typed and untyped writers agree byte for byte.
pub fn format_timestamp(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::AutoSi, true)
}

// ─────────────────────────────────────────────────────────────────────────────
// Record accessors
// ─────────────────────────────────────────────────────────────────────────────

fn rec_id(rec: &Value) -> Option<&str> {
    rec.get("id").and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn str_field<'a>(rec: &'a Value, key: &str) -> Option<&'a str> {
    rec.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

fn ts_field(rec: &Value, key: &str) -> Option<DateTime<Utc>> {
    parse_timestamp(rec.get(key)?.as_str()?)
}

/// The clock that picks the merge base: `updatedAt`, else `createdAt`.
fn record_updated(rec: &Value) -> Option<DateTime<Utc>> {
    ts_field(rec, "updatedAt").or_else(|| ts_field(rec, "createdAt"))
}

/// Newest of every clock a record carries. A tombstone must be newer than
/// this to remove the record.
fn record_latest_clock(rec: &Value) -> Option<DateTime<Utc>> {
    [
        ts_field(rec, "updatedAt"),
        ts_field(rec, "openChangedAt"),
        ts_field(rec, "createdAt"),
    ]
    .into_iter()
    .max()
    .flatten()
}

fn paths_map(rec: &Value) -> BTreeMap<String, String> {
    rec.get("paths")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| {
                    v.as_str()
                        .filter(|s| !s.is_empty())
                        .map(|s| (k.clone(), s.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The path a record has on `platform`, without touching the filesystem:
/// `paths[platform]` when set, else the legacy `path` when its shape belongs
/// to `platform`, else `None`.
pub fn record_local_path(record: &Value, platform: &str) -> Option<String> {
    if let Some(p) = record
        .get("paths")
        .and_then(|m| m.get(platform))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Some(p.to_string());
    }
    let bare = str_field(record, "path")?;
    (platform_of_path(bare) == Some(platform)).then(|| bare.to_string())
}

/// Every platform a record claims a checkout on: its `paths` slots, plus the
/// bare `path` under its shape platform when that slot is empty.
fn record_claims(rec: &Value) -> BTreeMap<String, String> {
    let mut claims = paths_map(rec);
    if let Some(bare) = str_field(rec, "path") {
        if let Some(platform) = platform_of_path(bare) {
            claims
                .entry(platform.to_string())
                .or_insert_with(|| bare.to_string());
        }
    }
    claims
}

/// Two claim sets can belong to one project when every platform both claim
/// points at the same directory.
fn claims_compatible(a: &BTreeMap<String, String>, b: &BTreeMap<String, String>) -> bool {
    a.iter().all(|(platform, pa)| match b.get(platform) {
        Some(pb) => same_path(pa, pb, platform),
        None => true,
    })
}

/// Key-sorted serialization. Only used to break exact clock ties the same
/// way on every machine, whatever map ordering serde_json was built with.
fn canonical(v: &Value) -> String {
    fn write(v: &Value, out: &mut String) {
        match v {
            Value::Object(m) => {
                out.push('{');
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                for (i, k) in keys.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&Value::String(k.clone()).to_string());
                    out.push(':');
                    write(&m[k], out);
                }
                out.push('}');
            }
            Value::Array(a) => {
                out.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(x, out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut s = String::new();
    write(v, &mut s);
    s
}

// ─────────────────────────────────────────────────────────────────────────────
// Merge
// ─────────────────────────────────────────────────────────────────────────────

/// `open` / `openChangedAt` are their own last-writer-wins register: the side
/// with the later `openChangedAt` wins, a missing clock loses, and with no
/// clock on either side the base keeps its value.
fn apply_open_rule(out: &mut Map<String, Value>, base: &Value, other: &Value) {
    let bc = ts_field(base, "openChangedAt");
    let oc = ts_field(other, "openChangedAt");
    let other_wins = match (bc, oc) {
        (Some(b), Some(o)) => o > b,
        (None, Some(_)) => true,
        _ => false,
    };
    if other_wins {
        for key in ["open", "openChangedAt"] {
            if let Some(v) = other.get(key) {
                out.insert(key.to_string(), v.clone());
            }
        }
    }
}

/// Merge two versions of the same record.
fn merge_records(local: &Value, remote: &Value, platform: &str) -> Value {
    // Later `updatedAt` is the base. An exact tie is broken by content so both
    // machines pick the same base; "tie -> local" would let two machines keep
    // their own copy and push it back at each other forever.
    let local_is_base = match record_updated(local).cmp(&record_updated(remote)) {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => canonical(local) >= canonical(remote),
    };
    let (base, other) = if local_is_base {
        (local, remote)
    } else {
        (remote, local)
    };
    let (Some(bo), Some(oo)) = (base.as_object(), other.as_object()) else {
        return base.clone();
    };
    let mut out = bo.clone();
    for (k, v) in oo {
        if !out.contains_key(k) {
            out.insert(k.clone(), v.clone());
        }
    }

    // Each machine owns its own slot: this platform's slot comes from local,
    // every other slot from remote (the machine that actually wrote it).
    let lp = paths_map(local);
    let rp = paths_map(remote);
    if !lp.is_empty() || !rp.is_empty() {
        let slots: BTreeSet<&String> = lp.keys().chain(rp.keys()).collect();
        let mut merged = Map::new();
        for slot in slots {
            let (first, second) = if slot == platform {
                (lp.get(slot), rp.get(slot))
            } else {
                (rp.get(slot), lp.get(slot))
            };
            if let Some(v) = first.or(second) {
                merged.insert(slot.clone(), Value::String(v.clone()));
            }
        }
        out.insert("paths".to_string(), Value::Object(merged));
    }

    apply_open_rule(&mut out, base, other);

    if str_field(base, "gitRepo").is_none() {
        if let Some(g) = str_field(other, "gitRepo") {
            out.insert("gitRepo".to_string(), Value::String(g.to_string()));
        }
    }
    Value::Object(out)
}

/// Merge two tombstones for the same id: the later `deletedAt` wins, and a
/// `replacedBy` from either side survives.
fn merge_tombstone_entries(a: &Value, b: &Value) -> Value {
    let a_wins = match ts_field(a, "deletedAt").cmp(&ts_field(b, "deletedAt")) {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => canonical(a) >= canonical(b),
    };
    let (w, l) = if a_wins { (a, b) } else { (b, a) };
    let mut out = w.as_object().cloned().unwrap_or_default();
    if let Some(lo) = l.as_object() {
        for (k, v) in lo {
            if !out.contains_key(k) {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    if str_field(w, "replacedBy").is_none() {
        if let Some(r) = str_field(l, "replacedBy") {
            out.insert("replacedBy".to_string(), Value::String(r.to_string()));
        }
    }
    Value::Object(out)
}

fn merge_tombstones(a: Option<&Value>, b: Option<&Value>) -> Map<String, Value> {
    let mut out = a.and_then(Value::as_object).cloned().unwrap_or_default();
    if let Some(bo) = b.and_then(Value::as_object) {
        for (id, entry) in bo {
            let merged = match out.get(id) {
                Some(existing) => merge_tombstone_entries(existing, entry),
                None => entry.clone(),
            };
            out.insert(id.clone(), merged);
        }
    }
    out
}

fn records_of(obj: &Map<String, Value>) -> &[Value] {
    obj.get(PROJECTS_KEY)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// Merge two registries. Used for both directions of the `projects` sync key:
/// push (`local` = disk, `remote` = server) and pull (same roles).
///
/// - Records are unioned by id; a record present on only one side is kept.
/// - Same id on both sides: the later `updatedAt` is the base, `open` follows
///   the later `openChangedAt`, `paths` merge per slot (this `platform`'s slot
///   from local, the others from remote), `gitRepo` is the base's unless empty,
///   and any field the base lacks is filled from the other side.
/// - `deletedProjects` are unioned (later `deletedAt` wins per id).
/// - Other top-level keys: local's, filled from remote.
/// - Record order: local's order, then remote-only records in remote order.
///
/// The result is passed through [`normalize`], which applies tombstones and
/// folds duplicate registrations of one repo together.
pub fn merge(local: &Value, remote: &Value, platform: &str) -> Value {
    merge_keeping_apart(local, remote, platform, &HashSet::new())
}

/// [`merge`], then [`normalize_keeping_apart`] instead of [`normalize`]: the
/// records in `keep_apart` are never folded into or with another record.
pub fn merge_keeping_apart(
    local: &Value,
    remote: &Value,
    platform: &str,
    keep_apart: &HashSet<String>,
) -> Value {
    let empty = Map::new();
    let lo = local.as_object().unwrap_or(&empty);
    let ro = remote.as_object().unwrap_or(&empty);

    let mut out = lo.clone();
    for (k, v) in ro {
        if !out.contains_key(k) {
            out.insert(k.clone(), v.clone());
        }
    }

    let local_records = records_of(lo);
    let remote_records = records_of(ro);
    let mut remote_by_id: HashMap<&str, &Value> = HashMap::new();
    for rec in remote_records {
        if let Some(id) = rec_id(rec) {
            remote_by_id.entry(id).or_insert(rec);
        }
    }
    let local_ids: HashSet<&str> = local_records.iter().filter_map(rec_id).collect();

    let mut projects: Vec<Value> = Vec::with_capacity(local_records.len() + remote_records.len());
    for rec in local_records {
        match rec_id(rec).and_then(|id| remote_by_id.get(id)) {
            Some(remote_rec) => projects.push(merge_records(rec, remote_rec, platform)),
            None => projects.push(rec.clone()),
        }
    }
    let mut appended: HashSet<&str> = HashSet::new();
    for rec in remote_records {
        match rec_id(rec) {
            Some(id) => {
                if !local_ids.contains(id) && appended.insert(id) {
                    projects.push(rec.clone());
                }
            }
            None => {
                if !projects.contains(rec) {
                    projects.push(rec.clone());
                }
            }
        }
    }
    out.insert(PROJECTS_KEY.to_string(), Value::Array(projects));

    let tombs = merge_tombstones(lo.get(DELETED_KEY), ro.get(DELETED_KEY));
    if !tombs.is_empty() || out.contains_key(DELETED_KEY) {
        out.insert(DELETED_KEY.to_string(), Value::Object(tombs));
    }
    normalize_keeping_apart(&Value::Object(out), keep_apart)
}

// ─────────────────────────────────────────────────────────────────────────────
// Normalize
// ─────────────────────────────────────────────────────────────────────────────

/// Fold `loser` into `survivor`: missing fields filled, claims unioned into
/// `paths`, `open` by clock, `gitRepo` kept unless empty, `updatedAt` the
/// later of the two.
fn absorb(survivor: &Value, loser: &Value) -> Value {
    let (Some(so), Some(lo)) = (survivor.as_object(), loser.as_object()) else {
        return survivor.clone();
    };
    let mut out = so.clone();
    for (k, v) in lo {
        if !out.contains_key(k) {
            out.insert(k.clone(), v.clone());
        }
    }
    let mut claims = record_claims(survivor);
    for (platform, path) in record_claims(loser) {
        claims.entry(platform).or_insert(path);
    }
    if !claims.is_empty() {
        let paths: Map<String, Value> = claims
            .into_iter()
            .map(|(k, v)| (k, Value::String(v)))
            .collect();
        out.insert("paths".to_string(), Value::Object(paths));
    }
    apply_open_rule(&mut out, survivor, loser);
    if str_field(survivor, "gitRepo").is_none() {
        if let Some(g) = str_field(loser, "gitRepo") {
            out.insert("gitRepo".to_string(), Value::String(g.to_string()));
        }
    }
    if ts_field(loser, "updatedAt") > ts_field(survivor, "updatedAt") {
        if let Some(u) = loser.get("updatedAt") {
            out.insert("updatedAt".to_string(), u.clone());
        }
    }
    Value::Object(out)
}

/// A hand-edited or half-merged file can carry the same id twice. Keep the
/// first position and fold later copies into it.
fn collapse_duplicate_ids(records: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(records.len());
    let mut pos: HashMap<String, usize> = HashMap::new();
    for rec in records {
        match rec_id(&rec).map(str::to_string) {
            Some(id) => match pos.get(&id) {
                Some(&i) => out[i] = absorb(&out[i], &rec),
                None => {
                    pos.insert(id, out.len());
                    out.push(rec);
                }
            },
            None => out.push(rec),
        }
    }
    out
}

/// Whether the `replacedBy` chain starting at `id` hands the record over to
/// something else: it ends at a record that is not replaced (live, or a plain
/// delete). A chain that loops back to `id` (a self-reference, or `a -> b`
/// plus `b -> a` from two machines that folded in opposite directions) or
/// runs past [`MAX_REPLACED_HOPS`] hands over nothing; dropping on it would
/// delete every record in the loop.
fn replacement_settles(id: &str, tombs: &Map<String, Value>) -> bool {
    let replaced_by = |cur: &str| tombs.get(cur).and_then(|t| str_field(t, "replacedBy"));
    let Some(mut cur) = replaced_by(id) else {
        return false;
    };
    for _ in 0..MAX_REPLACED_HOPS {
        if cur == id {
            return false;
        }
        match replaced_by(cur) {
            Some(next) => cur = next,
            None => return true,
        }
    }
    false
}

fn is_tombstoned(rec: &Value, tombs: &Map<String, Value>) -> bool {
    let Some(id) = rec_id(rec) else {
        return false;
    };
    let Some(tomb) = tombs.get(id) else {
        return false;
    };
    if str_field(tomb, "replacedBy").is_some() {
        // Its `deletedAt` is the fold time, not a user delete, so a broken
        // handover keeps the record rather than falling back to the clocks.
        return replacement_settles(id, tombs);
    }
    match ts_field(tomb, "deletedAt") {
        Some(deleted) => Some(deleted) > record_latest_clock(rec),
        None => false,
    }
}

/// Fold records of one repo (same `gitRepo`) whose platform claims do not
/// conflict. Records in `keep_apart` take no part. See [`normalize`].
fn dedupe_by_git_repo(
    records: &mut Vec<Value>,
    tombs: &mut Map<String, Value>,
    keep_apart: &HashSet<String>,
) {
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, rec) in records.iter().enumerate() {
        match rec_id(rec) {
            None => continue,
            Some(id) if keep_apart.contains(id) => continue,
            Some(_) => {}
        }
        if let Some(repo) = str_field(rec, "gitRepo") {
            groups.entry(git_repo_key(repo)).or_default().push(i);
        }
    }
    let mut removed = vec![false; records.len()];
    for (_, mut idxs) in groups {
        if idxs.len() < 2 {
            continue;
        }
        // Survivor is the oldest registration (tie -> smaller id), so every
        // machine picks the same one regardless of array order.
        idxs.sort_by(|&a, &b| {
            ts_field(&records[a], "createdAt")
                .cmp(&ts_field(&records[b], "createdAt"))
                .then_with(|| rec_id(&records[a]).cmp(&rec_id(&records[b])))
        });
        for x in 0..idxs.len() {
            let i = idxs[x];
            if removed[i] {
                continue;
            }
            for &j in &idxs[x + 1..] {
                if removed[j] {
                    continue;
                }
                if !claims_compatible(&record_claims(&records[i]), &record_claims(&records[j])) {
                    continue;
                }
                let survivor_id = rec_id(&records[i]).unwrap_or_default().to_string();
                let loser_id = rec_id(&records[j]).unwrap_or_default().to_string();
                let deleted_at =
                    record_latest_clock(&records[i]).max(record_latest_clock(&records[j]));
                records[i] = absorb(&records[i], &records[j]);
                removed[j] = true;

                let mut tomb = Map::new();
                if let Some(d) = deleted_at {
                    tomb.insert("deletedAt".to_string(), Value::String(format_timestamp(d)));
                }
                tomb.insert("replacedBy".to_string(), Value::String(survivor_id));
                let tomb = Value::Object(tomb);
                let entry = match tombs.get(&loser_id) {
                    Some(existing) => merge_tombstone_entries(existing, &tomb),
                    None => tomb,
                };
                tombs.insert(loser_id, entry);
            }
        }
    }
    let mut k = 0;
    records.retain(|_| {
        let keep = !removed[k];
        k += 1;
        keep
    });
}

/// Bring a registry to its canonical form. Deterministic, so every machine
/// that sees the same records converges on the same result; idempotent.
///
/// 1. Records sharing an id are folded into the first.
/// 2. Tombstones apply: a record is dropped when its tombstone has
///    `replacedBy` (unless that chain loops back to it), or when `deletedAt`
///    is newer than every clock on the record (an edit after the delete
///    brings it back).
/// 3. Records with the same `gitRepo` (case-insensitive) are folded together
///    when their platform claims are compatible (claims = `paths` slots plus
///    the bare `path` under its shape platform; compatible = no platform
///    claimed by both with different directories). The oldest registration
///    survives with the union of claims; each loser becomes a tombstone
///    `{deletedAt, replacedBy: survivor}`. Records without `gitRepo` are
///    never folded.
///
/// Surviving records keep their positions.
pub fn normalize(value: &Value) -> Value {
    normalize_keeping_apart(value, &HashSet::new())
}

/// [`normalize`], except that the records in `keep_apart` are never folded
/// (step 3): not into another record, and no other record into them.
///
/// This is for projects whose sync is disabled on this machine. The server
/// never sees such a record, so a fold involving it would hand the other
/// machine a `replacedBy` pointing at an id it cannot resolve (and its own
/// record would be dropped by it), or push the private record's fields out
/// under the survivor's id.
pub fn normalize_keeping_apart(value: &Value, keep_apart: &HashSet<String>) -> Value {
    let Some(obj) = value.as_object() else {
        return value.clone();
    };
    let mut records = collapse_duplicate_ids(records_of(obj).to_vec());
    let mut tombs = obj
        .get(DELETED_KEY)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    records.retain(|rec| !is_tombstoned(rec, &tombs));
    dedupe_by_git_repo(&mut records, &mut tombs, keep_apart);

    let mut out = obj.clone();
    if obj.contains_key(PROJECTS_KEY) || !records.is_empty() {
        out.insert(PROJECTS_KEY.to_string(), Value::Array(records));
    }
    if obj.contains_key(DELETED_KEY) || !tombs.is_empty() {
        out.insert(DELETED_KEY.to_string(), Value::Object(tombs));
    }
    Value::Object(out)
}

// ─────────────────────────────────────────────────────────────────────────────
// Id resolution and comparison
// ─────────────────────────────────────────────────────────────────────────────

/// Follow `replacedBy` from `id` until a live id (per `is_live`) is reached.
/// `None` when the chain dead-ends at a plain delete, is broken, or loops.
pub fn resolve_id_with(
    is_live: impl Fn(&str) -> bool,
    deleted: Option<&Map<String, Value>>,
    id: &str,
) -> Option<String> {
    let mut cur = id.to_string();
    for _ in 0..=MAX_REPLACED_HOPS {
        if is_live(&cur) {
            return Some(cur);
        }
        let next = deleted?.get(&cur).and_then(|t| str_field(t, "replacedBy"))?;
        cur = next.to_string();
    }
    None
}

/// The live record id `id` refers to today: `id` itself if the record exists,
/// else wherever its `replacedBy` chain leads. Tabs and sessions hold ids from
/// before a merge folded two registrations together; this maps them forward.
pub fn resolve_id(value: &Value, id: &str) -> Option<String> {
    let live: HashSet<&str> = value
        .as_object()
        .map(|o| records_of(o).iter().filter_map(rec_id).collect())
        .unwrap_or_default();
    let deleted = value.get(DELETED_KEY).and_then(Value::as_object);
    resolve_id_with(|i| live.contains(i), deleted, id)
}

type RecordIndex<'a> = (BTreeMap<&'a str, &'a Value>, Vec<String>);

fn record_index(obj: &Map<String, Value>) -> RecordIndex<'_> {
    let mut by_id = BTreeMap::new();
    let mut anonymous = Vec::new();
    for rec in records_of(obj) {
        match rec_id(rec) {
            Some(id) => {
                by_id.insert(id, rec);
            }
            None => anonymous.push(canonical(rec)),
        }
    }
    anonymous.sort();
    (by_id, anonymous)
}

/// Whether two registries hold the same content, for "did this pull change
/// anything". Record order is ignored (each machine keeps its own order, and
/// comparing it would make two machines push their orderings at each other
/// forever), and so is `machineId`, which names the last writer rather than
/// anything in the registry.
pub fn projects_semantically_equal(a: &Value, b: &Value) -> bool {
    let (Some(ao), Some(bo)) = (a.as_object(), b.as_object()) else {
        return a == b;
    };
    if record_index(ao) != record_index(bo) {
        return false;
    }
    let empty = Value::Object(Map::new());
    let ta = ao.get(DELETED_KEY).unwrap_or(&empty);
    let tb = bo.get(DELETED_KEY).unwrap_or(&empty);
    if ta != tb {
        return false;
    }
    let ignored = [PROJECTS_KEY, DELETED_KEY, "machineId"];
    let keys: BTreeSet<&String> = ao.keys().chain(bo.keys()).collect();
    keys.into_iter()
        .filter(|k| !ignored.contains(&k.as_str()))
        .all(|k| ao.get(k) == bo.get(k))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    // ---- platforms and paths ----

    #[test]
    fn platform_of_path_by_shape() {
        assert_eq!(platform_of_path("C:\\code\\mnemos"), Some("win32"));
        assert_eq!(platform_of_path("c:/code/mnemos"), Some("win32"));
        assert_eq!(platform_of_path("\\\\server\\share\\x"), Some("win32"));
        assert_eq!(platform_of_path("/Users/alice/code/tcbv2"), Some("darwin"));
        assert_eq!(platform_of_path("/home/alice/code"), Some("linux"));
        assert_eq!(platform_of_path("relative/dir"), None);
        assert_eq!(platform_of_path(""), None);
        assert_eq!(platform_of_path("C:"), None);
    }

    #[test]
    fn current_platform_is_one_of_the_slots() {
        assert!(PLATFORMS.contains(&current_platform()));
    }

    #[test]
    fn same_path_rules_per_platform() {
        assert!(same_path("C:\\code\\Admin\\", "c:/code/admin", "win32"));
        assert!(same_path("/Users/alice/Code/", "/Users/alice/code", "darwin"));
        assert!(!same_path("/home/alice/Code", "/home/alice/code", "linux"));
        assert!(same_path("/home/alice/code/", "/home/alice/code", "linux"));
        // Verbatim (`canonicalize`) forms name the same Windows directories.
        assert!(same_path(r"\\?\C:\code\mnemos", r"c:\code\mnemos\", "win32"));
        assert!(same_path(r"\\?\UNC\srv\share\x", r"\\srv\share\x", "win32"));
        assert!(is_fs_root(r"\\?\C:\", "win32"));
        assert_eq!(strip_verbatim(r"\\?\C:\code"), r"C:\code");
        assert_eq!(strip_verbatim(r"\\?\unc\srv\share"), r"\\srv\share");
        assert_eq!(strip_verbatim(r"\\?\ø"), "ø");
        assert_eq!(strip_verbatim("/Users/alice"), "/Users/alice");
    }

    #[test]
    fn path_parent_and_name() {
        assert_eq!(path_parent("/Users/alice/code", "darwin").as_deref(), Some("/Users/alice"));
        assert_eq!(path_parent("/Users", "darwin").as_deref(), Some("/"));
        assert_eq!(path_parent("/", "darwin"), None);
        assert_eq!(path_parent("C:\\code\\x\\", "win32").as_deref(), Some("C:\\code"));
        assert_eq!(path_parent("C:\\code", "win32").as_deref(), Some("C:\\"));
        assert_eq!(path_parent("C:\\", "win32"), None);
        assert_eq!(path_file_name("C:\\code\\notes app", "win32").as_deref(), Some("notes app"));
        assert_eq!(path_file_name("/Users/alice/skole/", "darwin").as_deref(), Some("skole"));
        assert_eq!(path_file_name("C:\\", "win32"), None);
        assert_eq!(path_join("/Users/alice", "code", "darwin"), "/Users/alice/code");
        assert_eq!(path_join("C:\\Users\\bob", "code", "win32"), "C:\\Users\\bob\\code");
        assert_eq!(path_join("C:\\", "code", "win32"), "C:\\code");
        assert!(is_fs_root("C:\\", "win32"));
        assert!(is_fs_root("c:", "win32"));
        assert!(is_fs_root("/", "darwin"));
        assert!(!is_fs_root("/Users", "darwin"));
    }

    // ---- git + slugs ----

    #[test]
    fn normalize_git_remote_matches_admin_git_repo_shape() {
        assert_eq!(
            normalize_git_remote("git@github.com:acme/Admin.git").as_deref(),
            Some("acme/Admin")
        );
        assert_eq!(
            normalize_git_remote("https://github.com/bob/TCB").as_deref(),
            Some("bob/TCB")
        );
        assert_eq!(
            normalize_git_remote("https://dev.azure.com/gx/Globex/_git/globex\n").as_deref(),
            Some("_git/globex")
        );
        assert_eq!(normalize_git_remote("nonsense"), None);
    }

    #[test]
    fn git_repo_key_is_case_insensitive() {
        assert_eq!(git_repo_key(" acme/TCB "), git_repo_key("acme/tcb"));
    }

    #[test]
    fn claude_project_slug_matches_claude_code() {
        let cases = [
            ("/Users/alice/code/tcbv2", "-Users-alice-code-tcbv2"),
            (
                "/Users/alice/code/Admin/.claude/apps/TwinCurrent-Build",
                "-Users-alice-code-Admin--claude-apps-TwinCurrent-Build",
            ),
            (r"C:\code\Ledger_App", "C--code-Ledger-App"),
            (r"C:\code\notes app", "C--code-notes-app"),
            (r"C:\Users\bob\.claude", "C--Users-bob--claude"),
        ];
        for (input, want) in cases {
            assert_eq!(
                claude_project_slug(&PathBuf::from(input)).as_deref(),
                Some(want),
                "slug of {input}"
            );
        }
    }

    #[test]
    fn claude_project_slug_counts_utf16_units_and_caps_length() {
        // One BMP char -> one dash; one astral char -> two dashes.
        assert_eq!(claude_project_slug(Path::new("/a/ø")).as_deref(), Some("-a--"));
        assert_eq!(claude_project_slug(Path::new("/a/😀")).as_deref(), Some("-a---"));
        let long = format!("/{}", "a".repeat(199));
        assert_eq!(claude_project_slug(Path::new(&long)).map(|s| s.len()), Some(200));
        let too_long = format!("/{}", "a".repeat(200));
        assert_eq!(claude_project_slug(Path::new(&too_long)), None);
    }

    // ---- record helpers ----

    #[test]
    fn record_local_path_prefers_slot_then_shape() {
        let rec = json!({ "path": "/Users/alice/code/Admin", "paths": { "win32": "C:\\code\\Admin" } });
        assert_eq!(record_local_path(&rec, "win32").as_deref(), Some("C:\\code\\Admin"));
        assert_eq!(record_local_path(&rec, "darwin").as_deref(), Some("/Users/alice/code/Admin"));
        assert_eq!(record_local_path(&rec, "linux"), None);
        let win_only = json!({ "path": "C:\\code\\skole" });
        assert_eq!(record_local_path(&win_only, "darwin"), None);
    }

    fn rec(id: &str, extra: Value) -> Value {
        let mut base = json!({
            "id": id,
            "name": id,
            "path": format!("/Users/alice/code/{id}"),
            "createdAt": "2026-01-01T00:00:00Z",
            "lastOpenedAt": null
        });
        for (k, v) in extra.as_object().unwrap() {
            base[k] = v.clone();
        }
        base
    }

    fn reg(records: Vec<Value>) -> Value {
        json!({ "projects": records, "machineId": "m" })
    }

    fn ids(v: &Value) -> Vec<String> {
        v["projects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect()
    }

    // ---- merge ----

    #[test]
    fn merge_never_drops_one_sided_records() {
        let local = reg(vec![rec("a", json!({})), rec("b", json!({}))]);
        let remote = reg(vec![rec("c", json!({})), rec("a", json!({}))]);
        let merged = merge(&local, &remote, "darwin");
        assert_eq!(ids(&merged), vec!["a", "b", "c"]);
        let other = merge(&remote, &local, "darwin");
        assert!(projects_semantically_equal(&merged, &other));
    }

    #[test]
    fn merge_picks_later_updated_base_and_fills_missing_fields() {
        let local = reg(vec![rec(
            "a",
            json!({ "name": "old", "updatedAt": "2026-02-01T00:00:00Z", "color": "red" }),
        )]);
        let remote = reg(vec![rec(
            "a",
            json!({ "name": "new", "updatedAt": "2026-03-01T00:00:00Z", "settings": { "x": 1 } }),
        )]);
        let merged = merge(&local, &remote, "darwin");
        let r = &merged["projects"][0];
        assert_eq!(r["name"], "new");
        assert_eq!(r["color"], "red");
        assert_eq!(r["settings"]["x"], 1);
    }

    #[test]
    fn merge_open_follows_open_clock_not_base() {
        let local = reg(vec![rec(
            "a",
            json!({ "updatedAt": "2026-05-01T00:00:00Z", "open": true, "openChangedAt": "2026-04-01T00:00:00Z" }),
        )]);
        let remote = reg(vec![rec(
            "a",
            json!({ "updatedAt": "2026-03-01T00:00:00Z", "open": false, "openChangedAt": "2026-04-02T00:00:00Z" }),
        )]);
        let merged = merge(&local, &remote, "darwin");
        assert_eq!(merged["projects"][0]["open"], false);
        assert_eq!(merged["projects"][0]["updatedAt"], "2026-05-01T00:00:00Z");
        // A missing clock loses even against an older base.
        let unclocked = reg(vec![rec("a", json!({ "updatedAt": "2026-09-01T00:00:00Z", "open": false }))]);
        let merged = merge(&unclocked, &local, "darwin");
        assert_eq!(merged["projects"][0]["open"], true);
    }

    #[test]
    fn merge_paths_per_slot() {
        let local = reg(vec![rec(
            "a",
            json!({ "updatedAt": "2026-05-01T00:00:00Z", "paths": { "win32": "C:\\new", "darwin": "/Users/stale" } }),
        )]);
        let remote = reg(vec![rec(
            "a",
            json!({ "updatedAt": "2026-01-02T00:00:00Z", "paths": { "win32": "C:\\stale", "darwin": "/Users/alice/code/a", "linux": "/home/a" } }),
        )]);
        let merged = merge(&local, &remote, "win32");
        let paths = &merged["projects"][0]["paths"];
        assert_eq!(paths["win32"], "C:\\new");
        assert_eq!(paths["darwin"], "/Users/alice/code/a");
        assert_eq!(paths["linux"], "/home/a");
    }

    #[test]
    fn merge_git_repo_falls_back_to_other_side() {
        let local = reg(vec![rec("a", json!({ "updatedAt": "2026-05-01T00:00:00Z", "gitRepo": "" }))]);
        let remote = reg(vec![rec("a", json!({ "gitRepo": "acme/a" }))]);
        let merged = merge(&local, &remote, "darwin");
        assert_eq!(merged["projects"][0]["gitRepo"], "acme/a");
    }

    #[test]
    fn merge_tie_break_is_symmetric() {
        let a = reg(vec![rec("a", json!({ "updatedAt": "2026-05-01T00:00:00Z", "name": "one" }))]);
        let b = reg(vec![rec("a", json!({ "updatedAt": "2026-05-01T00:00:00Z", "name": "two" }))]);
        let ab = merge(&a, &b, "darwin");
        let ba = merge(&b, &a, "darwin");
        assert!(projects_semantically_equal(&ab, &ba));
    }

    #[test]
    fn tombstone_drops_older_record_but_not_newer_edit() {
        let local = reg(vec![rec("a", json!({ "updatedAt": "2026-02-01T00:00:00Z" }))]);
        let mut remote = reg(vec![]);
        remote["deletedProjects"] = json!({ "a": { "deletedAt": "2026-03-01T00:00:00Z" } });
        let merged = merge(&local, &remote, "darwin");
        assert!(ids(&merged).is_empty());
        assert_eq!(merged["deletedProjects"]["a"]["deletedAt"], "2026-03-01T00:00:00Z");
        // Merging the stale copy back in again does not resurrect it.
        let again = merge(&merged, &local, "darwin");
        assert!(ids(&again).is_empty());
        // An edit after the delete wins.
        let edited = reg(vec![rec("a", json!({ "updatedAt": "2026-04-01T00:00:00Z" }))]);
        assert_eq!(ids(&merge(&edited, &remote, "darwin")), vec!["a"]);
    }

    #[test]
    fn replaced_tombstone_always_drops() {
        let local = reg(vec![rec("a", json!({ "updatedAt": "2027-01-01T00:00:00Z" })), rec("b", json!({}))]);
        let mut remote = reg(vec![]);
        remote["deletedProjects"] =
            json!({ "a": { "deletedAt": "2026-01-01T00:00:00Z", "replacedBy": "b" } });
        let merged = merge(&local, &remote, "darwin");
        assert_eq!(ids(&merged), vec!["b"]);
        assert_eq!(resolve_id(&merged, "a").as_deref(), Some("b"));
    }

    #[test]
    fn tombstone_entries_merge_keeps_later_and_replaced_by() {
        let mut a = reg(vec![]);
        a["deletedProjects"] = json!({ "x": { "deletedAt": "2026-01-01T00:00:00Z", "replacedBy": "y" } });
        let mut b = reg(vec![]);
        b["deletedProjects"] = json!({ "x": { "deletedAt": "2026-02-01T00:00:00Z" } });
        let merged = merge(&a, &b, "darwin");
        assert_eq!(merged["deletedProjects"]["x"]["deletedAt"], "2026-02-01T00:00:00Z");
        assert_eq!(merged["deletedProjects"]["x"]["replacedBy"], "y");
    }

    #[test]
    fn merge_preserves_unknown_top_level_keys() {
        let local = json!({ "projects": [], "machineId": "local", "settings": { "a": 1 } });
        let remote = json!({ "projects": [], "machineId": "remote", "schemaVersion": 2 });
        let merged = merge(&local, &remote, "darwin");
        assert_eq!(merged["machineId"], "local");
        assert_eq!(merged["settings"]["a"], 1);
        assert_eq!(merged["schemaVersion"], 2);
    }

    // ---- normalize ----

    #[test]
    fn normalize_folds_compatible_registrations_of_one_repo() {
        // Mac `tcbv2` and Windows `mnemos`: same repo, one darwin claim, one
        // win32 claim -> one record under the older id.
        let tcbv2 = json!({
            "id": "3092ffc6", "name": "tcbv2", "path": "/Users/alice/code/tcbv2",
            "createdAt": "2026-04-30T09:39:17.164Z", "updatedAt": "2026-04-30T09:39:21.074Z",
            "gitRepo": "acme/TCB", "lastOpenedAt": null
        });
        let mnemos = json!({
            "id": "project-1790233543395", "name": "mnemos", "path": "C:\\code\\mnemos",
            "createdAt": "2026-09-24T07:05:43.395471300Z", "gitRepo": "acme/tcb",
            "open": true, "openChangedAt": "2026-09-24T08:00:00Z", "updatedAt": "2026-09-24T08:00:00Z",
            "lastOpenedAt": null
        });
        let v = normalize(&reg(vec![mnemos.clone(), tcbv2.clone()]));
        assert_eq!(ids(&v), vec!["3092ffc6"]);
        let r = &v["projects"][0];
        assert_eq!(r["name"], "tcbv2");
        assert_eq!(r["paths"]["darwin"], "/Users/alice/code/tcbv2");
        assert_eq!(r["paths"]["win32"], "C:\\code\\mnemos");
        assert_eq!(r["open"], true);
        assert_eq!(r["gitRepo"], "acme/TCB");
        let t = &v["deletedProjects"]["project-1790233543395"];
        assert_eq!(t["replacedBy"], "3092ffc6");
        assert_eq!(t["deletedAt"], "2026-09-24T08:00:00Z");
        assert_eq!(resolve_id(&v, "project-1790233543395").as_deref(), Some("3092ffc6"));
        // Idempotent, and independent of input order.
        assert_eq!(normalize(&v), v);
        let swapped = normalize(&reg(vec![tcbv2, mnemos]));
        assert!(projects_semantically_equal(&v, &swapped));
    }

    #[test]
    fn normalize_keeps_conflicting_claims_apart() {
        // TwinCurrent-Build and TCB are both bob/TCB, with two
        // different darwin checkouts: two projects.
        let a = json!({
            "id": "ed5f6c66", "name": "TwinCurrent-Build",
            "path": "/Users/alice/code/Admin/.claude/apps/TwinCurrent-Build",
            "createdAt": "2026-01-21T22:51:37.631Z", "gitRepo": "bob/TCB"
        });
        let b = json!({
            "id": "421a6cc0", "name": "TCB", "path": "/Users/alice/code/TCB",
            "createdAt": "2026-03-17T23:59:30.315Z", "gitRepo": "bob/TCB"
        });
        let v = normalize(&reg(vec![a, b]));
        assert_eq!(ids(&v), vec!["ed5f6c66", "421a6cc0"]);
        assert!(v.get("deletedProjects").is_none());
    }

    #[test]
    fn kept_apart_records_are_never_folded() {
        // Windows `mnemos` has sync disabled there; the Mac's `tcbv2` is the
        // same repo. Folding either way would push a handover to an id the
        // server never sees (or the private record's paths).
        let win = json!({
            "id": "win-d", "name": "mnemos", "path": "C:\\code\\mnemos",
            "createdAt": "2026-01-01T00:00:00Z", "gitRepo": "acme/TCB"
        });
        let mac = json!({
            "id": "mac-r", "name": "tcbv2", "path": "/Users/alice/code/tcbv2",
            "createdAt": "2026-02-01T00:00:00Z", "gitRepo": "acme/TCB"
        });
        let apart: HashSet<String> = ["win-d".to_string()].into_iter().collect();
        for records in [vec![win.clone(), mac.clone()], vec![mac.clone(), win.clone()]] {
            let v = normalize_keeping_apart(&reg(records), &apart);
            let mut got = ids(&v);
            got.sort();
            assert_eq!(got, vec!["mac-r", "win-d"]);
            assert!(v.get("deletedProjects").is_none(), "{v}");
        }
        let merged = merge_keeping_apart(&reg(vec![win]), &reg(vec![mac]), "win32", &apart);
        assert_eq!(ids(&merged), vec!["win-d", "mac-r"]);
        assert!(merged.get("deletedProjects").is_none());
        // Without the exclusion they fold (the default rule is unchanged).
        assert_eq!(ids(&normalize(&merged)), vec!["win-d"]);
    }

    #[test]
    fn normalize_never_folds_records_without_git_repo() {
        let a = json!({ "id": "a", "name": "skole", "path": "C:\\code\\skole", "createdAt": "2026-01-01T00:00:00Z" });
        let b = json!({ "id": "b", "name": "skole", "path": "/Users/alice/skole", "createdAt": "2026-01-02T00:00:00Z" });
        assert_eq!(ids(&normalize(&reg(vec![a, b]))), vec!["a", "b"]);
    }

    #[test]
    fn normalize_collapses_duplicate_ids() {
        let a1 = rec("a", json!({ "paths": { "darwin": "/Users/alice/code/a" } }));
        let a2 = rec("a", json!({ "paths": { "win32": "C:\\code\\a" }, "gitRepo": "o/a" }));
        let v = normalize(&reg(vec![a1, a2]));
        assert_eq!(ids(&v), vec!["a"]);
        assert_eq!(v["projects"][0]["paths"]["win32"], "C:\\code\\a");
        assert_eq!(v["projects"][0]["gitRepo"], "o/a");
    }

    #[test]
    fn replaced_by_loop_never_drops_the_records_in_it() {
        // Two machines folded in opposite directions (or a hand edit): the
        // handover goes nowhere, so neither record may be dropped for it.
        let mut v = reg(vec![
            rec("x", json!({ "path": "/Users/alice/code/x" })),
            rec("y", json!({ "path": "/Users/alice/code/y" })),
        ]);
        v["deletedProjects"] = json!({
            "x": { "deletedAt": "2026-06-01T00:00:00Z", "replacedBy": "y" },
            "y": { "deletedAt": "2026-06-01T00:00:00Z", "replacedBy": "x" }
        });
        assert_eq!(ids(&normalize(&v)), vec!["x", "y"]);
        assert_eq!(resolve_id(&normalize(&v), "x").as_deref(), Some("x"));

        let mut selfref = reg(vec![rec("x", json!({}))]);
        selfref["deletedProjects"] =
            json!({ "x": { "deletedAt": "2026-06-01T00:00:00Z", "replacedBy": "x" } });
        assert_eq!(ids(&normalize(&selfref)), vec!["x"]);

        // A chain that ends at a live record or a plain delete still drops.
        let mut chain = reg(vec![rec("a", json!({})), rec("c", json!({}))]);
        chain["deletedProjects"] = json!({
            "a": { "deletedAt": "2026-01-01T00:00:00Z", "replacedBy": "b" },
            "b": { "deletedAt": "2026-01-01T00:00:00Z", "replacedBy": "c" },
            "c": { "deletedAt": "2025-01-01T00:00:00Z" }
        });
        assert_eq!(ids(&normalize(&chain)), vec!["c"]);
    }

    // ---- ids + equality ----

    #[test]
    fn resolve_id_follows_chains_and_survives_cycles() {
        let mut v = reg(vec![rec("c", json!({}))]);
        v["deletedProjects"] = json!({
            "a": { "deletedAt": "2026-01-01T00:00:00Z", "replacedBy": "b" },
            "b": { "deletedAt": "2026-01-01T00:00:00Z", "replacedBy": "c" },
            "x": { "deletedAt": "2026-01-01T00:00:00Z", "replacedBy": "y" },
            "y": { "deletedAt": "2026-01-01T00:00:00Z", "replacedBy": "x" },
            "gone": { "deletedAt": "2026-01-01T00:00:00Z" }
        });
        assert_eq!(resolve_id(&v, "a").as_deref(), Some("c"));
        assert_eq!(resolve_id(&v, "c").as_deref(), Some("c"));
        assert_eq!(resolve_id(&v, "x"), None);
        assert_eq!(resolve_id(&v, "gone"), None);
        assert_eq!(resolve_id(&v, "unknown"), None);
    }

    #[test]
    fn semantic_equality_ignores_order_and_machine_id() {
        let a = json!({ "projects": [rec("a", json!({})), rec("b", json!({}))], "machineId": "one" });
        let b = json!({ "projects": [rec("b", json!({})), rec("a", json!({}))], "machineId": "two" });
        assert!(projects_semantically_equal(&a, &b));
        let c = json!({ "projects": [rec("b", json!({})), rec("a", json!({ "name": "x" }))] });
        assert!(!projects_semantically_equal(&a, &c));
        let mut d = a.clone();
        d["deletedProjects"] = json!({});
        assert!(projects_semantically_equal(&a, &d));
        d["deletedProjects"] = json!({ "z": { "deletedAt": "2026-01-01T00:00:00Z" } });
        assert!(!projects_semantically_equal(&a, &d));
    }
}

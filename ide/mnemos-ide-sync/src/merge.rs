// SPDX-License-Identifier: AGPL-3.0-only
//! Per-key merge strategies for `pushWithMerge`.
//!
//! Mirrors v1's `apps/frontend/src/main/sync-manager.ts::mergeForKey`:
//! - `claude-memory:*` → file-level mtime-win + timed tombstones (`MemoryPayload`)
//! - `claude-mcp-servers`, `claude-agents`, `claude-commands`, `claude-hooks`,
//!   `claude-skills`, `claude-rules`, `claude-output-styles` → directory-shape
//!   union, local-wins on conflict, deletes as `__deleted` tombstones
//! - `projects` → `mnemos_ide_core::registry::merge`: record-level union with per-field
//!   clocks, so one machine's push can never erase the other machine's projects
//! - `claude-session-index` → union of sessions and machines, the higher
//!   segment number winning per (session, machine)
//! - everything else (settings, claude-md, …) → local-wins on the blob

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use mnemos_ide_core::registry;

/// Sync key of the shared Claude session index: the newest transcript
/// segment each machine has uploaded for each session,
/// `{"<sid>": {"<machine>": {"seq": <max seq uploaded>, "at": "<RFC3339>"}}}`.
/// A machine polls this one small key to learn which sessions changed
/// elsewhere, and lists only those sessions in the blob store. It is state,
/// not a file: never written to disk, never mirrored to a team.
pub const SESSION_INDEX_KEY: &str = "claude-session-index";

/// Index entries this much older than the newest entry are dropped by the
/// merge. The index is fetched whole every 30 seconds (no conditional GET,
/// no compression), and every session, workflow subagents included, adds an
/// entry, so the window is kept short. It is only a delivery hint: a machine
/// that was away longer catches up through its startup and hourly full
/// listings, which never read the index.
const SESSION_INDEX_MAX_AGE_DAYS: i64 = 3;

/// Per-file entry in a v2 memory payload. `mtime` is ms-since-epoch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryFileEntry {
    pub content: String,
    pub mtime: i64,
}

/// v2 shape of `claude-memory:{profile}:{repo}`.
///
/// Deletions are tombstones named `memory/<rel>` or `MEMORY.md`:
/// `__deleted_at` holds when each was deleted (ms), and `__deleted_files`
/// lists the same names for older builds, which read only that list. A name
/// only in `__deleted_files` (written by an older writer, with no time) is
/// passed through but never deletes anything here.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryPayload {
    #[serde(default)]
    pub files: BTreeMap<String, MemoryFileEntry>,
    #[serde(default, rename = "memoryMd")]
    pub memory_md: Option<MemoryFileEntry>,
    #[serde(default, rename = "__deleted_files", skip_serializing_if = "Vec::is_empty")]
    pub deleted_files: Vec<String>,
    #[serde(default, rename = "__deleted_at", skip_serializing_if = "BTreeMap::is_empty")]
    pub deleted_at: BTreeMap<String, i64>,
    /// Local only, never sent: names this machine saw exist again after a
    /// delete (moved back from a backup, say, keeping an old mtime). They
    /// clear the tombstone whatever the file's time.
    #[serde(skip)]
    pub revived: BTreeSet<String>,
}

/// Name of a memory file in the payload's tombstones (`memory/<rel>`).
pub fn memory_tombstone_name(rel: &str) -> String {
    format!("memory/{rel}")
}

/// Tombstone name of `MEMORY.md`.
pub const MEMORY_MD_NAME: &str = "MEMORY.md";

/// Local-only entry of a memory value handed to [`merge_memory`]: see
/// [`MemoryPayload::revived`]. The merge never writes it.
pub const MEMORY_REVIVED_KEY: &str = "__revived";

fn normalize_memory_file(v: &Value) -> Option<MemoryFileEntry> {
    if v.is_null() {
        return None;
    }
    if let Some(s) = v.as_str() {
        return Some(MemoryFileEntry {
            content: s.to_string(),
            mtime: 0,
        });
    }
    if let Some(obj) = v.as_object() {
        let content = obj.get("content")?.as_str()?.to_string();
        let mtime = obj
            .get("mtime")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        return Some(MemoryFileEntry { content, mtime });
    }
    None
}

fn parse_memory_payload(v: &Value) -> MemoryPayload {
    let mut p = MemoryPayload::default();
    let Some(obj) = v.as_object() else {
        return p;
    };
    if let Some(files) = obj.get("files").and_then(Value::as_object) {
        for (rel, entry) in files {
            if let Some(e) = normalize_memory_file(entry) {
                p.files.insert(rel.clone(), e);
            }
        }
    }
    if let Some(md) = obj.get("memoryMd") {
        p.memory_md = normalize_memory_file(md);
    }
    if let Some(arr) = obj.get("__deleted_files").and_then(Value::as_array) {
        for s in arr {
            if let Some(s) = s.as_str() {
                p.deleted_files.push(s.to_string());
            }
        }
    }
    if let Some(times) = obj.get("__deleted_at").and_then(Value::as_object) {
        for (name, at) in times {
            if let Some(at) = at.as_i64() {
                p.deleted_at.insert(name.clone(), at);
            }
        }
    }
    if let Some(arr) = obj.get(MEMORY_REVIVED_KEY).and_then(Value::as_array) {
        p.revived
            .extend(arr.iter().filter_map(Value::as_str).map(str::to_string));
    }
    p
}

/// The newer of two entries (`local` on a tie).
fn newer(l: &MemoryFileEntry, r: &MemoryFileEntry) -> MemoryFileEntry {
    if l.mtime >= r.mtime {
        l.clone()
    } else {
        r.clone()
    }
}

/// Merge two memory payloads: per file the newer entry wins (a bare string
/// counts as mtime 0), and deletes are timed tombstones.
/// - A tombstone keeps the latest time either side has for the name.
/// - It removes the file when it is at least as new as the file's winning
///   entry, and is dropped itself once a newer file exists (an edit made
///   after the delete wins).
/// - A name the local side revived ([`MemoryPayload::revived`]) loses its
///   tombstone whatever the file's time.
/// - A name only in an older writer's `__deleted_files` never removes
///   anything; it is passed through for older builds.
/// - `MEMORY.md` is merged the same way under the name `MEMORY.md`.
///
/// Symmetric in its sides apart from ties between two entries with the same
/// mtime (local wins) and the local-only revived names.
pub fn merge_memory(local: &Value, remote: &Value) -> Value {
    let local = parse_memory_payload(local);
    let remote = parse_memory_payload(remote);
    let revived: BTreeSet<&str> = local
        .revived
        .iter()
        .chain(&remote.revived)
        .map(String::as_str)
        .collect();

    // Latest delete time per name.
    let mut deleted_at: BTreeMap<String, i64> = local.deleted_at.clone();
    for (name, at) in &remote.deleted_at {
        let slot = deleted_at.entry(name.clone()).or_insert(*at);
        *slot = (*slot).max(*at);
    }
    deleted_at.retain(|name, _| !revived.contains(name.as_str()));
    // Keeps `entry` unless a tombstone at least as new covers `name`; a
    // tombstone the entry outlived is dropped.
    let mut settle = |name: String, entry: MemoryFileEntry| -> Option<MemoryFileEntry> {
        match deleted_at.get(&name) {
            Some(at) if *at >= entry.mtime => None,
            Some(_) => {
                deleted_at.remove(&name);
                Some(entry)
            }
            None => Some(entry),
        }
    };

    let all_rels: BTreeSet<&String> = local.files.keys().chain(remote.files.keys()).collect();
    let mut merged_files: BTreeMap<String, MemoryFileEntry> = BTreeMap::new();
    for rel in all_rels {
        let chosen = match (local.files.get(rel), remote.files.get(rel)) {
            (Some(l), Some(r)) => newer(l, r),
            (Some(e), None) | (None, Some(e)) => e.clone(),
            (None, None) => continue,
        };
        if let Some(entry) = settle(memory_tombstone_name(rel), chosen) {
            merged_files.insert(rel.clone(), entry);
        }
    }

    let memory_md = match (&local.memory_md, &remote.memory_md) {
        (Some(l), Some(r)) => Some(newer(l, r)),
        (Some(e), None) | (None, Some(e)) => Some(e.clone()),
        (None, None) => None,
    }
    .and_then(|entry| settle(MEMORY_MD_NAME.to_string(), entry));

    // Older builds read only this list (and delete every name in it on each
    // apply), so it carries the live tombstones plus what an older writer
    // listed, minus anything revived.
    let timed: BTreeSet<&String> = local.deleted_at.keys().chain(remote.deleted_at.keys()).collect();
    let mut union_deleted: BTreeSet<String> = deleted_at.keys().cloned().collect();
    union_deleted.extend(
        local
            .deleted_files
            .iter()
            .chain(&remote.deleted_files)
            .filter(|name| !timed.contains(name) && !revived.contains(name.as_str()))
            .cloned(),
    );

    let mut out = serde_json::Map::new();
    let files_obj: serde_json::Map<String, Value> = merged_files
        .into_iter()
        .map(|(k, v)| (k, serde_json::to_value(v).unwrap()))
        .collect();
    out.insert("files".to_string(), Value::Object(files_obj));
    out.insert(
        "memoryMd".to_string(),
        match memory_md {
            Some(e) => serde_json::to_value(e).unwrap(),
            None => Value::Null,
        },
    );
    if !union_deleted.is_empty() {
        out.insert(
            "__deleted_files".to_string(),
            Value::Array(union_deleted.into_iter().map(Value::String).collect()),
        );
    }
    if !deleted_at.is_empty() {
        out.insert(
            "__deleted_at".to_string(),
            Value::Object(
                deleted_at
                    .into_iter()
                    .map(|(name, at)| (name, Value::from(at)))
                    .collect(),
            ),
        );
    }
    Value::Object(out)
}

/// Reserved entry of a directory payload: `{"<relative/path>": <deleted at,
/// ms>}`. Older builds skip it (it is not a string) and keep it through
/// their union.
pub const DIR_TOMBSTONES_KEY: &str = "__deleted";

/// The tombstones of a flat directory payload.
fn dir_tombstones(map: &serde_json::Map<String, Value>) -> BTreeMap<String, i64> {
    map.get(DIR_TOMBSTONES_KEY)
        .and_then(Value::as_object)
        .map(|t| {
            t.iter()
                .filter_map(|(rel, at)| Some((rel.clone(), at.as_i64()?)))
                .collect()
        })
        .unwrap_or_default()
}

/// Merge one flat directory payload (`relative/path -> base64`, plus
/// `__deleted`): a union, local-wins on conflict, with both sides'
/// tombstones (latest time per path). A tombstoned file is dropped, except
/// that a file the local side has outlives the other side's tombstone for
/// it and clears it: the local side is this machine's disk, which the
/// caller has already settled against the server's tombstones (a local copy
/// no newer than the delete is removed first), so what is left there was
/// re-created or edited after the delete. A side that carries a file and
/// its own tombstone for it (an older build's union) counts as deleted.
fn merge_flat_dir(
    local: &serde_json::Map<String, Value>,
    remote: &serde_json::Map<String, Value>,
) -> serde_json::Map<String, Value> {
    let local_tombstones = dir_tombstones(local);
    let mut tombstones = dir_tombstones(remote);
    for (rel, at) in &local_tombstones {
        let slot = tombstones.entry(rel.clone()).or_insert(*at);
        *slot = (*slot).max(*at);
    }
    let mut merged = remote.clone();
    merged.remove(DIR_TOMBSTONES_KEY);
    for (rel, v) in local {
        if rel == DIR_TOMBSTONES_KEY {
            continue;
        }
        if !local_tombstones.contains_key(rel) {
            tombstones.remove(rel);
        }
        merged.insert(rel.clone(), v.clone());
    }
    merged.retain(|rel, _| !tombstones.contains_key(rel));
    if !tombstones.is_empty() {
        merged.insert(
            DIR_TOMBSTONES_KEY.to_string(),
            Value::Object(
                tombstones
                    .into_iter()
                    .map(|(rel, at)| (rel, Value::from(at)))
                    .collect(),
            ),
        );
    }
    merged
}

/// Union two `Record<String, base64>` dir payloads, local-wins on conflict,
/// with deletes carried as tombstones (see [`merge_flat_dir`]). Honors
/// `{darwin: {...}, win32: {...}}` platform envelopes by merging only the
/// per-platform sub-objects, each the same way.
pub fn merge_dir_payload(local: &Value, remote: &Value) -> Value {
    let (Some(local_obj), Some(remote_obj)) = (local.as_object(), remote.as_object()) else {
        if !local.is_null() {
            return local.clone();
        }
        return remote.clone();
    };

    let is_platform_envelope = !local_obj.is_empty()
        && local_obj
            .keys()
            .all(|k| matches!(k.as_str(), "darwin" | "win32" | "linux"));

    if is_platform_envelope {
        let mut merged = remote_obj.clone();
        for (platform, l_val) in local_obj {
            let r_val = remote_obj
                .get(platform)
                .cloned()
                .unwrap_or(Value::Object(serde_json::Map::new()));
            let combined = match (r_val.as_object(), l_val.as_object()) {
                (Some(r), Some(l)) => Value::Object(merge_flat_dir(l, r)),
                _ => l_val.clone(),
            };
            merged.insert(platform.clone(), combined);
        }
        return Value::Object(merged);
    }

    Value::Object(merge_flat_dir(local_obj, remote_obj))
}

/// The keys a platform envelope is made of. A payload for a
/// `platform_specific` key is stored server-side as `{darwin: …, win32: …,
/// linux: …}` and unwrapped to the current platform's slot on pull.
pub const PLATFORM_KEYS: [&str; 3] = ["darwin", "win32", "linux"];

/// Remove platform-envelope keys that have leaked *into* a platform's
/// content.
///
/// The failure this guards against: if a settings object ever contains a
/// top-level `win32` (or `darwin`/`linux`) key — because an older writer once
/// wrote the whole envelope to disk instead of one slot — `wrap_platform`
/// faithfully wraps that object as `{darwin: {…, win32: {…}}}`, the pull
/// side unwraps `darwin` and writes the nested `win32` right back, and the
/// stale copy round-trips forever. Claude Code ignores the unknown key, but
/// the file carries a permanently stale snapshot and config linters flag it.
///
/// Stripping at the platform boundary makes the leak self-healing in both
/// directions. Only object payloads are touched; anything else passes
/// through unchanged.
pub fn strip_nested_platform_keys(value: Value) -> Value {
    match value {
        Value::Object(mut obj) => {
            for key in PLATFORM_KEYS {
                obj.remove(key);
            }
            Value::Object(obj)
        }
        other => other,
    }
}

/// One `(session, machine)` entry of the session index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct IndexEntry {
    seq: u64,
    at: DateTime<Utc>,
}

/// The well-formed entries of an index value. Anything else (a non-object,
/// an entry without a `u64` seq or an RFC3339 `at`) is dropped, so both
/// sides of a merge agree on what counts.
fn parse_session_index(v: &Value) -> BTreeMap<(String, String), IndexEntry> {
    let mut out = BTreeMap::new();
    let Some(sessions) = v.as_object() else {
        return out;
    };
    for (sid, machines) in sessions {
        let Some(machines) = machines.as_object() else {
            continue;
        };
        for (machine, entry) in machines {
            let seq = entry.get("seq").and_then(Value::as_u64);
            let at = entry
                .get("at")
                .and_then(Value::as_str)
                .and_then(registry::parse_timestamp);
            if let (Some(seq), Some(at)) = (seq, at) {
                out.insert((sid.clone(), machine.clone()), IndexEntry { seq, at });
            }
        }
    }
    out
}

/// Merge two session index values: every session and machine either side
/// has, keeping per `(session, machine)` the higher `seq` (on a tie, the
/// later `at`). Then entries older than [`SESSION_INDEX_MAX_AGE_DAYS`]
/// before the newest one are dropped.
///
/// Commutative: the result is a function of the union alone, timestamps
/// are rewritten in one spelling, and keys come out sorted. Pruning is
/// relative to the newest entry, capped at this machine's clock: one entry
/// stamped by a machine whose clock runs days fast must not prune every
/// other entry (this machine's owed one included, which would then count as
/// already written). Pruning only ever removes, so the cap cannot keep two
/// machines from converging.
pub fn merge_session_index(local: &Value, remote: &Value) -> Value {
    let mut merged = parse_session_index(local);
    for (key, entry) in parse_session_index(remote) {
        let slot = merged.entry(key).or_insert(entry);
        if entry > *slot {
            *slot = entry;
        }
    }
    let cutoff = merged
        .values()
        .map(|e| e.at)
        .max()
        .map(|newest| newest.min(Utc::now()) - chrono::Duration::days(SESSION_INDEX_MAX_AGE_DAYS));
    let mut out: BTreeMap<String, serde_json::Map<String, Value>> = BTreeMap::new();
    for ((sid, machine), entry) in merged {
        if cutoff.is_some_and(|c| entry.at < c) {
            continue;
        }
        let mut e = serde_json::Map::new();
        e.insert("seq".to_string(), Value::from(entry.seq));
        e.insert("at".to_string(), Value::String(registry::format_timestamp(entry.at)));
        out.entry(sid).or_default().insert(machine, Value::Object(e));
    }
    Value::Object(out.into_iter().map(|(sid, m)| (sid, Value::Object(m))).collect())
}

/// Dispatch to the right merge strategy for a given sync key.
pub fn merge_for_key(key: &str, local: &Value, remote: &Value) -> Value {
    // Before the null check: a missing server copy still goes through the
    // merge, which settles the value's shape and spelling.
    if key == SESSION_INDEX_KEY {
        return merge_session_index(local, remote);
    }
    // Also before it: the merge drops the local-only revived names, which
    // must never reach the server.
    if key.starts_with("claude-memory:") {
        return merge_memory(local, remote);
    }
    if remote.is_null() {
        return local.clone();
    }

    if matches!(
        key,
        "claude-mcp-servers"
            | "claude-agents"
            | "claude-commands"
            | "claude-hooks"
            | "claude-skills"
            | "claude-rules"
            | "claude-output-styles"
    ) {
        return merge_dir_payload(local, remote);
    }

    // The project registry is edited on several machines. Whole-blob
    // local-wins let the last machine to push erase every project the other
    // one had added, so it is merged record by record instead: records only
    // one side has are kept, deletes need a tombstone.
    if key == registry::PROJECTS_KEY {
        return registry::merge(local, remote, registry::current_platform());
    }

    // Local-wins for the remaining keys (settings, claude-md, claude-env,
    // claude-statusline, claude-settings(-local), claude-json-mcp-servers,
    // mnemos-user-settings).
    local.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strip_nested_platform_keys_removes_leaked_envelope_keys() {
        let leaked = json!({
            "model": "current",
            "hooks": {"PermissionRequest": []},
            "win32": {"model": "stale"},
            "darwin": {"model": "stale"},
            "linux": {"model": "stale"}
        });
        let cleaned = strip_nested_platform_keys(leaked);
        let obj = cleaned.as_object().unwrap();
        assert!(!obj.contains_key("win32"));
        assert!(!obj.contains_key("darwin"));
        assert!(!obj.contains_key("linux"));
        assert_eq!(obj.len(), 2);
    }

    #[test]
    fn strip_nested_platform_keys_leaves_real_content_and_order_alone() {
        let value = json!({
            "$schema": "s",
            "model": "m",
            "hooks": {"Stop": [{"hooks": []}]},
            "permissions": {"defaultMode": "default"}
        });
        let before: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
        let cleaned = strip_nested_platform_keys(value.clone());
        let after: Vec<String> = cleaned.as_object().unwrap().keys().cloned().collect();
        assert_eq!(cleaned, value);
        assert_eq!(before, after);
    }

    #[test]
    fn strip_nested_platform_keys_passes_non_objects_through() {
        assert_eq!(strip_nested_platform_keys(json!("raw text")), json!("raw text"));
        assert_eq!(strip_nested_platform_keys(json!(null)), json!(null));
        assert_eq!(strip_nested_platform_keys(json!([1, 2])), json!([1, 2]));
    }

    #[test]
    fn merge_memory_picks_higher_mtime() {
        let local = json!({
            "files": { "a.md": {"content": "L", "mtime": 100} },
            "memoryMd": null
        });
        let remote = json!({
            "files": { "a.md": {"content": "R", "mtime": 200} },
            "memoryMd": null
        });
        let merged = merge_memory(&local, &remote);
        assert_eq!(
            merged
                .pointer("/files/a.md/content")
                .and_then(Value::as_str),
            Some("R"),
        );
    }

    #[test]
    fn merge_memory_unions_files() {
        let local = json!({"files": {"a.md": {"content": "A", "mtime": 1}}, "memoryMd": null});
        let remote = json!({"files": {"b.md": {"content": "B", "mtime": 1}}, "memoryMd": null});
        let merged = merge_memory(&local, &remote);
        assert!(merged.pointer("/files/a.md").is_some());
        assert!(merged.pointer("/files/b.md").is_some());
    }

    #[test]
    fn a_memory_delete_propagates_and_is_listed_for_older_builds() {
        let local = json!({
            "files": {"a.md": {"content": "A", "mtime": 1}},
            "memoryMd": null,
            "__deleted_at": {"memory/b.md": 200, "MEMORY.md": 200}
        });
        let remote = json!({
            "files": {"b.md": {"content": "B", "mtime": 100}},
            "memoryMd": {"content": "M", "mtime": 150}
        });
        let merged = merge_memory(&local, &remote);
        assert!(merged.pointer("/files/b.md").is_none());
        assert_eq!(merged["memoryMd"], Value::Null);
        assert_eq!(merged["__deleted_at"], json!({"memory/b.md": 200, "MEMORY.md": 200}));
        assert_eq!(merged["__deleted_files"], json!(["MEMORY.md", "memory/b.md"]));
        // Settled: merging again changes nothing.
        assert_eq!(merge_memory(&merged, &remote), merged);
    }

    #[test]
    fn a_file_recreated_after_its_delete_survives() {
        let server = json!({
            "files": {},
            "memoryMd": null,
            "__deleted_at": {"memory/b.md": 200},
            "__deleted_files": ["memory/b.md"]
        });
        let recreated = json!({"files": {"b.md": {"content": "B2", "mtime": 300}}, "memoryMd": null});
        let merged = merge_memory(&recreated, &server);
        assert_eq!(merged.pointer("/files/b.md/content"), Some(&json!("B2")));
        assert!(merged.get("__deleted_at").is_none());
        assert!(merged.get("__deleted_files").is_none(), "older builds must stop deleting it");

        // An older copy of the file (the other machine never saw the delete)
        // stays deleted.
        let stale = json!({"files": {"b.md": {"content": "B", "mtime": 100}}, "memoryMd": null});
        let merged = merge_memory(&stale, &server);
        assert!(merged.pointer("/files/b.md").is_none());
        assert_eq!(merged["__deleted_at"]["memory/b.md"], 200);
    }

    #[test]
    fn a_revived_file_clears_its_tombstone_whatever_its_time() {
        // Moved back from a backup: the file keeps its old mtime.
        let local = json!({
            "files": {"b.md": {"content": "B", "mtime": 100}},
            "memoryMd": null,
            "__revived": ["memory/b.md"]
        });
        let server = json!({
            "files": {},
            "memoryMd": null,
            "__deleted_at": {"memory/b.md": 200},
            "__deleted_files": ["memory/b.md"]
        });
        let merged = merge_memory(&local, &server);
        assert_eq!(merged.pointer("/files/b.md/content"), Some(&json!("B")));
        assert!(merged.get("__deleted_at").is_none());
        assert!(merged.get("__deleted_files").is_none());
        assert!(merged.get("__revived").is_none(), "local only");
        // Never sent, even with no server copy at all.
        let first = merge_for_key("claude-memory:p:o/r", &local, &Value::Null);
        assert!(first.get("__revived").is_none());
        assert_eq!(first.pointer("/files/b.md/content"), Some(&json!("B")));
    }

    #[test]
    fn a_legacy_delete_list_entry_never_deletes() {
        // An older writer's list, with no time: a file only it names (a bare
        // string entry has mtime 0) is kept, and the name passes through.
        let local = json!({"files": {"b.md": "QkI="}, "memoryMd": null});
        let remote = json!({"files": {}, "memoryMd": null, "__deleted_files": ["memory/b.md"]});
        let merged = merge_memory(&local, &remote);
        assert_eq!(merged.pointer("/files/b.md/content"), Some(&json!("QkI=")));
        assert_eq!(merged["__deleted_files"], json!(["memory/b.md"]));
        assert!(merged.get("__deleted_at").is_none());
    }

    #[test]
    fn memory_merge_is_symmetric() {
        let a = json!({
            "files": {
                "x.md": {"content": "X-a", "mtime": 10},
                "y.md": {"content": "Y-a", "mtime": 50}
            },
            "memoryMd": {"content": "M-a", "mtime": 5},
            "__deleted_at": {"memory/z.md": 40, "memory/y.md": 30},
            "__deleted_files": ["memory/z.md", "memory/y.md", "memory/old.md"]
        });
        let b = json!({
            "files": {
                "x.md": {"content": "X-b", "mtime": 20},
                "z.md": {"content": "Z-b", "mtime": 30},
                "w.md": {"content": "W-b", "mtime": 1}
            },
            "memoryMd": {"content": "M-b", "mtime": 7},
            "__deleted_at": {"memory/w.md": 5, "memory/z.md": 35}
        });
        let ab = merge_memory(&a, &b);
        assert_eq!(ab, merge_memory(&b, &a));
        assert_eq!(merge_memory(&ab, &ab), ab);
        assert_eq!(ab.pointer("/files/x.md/content"), Some(&json!("X-b")));
        assert_eq!(ab.pointer("/files/y.md/content"), Some(&json!("Y-a")), "edited after the delete");
        assert!(ab.pointer("/files/z.md").is_none());
        assert!(ab.pointer("/files/w.md").is_none());
        assert_eq!(ab["memoryMd"]["content"], "M-b");
        assert_eq!(ab["__deleted_at"], json!({"memory/w.md": 5, "memory/z.md": 40}));
        assert_eq!(
            ab["__deleted_files"],
            json!(["memory/old.md", "memory/w.md", "memory/z.md"])
        );
    }

    #[test]
    fn projects_push_merges_remote_only_records_in() {
        // The server holds a project this machine has never seen; pushing the
        // local registry must add ours next to it, not replace it.
        let local = json!({
            "projects": [{
                "id": "local-1", "name": "mnemos", "path": "C:\\code\\mnemos",
                "createdAt": "2026-09-01T00:00:00Z"
            }],
            "machineId": "pc"
        });
        let remote = json!({
            "projects": [{
                "id": "remote-1", "name": "Sprocket", "path": "/Users/alice/code/Sprocket",
                "createdAt": "2026-08-01T00:00:00Z"
            }],
            "machineId": "mac"
        });
        let merged = merge_for_key("projects", &local, &remote);
        let ids: Vec<&str> = merged["projects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["local-1", "remote-1"]);
        // Top-level keys stay local's.
        assert_eq!(merged["machineId"], "pc");
    }

    #[test]
    fn projects_push_keeps_the_other_machines_path_slot() {
        // Same record on both sides: this machine owns its own `paths` slot,
        // the server's copy owns the rest.
        let this = registry::current_platform();
        let other = registry::PLATFORMS.iter().find(|p| **p != this).copied().unwrap();
        let local = json!({"projects": [{
            "id": "p", "name": "TCB", "path": "/x", "createdAt": "2026-09-01T00:00:00Z",
            "paths": { this: "/here" }
        }]});
        let remote = json!({"projects": [{
            "id": "p", "name": "TCB", "path": "/x", "createdAt": "2026-09-01T00:00:00Z",
            "paths": { this: "/stale", other: "/there" }
        }]});
        let merged = merge_for_key("projects", &local, &remote);
        let paths = &merged["projects"][0]["paths"];
        assert_eq!(paths[this], "/here");
        assert_eq!(paths[other], "/there");
    }

    const S1: &str = "0b6f3c1e-8a3b-4f7e-9d2a-1c2d3e4f5a6b";
    const S2: &str = "7f1d2c3b-0a9e-4d8c-b7a6-5f4e3d2c1b0a";

    /// An index value from `(sid, machine, seq, at)` rows.
    fn index(rows: &[(&str, &str, u64, &str)]) -> Value {
        let mut out = serde_json::Map::new();
        for (sid, machine, seq, at) in rows {
            let machines = out
                .entry(sid.to_string())
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .unwrap();
            machines.insert(machine.to_string(), json!({"seq": seq, "at": at}));
        }
        Value::Object(out)
    }

    #[test]
    fn session_index_merge_unions_and_keeps_the_higher_seq() {
        let mac = index(&[
            (S1, "mac", 3, "2026-09-24T10:00:00Z"),
            (S1, "pc", 1, "2026-09-24T09:00:00Z"),
        ]);
        let pc = index(&[
            (S1, "pc", 2, "2026-09-24T09:30:00Z"),
            (S2, "pc", 1, "2026-09-24T09:40:00Z"),
        ]);
        let merged = merge_for_key(SESSION_INDEX_KEY, &mac, &pc);
        assert_eq!(
            merged,
            index(&[
                (S1, "mac", 3, "2026-09-24T10:00:00Z"),
                (S1, "pc", 2, "2026-09-24T09:30:00Z"),
                (S2, "pc", 1, "2026-09-24T09:40:00Z"),
            ])
        );
        // A lower seq never wins, even when it is newer: a machine that lost
        // its local state re-uploads from the listing's next number, never
        // a smaller one.
        let stale = index(&[(S1, "mac", 2, "2026-09-24T11:00:00Z")]);
        assert_eq!(merge_for_key(SESSION_INDEX_KEY, &stale, &merged)[S1]["mac"]["seq"], 3);
    }

    #[test]
    fn session_index_merge_is_commutative_and_idempotent() {
        let a = index(&[
            (S1, "mac", 3, "2026-09-24T10:00:00Z"),
            (S2, "mac", 7, "2026-09-24T10:05:00+02:00"),
        ]);
        let b = index(&[
            (S1, "mac", 3, "2026-09-24T10:00:01Z"),
            (S1, "pc", 4, "2026-09-24T08:00:00Z"),
            (S2, "mac", 6, "2026-09-24T12:00:00Z"),
        ]);
        let ab = merge_session_index(&a, &b);
        let ba = merge_session_index(&b, &a);
        assert_eq!(ab, ba);
        assert_eq!(merge_session_index(&ab, &ab), ab);
        assert_eq!(merge_session_index(&ab, &a), ab);
        // Same seq: the later `at` wins, whichever side has it.
        assert_eq!(ab[S1]["mac"]["at"], "2026-09-24T10:00:01Z");
        // One spelling out, whatever came in.
        assert_eq!(ab[S2]["mac"], json!({"seq": 7, "at": "2026-09-24T08:05:00Z"}));
    }

    #[test]
    fn session_index_merge_drops_malformed_and_stale_entries() {
        let local = json!({
            S1: {
                "mac": {"seq": 2, "at": "2026-09-24T10:00:00Z"},
                // Two days before the newest entry: kept.
                "pc": {"seq": 4, "at": "2026-09-22T10:00:00Z"},
                "bad-seq": {"seq": "2", "at": "2026-09-24T10:00:00Z"},
                "bad-at": {"seq": 2, "at": "yesterday"},
                "not-an-object": 5
            },
            S2: "not an object",
            // Four days before the newest entry: gone. The whole session goes
            // with its only machine.
            "5d2e1c0b-aaaa-4bbb-8ccc-dddddddddddd": {
                "pc": {"seq": 9, "at": "2026-09-20T09:59:59Z"}
            }
        });
        let merged = merge_session_index(&local, &Value::Null);
        assert_eq!(
            merged,
            index(&[
                (S1, "mac", 2, "2026-09-24T10:00:00Z"),
                (S1, "pc", 4, "2026-09-22T10:00:00Z"),
            ])
        );
        // Nothing at all, from either side, is an empty index.
        assert_eq!(merge_session_index(&Value::Null, &json!("junk")), json!({}));
    }

    #[test]
    fn session_index_entry_from_a_fast_clock_does_not_prune_the_present() {
        let at = |days: i64| registry::format_timestamp(Utc::now() + chrono::Duration::days(days));
        let (now, stale, fast) = (at(0), at(-4), at(40));
        // This machine's owed entry, and one the server has from a machine
        // whose clock runs 40 days fast.
        let owed = index(&[(S1, "pc", 2, &now)]);
        let server = index(&[(S2, "fast", 1, &fast), (S2, "old", 7, &stale)]);
        let merged = merge_session_index(&owed, &server);
        assert_eq!(merged[S1]["pc"]["seq"], 2, "the owed entry survives");
        assert_eq!(merged[S2]["fast"]["seq"], 1);
        assert!(merged[S2].get("old").is_none(), "still pruned by the local clock");
        assert_ne!(merged, server, "so the owed entry is written, not taken as already there");
    }

    #[test]
    fn session_index_merge_without_a_server_copy_still_normalizes() {
        let local = index(&[(S1, "mac", 1, "2026-09-24T12:00:00+02:00")]);
        assert_eq!(
            merge_for_key(SESSION_INDEX_KEY, &local, &Value::Null),
            index(&[(S1, "mac", 1, "2026-09-24T10:00:00Z")])
        );
    }

    #[test]
    fn settings_stay_local_wins() {
        let merged = merge_for_key("settings", &json!({"a": 1}), &json!({"a": 2, "b": 3}));
        assert_eq!(merged, json!({"a": 1}));
    }

    #[test]
    fn merge_dir_local_wins() {
        let local = json!({"a.md": "AAAA"});
        let remote = json!({"a.md": "BBBB", "b.md": "CCCC"});
        let merged = merge_dir_payload(&local, &remote);
        assert_eq!(merged.pointer("/a.md").and_then(Value::as_str), Some("AAAA"));
        assert_eq!(merged.pointer("/b.md").and_then(Value::as_str), Some("CCCC"));
    }

    #[test]
    fn a_directory_delete_propagates_through_the_merge() {
        // This machine deleted b.md; the server still has it.
        let local = json!({"a.md": "A", "__deleted": {"b.md": 200}});
        let remote = json!({"a.md": "A", "b.md": "B", "c.md": "C"});
        let merged = merge_dir_payload(&local, &remote);
        assert_eq!(merged, json!({"a.md": "A", "c.md": "C", "__deleted": {"b.md": 200}}));

        // Another machine that still has no b.md merges the tombstone in.
        let other = json!({"a.md": "A", "c.md": "C"});
        assert_eq!(merge_dir_payload(&other, &merged), merged);

        // The later of two tombstones wins.
        let later = json!({"__deleted": {"b.md": 300, "c.md": 50}});
        let merged2 = merge_dir_payload(&later, &merged);
        assert_eq!(merged2["__deleted"], json!({"b.md": 300, "c.md": 50}));
        assert!(merged2.get("c.md").is_none());
    }

    #[test]
    fn a_file_the_local_side_has_outlives_a_tombstone() {
        // Re-created (or edited) here after the other machine deleted it;
        // the caller has already removed a local copy older than the delete.
        let local = json!({"b.md": "B2"});
        let remote = json!({"a.md": "A", "__deleted": {"b.md": 200}});
        assert_eq!(
            merge_dir_payload(&local, &remote),
            json!({"a.md": "A", "b.md": "B2"})
        );
    }

    #[test]
    fn a_side_carrying_a_file_and_its_own_tombstone_counts_as_deleted() {
        // What an older build's union leaves on the server.
        let old_build = json!({"a.md": "A", "b.md": "B", "__deleted": {"b.md": 200}});
        let disk = json!({"a.md": "A"});
        let expected = json!({"a.md": "A", "__deleted": {"b.md": 200}});
        assert_eq!(merge_dir_payload(&disk, &old_build), expected);
        assert_eq!(merge_dir_payload(&old_build, &disk), expected);
    }

    #[test]
    fn directory_tombstones_merge_per_platform_slot() {
        let local = json!({"darwin": {"a.py": "L", "__deleted": {"b.py": 9}}});
        let remote = json!({
            "darwin": {"a.py": "R", "b.py": "X"},
            "win32": {"c.py": "Y", "__deleted": {"d.py": 3}}
        });
        let merged = merge_dir_payload(&local, &remote);
        assert_eq!(
            merged,
            json!({
                "darwin": {"a.py": "L", "__deleted": {"b.py": 9}},
                "win32": {"c.py": "Y", "__deleted": {"d.py": 3}}
            })
        );
    }

    #[test]
    fn merge_dir_platform_envelope() {
        let local = json!({"darwin": {"a.py": "L"}});
        let remote = json!({"darwin": {"a.py": "R", "b.py": "X"}, "win32": {"c.py": "Y"}});
        let merged = merge_dir_payload(&local, &remote);
        assert_eq!(
            merged.pointer("/darwin/a.py").and_then(Value::as_str),
            Some("L")
        );
        assert_eq!(
            merged.pointer("/darwin/b.py").and_then(Value::as_str),
            Some("X")
        );
        assert_eq!(
            merged.pointer("/win32/c.py").and_then(Value::as_str),
            Some("Y")
        );
    }
}

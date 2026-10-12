// SPDX-License-Identifier: AGPL-3.0-only
//! Per-machine approval gates for synced content that runs code.
//!
//! Some synced `~/.claude` content is executed on the machine it lands on:
//! hook scripts, the commands and permissions in Claude Code's
//! `settings.json` (`hooks`, `statusLine`, `apiKeyHelper`, `env`,
//! `permissions`, ...), `statusline.py`, MCP server scripts under
//! `~/.claude/mcp-servers/`, the `mcpServers` entries of `~/.claude.json`
//! (which Claude Code and Mnemos spawn), and agent, command and skill files
//! (their frontmatter can grant tools, set `permissionMode`, carry hooks or
//! inline `!` commands). Whoever can write the server copy, or a team's
//! copy, could otherwise run code on every machine that pulls it.
//!
//! The rules:
//! - Hooks are opt-in per machine ([`GateStore::hooks_sync`], off by
//!   default). While off, `claude-hooks` and `claude-statusline` are neither
//!   pushed nor pulled here, and the command keys of `settings.json` /
//!   `settings.local.json` are frozen: a pull keeps this machine's commands
//!   and a push keeps the server's (so an older build that still syncs them
//!   loses nothing).
//! - Agents, commands and skills are not behind the opt-in (they always
//!   sync), but every change to them is gated like a hook's.
//! - Every gated item that a pull would change (a new or different hook,
//!   agent, command or skill file, command set, MCP server entry or MCP
//!   script) is written only when its content hash was approved on this
//!   machine. Otherwise this machine's copy stays, and the item is listed as
//!   a [`PendingApproval`] with a diff. The same holds for a deletion: a
//!   gated file another machine (or the team) deleted stays here until the
//!   deletion is approved, and a file the pulled copy merely lacks is never
//!   removed.
//! - While an item is pending, a push sends the server's copy of it, never
//!   this machine's older one, so the other machine's change is not undone.
//! - Approvals, dismissals, pending items and the opt-in live in
//!   `sync_gates.json` in the Mnemos store dir. That file is never synced.

use std::collections::BTreeMap;
use std::path::Path;

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::merge::DIR_TOMBSTONES_KEY;

/// Local-only file in the Mnemos store dir holding this machine's gates.
pub const GATES_FILE_NAME: &str = "sync_gates.json";

pub const HOOKS_KEY: &str = "claude-hooks";
pub const STATUSLINE_KEY: &str = "claude-statusline";
pub const CLAUDE_SETTINGS_KEY: &str = "claude-settings";
pub const CLAUDE_SETTINGS_LOCAL_KEY: &str = "claude-settings-local";
pub const MCP_DIR_KEY: &str = "claude-mcp-servers";
/// The `-v2` key (mcpServers without `env`/`headers`); the old key is never
/// applied, so it needs no gate.
pub const MCP_JSON_KEY: &str = "claude-json-mcp-servers-v2";
pub const AGENTS_KEY: &str = "claude-agents";
pub const COMMANDS_KEY: &str = "claude-commands";
pub const SKILLS_KEY: &str = "claude-skills";

/// Every sync key with gated content.
pub const GATED_KEYS: &[&str] = &[
    HOOKS_KEY,
    STATUSLINE_KEY,
    CLAUDE_SETTINGS_KEY,
    CLAUDE_SETTINGS_LOCAL_KEY,
    MCP_DIR_KEY,
    MCP_JSON_KEY,
    AGENTS_KEY,
    COMMANDS_KEY,
    SKILLS_KEY,
];

/// Claude Code `settings.json` keys whose values make Claude Code run a
/// program, change what a program it runs does, or let it run programs
/// without asking (`permissions` with `defaultMode` or `allow`, project MCP
/// servers started without the trust prompt, the sandbox). Gated as one
/// item. A name Claude Code does not know costs nothing here.
pub const CLAUDE_SETTINGS_COMMAND_KEYS: &[&str] = &[
    "hooks",
    "statusLine",
    "apiKeyHelper",
    "awsAuthRefresh",
    "awsCredentialExport",
    "otelHeadersHelper",
    "env",
    "enabledPlugins",
    "extraKnownMarketplaces",
    "permissions",
    "enableAllProjectMcpServers",
    "enabledMcpjsonServers",
    "fileSuggestion",
    "sandbox",
];

const MAX_HASHES_PER_ITEM: usize = 16;
const MAX_DIFF_LINES: usize = 80;
const MAX_DIFF_CHARS: usize = 6000;
/// Above this many line pairs the diff falls back to listing the new text.
const MAX_DIFF_CELLS: usize = 250_000;

/// How a gated key's value splits into approvable items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateKind {
    /// A directory key: one item per file, `<key>/<rel>`.
    Directory,
    /// `settings.json` / `settings.local.json`: the command keys, one item
    /// `<key>#commands`.
    SettingsCommands,
    /// `statusline.py`: the whole text, one item `<key>`.
    WholeText,
    /// `~/.claude.json` `mcpServers`: one item per server, `<key>/<name>`.
    McpEntries,
}

pub fn gate_kind(key: &str) -> Option<GateKind> {
    match key {
        HOOKS_KEY | MCP_DIR_KEY | AGENTS_KEY | COMMANDS_KEY | SKILLS_KEY => {
            Some(GateKind::Directory)
        }
        CLAUDE_SETTINGS_KEY | CLAUDE_SETTINGS_LOCAL_KEY => Some(GateKind::SettingsCommands),
        STATUSLINE_KEY => Some(GateKind::WholeText),
        MCP_JSON_KEY => Some(GateKind::McpEntries),
        _ => None,
    }
}

/// Keys (or, for settings, the command part of them) that only sync on a
/// machine that opted in to hooks sync.
pub fn needs_hooks_opt_in(key: &str) -> bool {
    matches!(
        key,
        HOOKS_KEY | STATUSLINE_KEY | CLAUDE_SETTINGS_KEY | CLAUDE_SETTINGS_LOCAL_KEY
    )
}

/// Keys that are not synced at all on a machine without the hooks opt-in.
/// (`settings.json` still syncs, with its commands frozen.)
pub fn skipped_without_hooks_opt_in(key: &str) -> bool {
    matches!(key, HOOKS_KEY | STATUSLINE_KEY)
}

/// Where a pulled value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GateSource {
    /// This account's own state (another of the user's machines).
    Personal,
    /// The selected team's shared blob.
    Team,
}

impl GateSource {
    fn as_str(self) -> &'static str {
        match self {
            GateSource::Personal => "personal",
            GateSource::Team => "team",
        }
    }
}

/// An incoming change waiting for this machine's approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingApproval {
    /// `<source>:<item>`; what [`GateStore::approve`] and
    /// [`GateStore::dismiss`] take.
    pub id: String,
    pub key: String,
    pub item: String,
    pub source: GateSource,
    /// What the item is, for people.
    pub title: String,
    /// SHA-256 of the incoming content.
    pub hash: String,
    /// Line diff from this machine's copy to the incoming one (truncated).
    pub diff: String,
    /// Nothing of this item exists on this machine yet.
    pub new_item: bool,
}

/// This machine's gate state, persisted in [`GATES_FILE_NAME`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateStore {
    /// Hooks sync opted in on this machine.
    #[serde(default, rename = "hooksSync")]
    pub hooks_sync: bool,
    /// Item -> approved content hashes (oldest first, capped).
    #[serde(default)]
    pub approved: BTreeMap<String, Vec<String>>,
    /// Item -> dismissed content hashes: never applied, no longer listed.
    #[serde(default)]
    pub dismissed: BTreeMap<String, Vec<String>>,
    /// Pending id -> the change.
    #[serde(default)]
    pub pending: BTreeMap<String, PendingApproval>,
}

impl GateStore {
    /// The store at `path`; the default (hooks off, nothing approved) when
    /// it is missing or unreadable.
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Write through a temp file and a rename.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(std::io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, path)
    }

    pub fn is_approved(&self, item: &str, hash: &str) -> bool {
        self.approved
            .get(item)
            .is_some_and(|h| h.iter().any(|x| x == hash))
    }

    pub fn is_dismissed(&self, item: &str, hash: &str) -> bool {
        self.dismissed
            .get(item)
            .is_some_and(|h| h.iter().any(|x| x == hash))
    }

    /// Pending items, ordered by id.
    pub fn pending_list(&self) -> Vec<PendingApproval> {
        self.pending.values().cloned().collect()
    }

    /// Replace the pending items of `key` from `source` with `items`.
    /// Returns whether anything changed.
    pub fn replace_pending(
        &mut self,
        key: &str,
        source: GateSource,
        items: Vec<PendingApproval>,
    ) -> bool {
        let before = self.pending.clone();
        self.pending
            .retain(|_, p| !(p.key == key && p.source == source));
        for p in items {
            self.pending.insert(p.id.clone(), p);
        }
        before != self.pending
    }

    /// Approve the pending items `ids`: their content hashes become
    /// approved for their items, and they stop being pending. Returns the
    /// keys they belong to.
    pub fn approve(&mut self, ids: &[String]) -> Vec<String> {
        let mut keys = Vec::new();
        for id in ids {
            if let Some(p) = self.pending.remove(id) {
                push_capped(self.approved.entry(p.item.clone()).or_default(), p.hash);
                if !keys.contains(&p.key) {
                    keys.push(p.key);
                }
            }
        }
        keys
    }

    /// Dismiss the pending items `ids`: their content is never applied and
    /// no longer listed, and pushes stop protecting the server's copy.
    pub fn dismiss(&mut self, ids: &[String]) -> Vec<String> {
        let mut keys = Vec::new();
        for id in ids {
            if let Some(p) = self.pending.remove(id) {
                push_capped(self.dismissed.entry(p.item.clone()).or_default(), p.hash);
                if !keys.contains(&p.key) {
                    keys.push(p.key);
                }
            }
        }
        keys
    }

    /// Approve `hash` for `item` directly (content this machine itself
    /// wrote, or a test).
    pub fn approve_hash(&mut self, item: &str, hash: &str) {
        let list = self.approved.entry(item.to_string()).or_default();
        if !list.iter().any(|h| h == hash) {
            push_capped(list, hash.to_string());
        }
    }
}

fn push_capped(list: &mut Vec<String>, hash: String) {
    list.retain(|h| *h != hash);
    list.push(hash);
    while list.len() > MAX_HASHES_PER_ITEM {
        list.remove(0);
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let out = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in out {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// The approvable items of a key's slot (this machine's part of its value),
/// as item name -> canonical bytes.
pub fn items(kind: GateKind, key: &str, slot: &Value) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    match kind {
        GateKind::Directory => {
            if let Some(map) = slot.as_object() {
                for (rel, b64) in map {
                    if rel == DIR_TOMBSTONES_KEY {
                        continue;
                    }
                    let Some(bytes) = b64.as_str().and_then(|s| B64.decode(s).ok()) else {
                        continue;
                    };
                    out.insert(format!("{key}/{rel}"), bytes);
                }
            }
        }
        GateKind::SettingsCommands => {
            let subset = commands_subset(slot);
            if !subset.is_empty() {
                let bytes = serde_json::to_vec(&Value::Object(subset)).unwrap_or_default();
                out.insert(format!("{key}#commands"), bytes);
            }
        }
        GateKind::WholeText => {
            if let Some(text) = slot.as_str() {
                out.insert(key.to_string(), text.as_bytes().to_vec());
            }
        }
        GateKind::McpEntries => {
            if let Some(map) = slot.as_object() {
                for (name, entry) in map {
                    let bytes = serde_json::to_vec(entry).unwrap_or_default();
                    out.insert(format!("{key}/{name}"), bytes);
                }
            }
        }
    }
    out
}

/// Set (or with `None`, remove) one item of a slot.
pub fn set_item(kind: GateKind, key: &str, slot: &mut Value, item: &str, bytes: Option<&[u8]>) {
    match kind {
        GateKind::Directory | GateKind::McpEntries => {
            let Some(name) = item.strip_prefix(key).and_then(|r| r.strip_prefix('/')) else {
                return;
            };
            if !slot.is_object() {
                *slot = Value::Object(Map::new());
            }
            let Some(map) = slot.as_object_mut() else {
                return;
            };
            match bytes {
                None => {
                    map.remove(name);
                }
                Some(bytes) if kind == GateKind::Directory => {
                    map.insert(name.to_string(), Value::String(B64.encode(bytes)));
                }
                Some(bytes) => match serde_json::from_slice::<Value>(bytes) {
                    Ok(entry) => {
                        map.insert(name.to_string(), entry);
                    }
                    Err(_) => {
                        map.remove(name);
                    }
                },
            }
        }
        GateKind::SettingsCommands => {
            let subset = bytes
                .and_then(|b| serde_json::from_slice::<Value>(b).ok())
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();
            *slot = with_commands(std::mem::take(slot), &subset);
        }
        GateKind::WholeText => {
            *slot = match bytes {
                Some(bytes) => Value::String(String::from_utf8_lossy(bytes).into_owned()),
                None => Value::Null,
            };
        }
    }
}

/// The command keys of a Claude Code settings object.
pub fn commands_subset(settings: &Value) -> Map<String, Value> {
    settings
        .as_object()
        .map(|obj| {
            obj.iter()
                .filter(|(k, _)| CLAUDE_SETTINGS_COMMAND_KEYS.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// `settings` with its command keys replaced by `subset`. A non-object
/// (a missing file) stays as it is when `subset` is empty.
pub fn with_commands(settings: Value, subset: &Map<String, Value>) -> Value {
    let mut obj = match settings {
        Value::Object(obj) => obj,
        other if subset.is_empty() => return other,
        _ => Map::new(),
    };
    obj.retain(|k, _| !CLAUDE_SETTINGS_COMMAND_KEYS.contains(&k.as_str()));
    for (k, v) in subset {
        obj.insert(k.clone(), v.clone());
    }
    Value::Object(obj)
}

fn title_for(key: &str, item: &str, source: GateSource) -> String {
    let name = item
        .strip_prefix(key)
        .map(|r| r.trim_start_matches('/'))
        .unwrap_or(item);
    let base = match key {
        HOOKS_KEY => format!("Hook ~/.claude/hooks/{name}"),
        MCP_DIR_KEY => format!("MCP server file ~/.claude/mcp-servers/{name}"),
        AGENTS_KEY => format!("Agent ~/.claude/agents/{name}"),
        COMMANDS_KEY => format!("Command ~/.claude/commands/{name}"),
        SKILLS_KEY => format!("Skill file ~/.claude/skills/{name}"),
        MCP_JSON_KEY => format!("MCP server '{name}' in ~/.claude.json"),
        CLAUDE_SETTINGS_KEY => {
            "Commands in ~/.claude/settings.json (hooks, statusLine, env, ...)".to_string()
        }
        CLAUDE_SETTINGS_LOCAL_KEY => {
            "Commands in ~/.claude/settings.local.json (hooks, statusLine, env, ...)".to_string()
        }
        STATUSLINE_KEY => "~/.claude/statusline.py".to_string(),
        _ => item.to_string(),
    };
    match source {
        GateSource::Personal => base,
        GateSource::Team => format!("{base} (from your team)"),
    }
}

/// The items of `server_slot` that would change this machine (differ from
/// `local_slot`) and were neither approved nor dismissed here.
pub fn pending_items(
    kind: GateKind,
    key: &str,
    server_slot: &Value,
    local_slot: &Value,
    store: &GateStore,
    source: GateSource,
) -> Vec<PendingApproval> {
    let local = items(kind, key, local_slot);
    let mut out = Vec::new();
    for (item, bytes) in items(kind, key, server_slot) {
        let current = local.get(&item);
        if current == Some(&bytes) {
            continue;
        }
        let hash = sha256_hex(&bytes);
        if store.is_approved(&item, &hash) || store.is_dismissed(&item, &hash) {
            continue;
        }
        out.push(PendingApproval {
            id: format!("{}:{item}", source.as_str()),
            key: key.to_string(),
            title: title_for(key, &item, source),
            diff: line_diff(current.map(Vec::as_slice), &bytes),
            new_item: current.is_none(),
            hash,
            item,
            source,
        });
    }
    if kind == GateKind::Directory {
        for (rel, at) in tombstones(server_slot) {
            let item = format!("{key}/{rel}");
            let Some(current) = local.get(&item) else {
                continue;
            };
            let hash = deletion_hash(at);
            if store.is_approved(&item, &hash) || store.is_dismissed(&item, &hash) {
                continue;
            }
            out.push(PendingApproval {
                id: format!("{}:{item}{DELETE_ID_SUFFIX}", source.as_str()),
                key: key.to_string(),
                title: format!("Delete {}", title_for(key, &item, source)),
                diff: line_diff(Some(current), b""),
                new_item: false,
                hash,
                item,
                source,
            });
        }
    }
    out
}

/// Appended to the id of a pending deletion, so a file can have a pending
/// change and a pending deletion at once.
pub const DELETE_ID_SUFFIX: &str = "#delete";

/// The approvable hash of a deletion of a gated file made at `at` (ms): one
/// per delete, so approving one never approves a later one.
pub fn deletion_hash(at: i64) -> String {
    sha256_hex(format!("mnemos-sync-delete:{at}").as_bytes())
}

/// The deletions (`relative path -> deleted at, ms`) a directory slot
/// carries.
pub fn tombstones(slot: &Value) -> BTreeMap<String, i64> {
    slot.get(DIR_TOMBSTONES_KEY)
        .and_then(Value::as_object)
        .map(|t| {
            t.iter()
                .filter_map(|(rel, at)| Some((rel.clone(), at.as_i64()?)))
                .collect()
        })
        .unwrap_or_default()
}

/// Whether this machine lets the deletion of `item` made at `at` remove its
/// copy: approved here, or (tests only) every gate trusted.
pub fn deletion_allowed(store: &GateStore, item: &str, at: i64, trust_all: bool) -> bool {
    trust_all || store.is_approved(item, &deletion_hash(at))
}

fn drop_tombstone(slot: &mut Value, rel: &str) {
    let Some(map) = slot.as_object_mut() else {
        return;
    };
    let now_empty = match map.get_mut(DIR_TOMBSTONES_KEY).and_then(Value::as_object_mut) {
        Some(tombs) => {
            tombs.remove(rel);
            tombs.is_empty()
        }
        None => return,
    };
    if now_empty {
        map.remove(DIR_TOMBSTONES_KEY);
    }
}

/// `write_slot` (what a pull is about to write here) with every item that
/// would change this machine without an approval put back to this
/// machine's copy (or left out, when there is none). Returns the filtered
/// slot and the items that were held back.
pub fn filter_incoming(
    kind: GateKind,
    key: &str,
    write_slot: Value,
    local_slot: &Value,
    store: &GateStore,
    trust_all: bool,
) -> (Value, Vec<String>) {
    let local = items(kind, key, local_slot);
    let mut slot = write_slot;
    let mut held = Vec::new();
    for (item, bytes) in items(kind, key, &slot) {
        let current = local.get(&item);
        if current == Some(&bytes) {
            continue;
        }
        if trust_all || store.is_approved(&item, &sha256_hex(&bytes)) {
            continue;
        }
        set_item(kind, key, &mut slot, &item, current.map(Vec::as_slice));
        held.push(item);
    }
    if kind == GateKind::Directory && !trust_all {
        // A gated file is removed here only by a deletion approved here: an
        // unapproved tombstone is dropped, and a file the incoming copy
        // merely lacks (a mirror pull would set it aside) is kept.
        let tombs = tombstones(&slot);
        for (rel, at) in &tombs {
            let item = format!("{key}/{rel}");
            let Some(current) = local.get(&item) else {
                continue;
            };
            if deletion_allowed(store, &item, *at, false) {
                continue;
            }
            drop_tombstone(&mut slot, rel);
            set_item(kind, key, &mut slot, &item, Some(current));
            held.push(item);
        }
        let present = items(kind, key, &slot);
        let still_deleted = tombstones(&slot);
        for (item, bytes) in &local {
            let rel = item
                .strip_prefix(key)
                .and_then(|r| r.strip_prefix('/'))
                .unwrap_or_default();
            if present.contains_key(item) || still_deleted.contains_key(rel) {
                continue;
            }
            set_item(kind, key, &mut slot, item, Some(bytes));
        }
    }
    (slot, held)
}

/// Name segments (split on `_`) of a `settings.json` `env` variable that
/// mark it as a secret: `ANTHROPIC_API_KEY`, `GITHUB_TOKEN`,
/// `AWS_SECRET_ACCESS_KEY`, `DB_PASSWORD`. Whole segments, so
/// `CLAUDE_CODE_MAX_OUTPUT_TOKENS` is not one.
const SECRET_ENV_SEGMENTS: &[&str] = &[
    "KEY", "APIKEY", "TOKEN", "SECRET", "PASSWORD", "PASSWD", "PASS", "PWD", "CREDENTIAL",
    "CREDENTIALS", "AUTH", "COOKIE", "PAT",
];

/// Whether a `settings.json` `env` variable named `name` holds a secret.
pub fn is_secret_env_name(name: &str) -> bool {
    name.to_ascii_uppercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|seg| SECRET_ENV_SEGMENTS.contains(&seg))
}

/// `slot` (a `settings.json` value) with its secret `env` entries
/// ([`is_secret_env_name`]) replaced by those of `keep_from`: none when
/// `keep_from` has none. Secret env values never sync (gates review finding
/// 5, like the `env` and `headers` of MCP servers): a push carries the
/// server's own values for those names (`keep_from` = the server's copy),
/// so an older build that still syncs them loses nothing but this machine
/// adds none, and a pull keeps this machine's values (`keep_from` = the
/// local copy).
pub fn hold_secret_env(slot: Value, keep_from: Option<&Value>) -> Value {
    let Value::Object(mut obj) = slot else {
        return slot;
    };
    let kept: Map<String, Value> = keep_from
        .and_then(|v| v.get("env"))
        .and_then(Value::as_object)
        .map(|env| {
            env.iter()
                .filter(|(k, _)| is_secret_env_name(k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default();
    let mut env = match obj.remove("env") {
        Some(Value::Object(env)) => env,
        Some(other) => {
            obj.insert("env".to_string(), other);
            return Value::Object(obj);
        }
        None if kept.is_empty() => return Value::Object(obj),
        None => Map::new(),
    };
    env.retain(|k, _| !is_secret_env_name(k));
    env.extend(kept);
    obj.insert("env".to_string(), Value::Object(env));
    Value::Object(obj)
}

/// `write_slot` of a settings key with this machine's commands kept
/// whatever the incoming value holds (hooks sync off).
pub fn keep_local_commands(write_slot: Value, local_slot: &Value) -> Value {
    with_commands(write_slot, &commands_subset(local_slot))
}

/// `local_slot` (what a push is about to offer) with every item pending
/// approval here taken from the server's copy (`remote_slot`), so the push
/// never undoes a change this machine has not accepted. With `freeze`
/// (hooks sync off, settings keys only), the command keys are the server's
/// whatever is pending, and none when the server has no copy.
pub fn protect_outgoing(
    kind: GateKind,
    key: &str,
    local_slot: Value,
    remote_slot: Option<&Value>,
    store: &GateStore,
    freeze: bool,
) -> Value {
    let mut slot = local_slot;
    if freeze && kind == GateKind::SettingsCommands {
        let subset = remote_slot.map(commands_subset).unwrap_or_default();
        return with_commands(slot, &subset);
    }
    if kind == GateKind::SettingsCommands {
        slot = hold_secret_env(slot, remote_slot);
    }
    let Some(remote_slot) = remote_slot else {
        return slot;
    };
    let remote = items(kind, key, remote_slot);
    for p in store.pending.values() {
        if p.key != key || p.source != GateSource::Personal {
            continue;
        }
        let Some(bytes) = remote.get(&p.item) else {
            continue;
        };
        if sha256_hex(bytes) == p.hash {
            set_item(kind, key, &mut slot, &p.item, Some(bytes));
        }
    }
    slot
}

/// A short line diff from `old` to `new`: `- ` and `+ ` lines only,
/// truncated. Binary content is described, not shown.
pub fn line_diff(old: Option<&[u8]>, new: &[u8]) -> String {
    let (Ok(new_text), old_text) = (
        std::str::from_utf8(new),
        old.map(std::str::from_utf8).transpose(),
    ) else {
        return format!("binary content, {} bytes", new.len());
    };
    let Ok(old_text) = old_text else {
        return format!("binary content, {} bytes", new.len());
    };
    let new_lines: Vec<&str> = new_text.lines().collect();
    let old_lines: Vec<&str> = old_text.map(|t| t.lines().collect()).unwrap_or_default();
    let mut out: Vec<String> = Vec::new();
    if old_lines.len().saturating_mul(new_lines.len()) > MAX_DIFF_CELLS {
        out.push(format!(
            "(too large to compare; {} new lines)",
            new_lines.len()
        ));
        out.extend(new_lines.iter().map(|l| format!("+ {l}")));
    } else {
        // Longest common subsequence, then walk it.
        let (n, m) = (old_lines.len(), new_lines.len());
        let mut lcs = vec![vec![0u32; m + 1]; n + 1];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                lcs[i][j] = if old_lines[i] == new_lines[j] {
                    lcs[i + 1][j + 1] + 1
                } else {
                    lcs[i + 1][j].max(lcs[i][j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n || j < m {
            if i < n && j < m && old_lines[i] == new_lines[j] {
                i += 1;
                j += 1;
            } else if j < m && (i == n || lcs[i][j + 1] >= lcs[i + 1][j]) {
                out.push(format!("+ {}", new_lines[j]));
                j += 1;
            } else {
                out.push(format!("- {}", old_lines[i]));
                i += 1;
            }
        }
    }
    let total = out.len();
    let mut text = String::new();
    for (shown, line) in out.iter().enumerate() {
        if shown >= MAX_DIFF_LINES || text.len() + line.len() > MAX_DIFF_CHARS {
            text.push_str(&format!("... {} more line(s)\n", total - shown));
            break;
        }
        text.push_str(line);
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn b64(s: &str) -> Value {
        Value::String(B64.encode(s))
    }

    #[test]
    fn unapproved_hook_changes_are_held_and_listed() {
        let store = GateStore::default();
        let local = json!({"a.sh": b64("echo a"), "same.sh": b64("x")});
        let server = json!({
            "a.sh": b64("echo evil"),
            "new.sh": b64("echo new"),
            "same.sh": b64("x"),
            "__deleted": {"gone.sh": 1}
        });
        let pending = pending_items(
            GateKind::Directory,
            HOOKS_KEY,
            &server,
            &local,
            &store,
            GateSource::Personal,
        );
        let ids: Vec<&str> = pending.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["personal:claude-hooks/a.sh", "personal:claude-hooks/new.sh"]
        );
        assert!(pending[1].new_item);
        assert!(pending[0].diff.contains("- echo a"));
        assert!(pending[0].diff.contains("+ echo evil"));

        let (written, held) = filter_incoming(
            GateKind::Directory,
            HOOKS_KEY,
            server,
            &local,
            &store,
            false,
        );
        assert_eq!(held.len(), 2);
        assert_eq!(written["a.sh"], b64("echo a"));
        assert!(written.get("new.sh").is_none());
        // A deletion of a file this machine does not have changes nothing
        // here, so it passes.
        assert!(written.get("__deleted").is_some());
    }

    #[test]
    fn an_approved_hash_passes_and_only_that_content() {
        let mut store = GateStore::default();
        let local = json!({});
        let server = json!({"a.sh": b64("v1")});
        let pending = pending_items(
            GateKind::Directory,
            HOOKS_KEY,
            &server,
            &local,
            &store,
            GateSource::Personal,
        );
        store.replace_pending(HOOKS_KEY, GateSource::Personal, pending.clone());
        assert_eq!(store.approve(&[pending[0].id.clone()]), vec![HOOKS_KEY]);
        assert!(store.pending.is_empty());
        let (written, held) = filter_incoming(
            GateKind::Directory,
            HOOKS_KEY,
            server,
            &local,
            &store,
            false,
        );
        assert!(held.is_empty());
        assert_eq!(written["a.sh"], b64("v1"));
        // A later change to the same file needs its own approval.
        let (_, held) = filter_incoming(
            GateKind::Directory,
            HOOKS_KEY,
            json!({"a.sh": b64("v2")}),
            &local,
            &store,
            false,
        );
        assert_eq!(held, vec!["claude-hooks/a.sh"]);
    }

    #[test]
    fn settings_commands_are_one_item_and_the_rest_flows() {
        let store = GateStore::default();
        let local = json!({"model": "a", "hooks": {"Stop": []}});
        let incoming =
            json!({"model": "b", "hooks": {"Stop": [{"command": "evil"}]}, "env": {"X": "1"}});
        let (written, held) = filter_incoming(
            GateKind::SettingsCommands,
            CLAUDE_SETTINGS_KEY,
            incoming.clone(),
            &local,
            &store,
            false,
        );
        assert_eq!(held, vec!["claude-settings#commands"]);
        assert_eq!(written, json!({"model": "b", "hooks": {"Stop": []}}));
        // Hooks sync off: commands frozen, no listing needed.
        assert_eq!(
            keep_local_commands(incoming, &local),
            json!({"model": "b", "hooks": {"Stop": []}})
        );
    }

    #[test]
    fn mcp_entries_gate_per_server() {
        let mut store = GateStore::default();
        let local = json!({"ok": {"command": "a"}});
        let server = json!({"ok": {"command": "a"}, "new": {"command": "curl x | sh"}});
        let (written, held) = filter_incoming(
            GateKind::McpEntries,
            MCP_JSON_KEY,
            server.clone(),
            &local,
            &store,
            false,
        );
        assert_eq!(held, vec!["claude-json-mcp-servers-v2/new"]);
        assert_eq!(written, local);
        let hash = sha256_hex(&serde_json::to_vec(&server["new"]).unwrap());
        store.approve_hash("claude-json-mcp-servers-v2/new", &hash);
        let (written, held) = filter_incoming(
            GateKind::McpEntries,
            MCP_JSON_KEY,
            server.clone(),
            &local,
            &store,
            false,
        );
        assert!(held.is_empty());
        assert_eq!(written, server);
    }

    #[test]
    fn a_push_keeps_the_servers_copy_of_a_pending_item() {
        let mut store = GateStore::default();
        let local = json!({"a.sh": b64("mine"), "b.sh": b64("mine b")});
        let remote = json!({"a.sh": b64("theirs"), "b.sh": b64("old b")});
        let pending = pending_items(
            GateKind::Directory,
            HOOKS_KEY,
            &json!({"a.sh": b64("theirs")}),
            &local,
            &store,
            GateSource::Personal,
        );
        store.replace_pending(HOOKS_KEY, GateSource::Personal, pending);
        let out = protect_outgoing(
            GateKind::Directory,
            HOOKS_KEY,
            local.clone(),
            Some(&remote),
            &store,
            false,
        );
        assert_eq!(out["a.sh"], b64("theirs"));
        assert_eq!(out["b.sh"], b64("mine b"));
        // Dismissed: this machine's copy goes up.
        store.dismiss(&["personal:claude-hooks/a.sh".to_string()]);
        let out = protect_outgoing(
            GateKind::Directory,
            HOOKS_KEY,
            local,
            Some(&remote),
            &store,
            false,
        );
        assert_eq!(out["a.sh"], b64("mine"));
    }

    #[test]
    fn secret_env_names_are_whole_segments() {
        for name in [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "GITHUB_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
            "DB_PASSWORD",
            "openai_apikey",
        ] {
            assert!(is_secret_env_name(name), "{name}");
        }
        for name in ["CLAUDE_CODE_MAX_OUTPUT_TOKENS", "DISABLE_TELEMETRY", "BASH_DEFAULT_TIMEOUT_MS"] {
            assert!(!is_secret_env_name(name), "{name}");
        }
    }

    /// Gates review finding 5: with hooks sync on, a push never carries this
    /// machine's secret env values; the server's stay for older builds.
    #[test]
    fn a_push_never_carries_local_secret_env() {
        let store = GateStore::default();
        let local = json!({"env": {"ANTHROPIC_API_KEY": "mine", "DISABLE_TELEMETRY": "1"}});
        let remote = json!({"env": {"ANTHROPIC_API_KEY": "old", "GITHUB_TOKEN": "theirs"}});
        let out = protect_outgoing(
            GateKind::SettingsCommands,
            CLAUDE_SETTINGS_KEY,
            local.clone(),
            Some(&remote),
            &store,
            false,
        );
        assert_eq!(
            out,
            json!({"env": {"ANTHROPIC_API_KEY": "old", "GITHUB_TOKEN": "theirs", "DISABLE_TELEMETRY": "1"}})
        );
        let out = protect_outgoing(
            GateKind::SettingsCommands,
            CLAUDE_SETTINGS_KEY,
            local,
            None,
            &store,
            false,
        );
        assert_eq!(out, json!({"env": {"DISABLE_TELEMETRY": "1"}}));
        // A pull keeps this machine's secret values and takes the rest.
        let incoming = json!({"env": {"ANTHROPIC_API_KEY": "old", "DISABLE_TELEMETRY": "0"}});
        let local = json!({"env": {"ANTHROPIC_API_KEY": "mine"}});
        assert_eq!(
            hold_secret_env(incoming, Some(&local)),
            json!({"env": {"DISABLE_TELEMETRY": "0", "ANTHROPIC_API_KEY": "mine"}})
        );
        // No env anywhere: unchanged.
        assert_eq!(hold_secret_env(json!({"model": "a"}), None), json!({"model": "a"}));
    }

    #[test]
    fn frozen_settings_push_the_servers_commands_or_none() {
        let store = GateStore::default();
        let local = json!({"model": "a", "hooks": {"mine": 1}});
        let remote = json!({"model": "z", "hooks": {"theirs": 1}});
        assert_eq!(
            protect_outgoing(
                GateKind::SettingsCommands,
                CLAUDE_SETTINGS_KEY,
                local.clone(),
                Some(&remote),
                &store,
                true
            ),
            json!({"model": "a", "hooks": {"theirs": 1}})
        );
        assert_eq!(
            protect_outgoing(
                GateKind::SettingsCommands,
                CLAUDE_SETTINGS_KEY,
                local,
                None,
                &store,
                true
            ),
            json!({"model": "a"})
        );
    }

    #[test]
    fn store_round_trips_and_defaults_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(GATES_FILE_NAME);
        assert_eq!(GateStore::load(&path), GateStore::default());
        let mut store = GateStore {
            hooks_sync: true,
            ..Default::default()
        };
        for i in 0..20 {
            store.approve_hash("x", &format!("h{i}"));
        }
        assert_eq!(store.approved["x"].len(), MAX_HASHES_PER_ITEM);
        assert!(store.is_approved("x", "h19"));
        assert!(!store.is_approved("x", "h0"));
        store.save(&path).unwrap();
        assert_eq!(GateStore::load(&path), store);
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(GateStore::load(&path), GateStore::default());
    }

    #[test]
    fn replace_pending_only_touches_its_key_and_source() {
        let mut store = GateStore::default();
        let p = |key: &str, source| PendingApproval {
            id: format!("{}:{key}/x", GateSource::as_str(source)),
            key: key.to_string(),
            item: format!("{key}/x"),
            source,
            title: String::new(),
            hash: "h".into(),
            diff: String::new(),
            new_item: true,
        };
        store.replace_pending(
            HOOKS_KEY,
            GateSource::Personal,
            vec![p(HOOKS_KEY, GateSource::Personal)],
        );
        store.replace_pending(
            HOOKS_KEY,
            GateSource::Team,
            vec![p(HOOKS_KEY, GateSource::Team)],
        );
        store.replace_pending(
            MCP_DIR_KEY,
            GateSource::Personal,
            vec![p(MCP_DIR_KEY, GateSource::Personal)],
        );
        assert!(store.replace_pending(HOOKS_KEY, GateSource::Personal, vec![]));
        assert_eq!(store.pending.len(), 2);
        assert!(!store.replace_pending(HOOKS_KEY, GateSource::Personal, vec![]));
    }

    #[test]
    fn diff_marks_changed_lines_and_truncates() {
        let d = line_diff(Some(b"a\nb\nc\n"), b"a\nB\nc\n");
        assert_eq!(d, "+ B\n- b\n");
        let big: String = (0..500).map(|i| format!("line {i}\n")).collect();
        let d = line_diff(None, big.as_bytes());
        assert!(d.contains("more line(s)"));
        assert_eq!(line_diff(None, &[0xff, 0xfe]), "binary content, 2 bytes");
    }

    #[test]
    fn agents_commands_and_skills_are_gated_without_the_opt_in() {
        for key in [AGENTS_KEY, COMMANDS_KEY, SKILLS_KEY] {
            assert_eq!(gate_kind(key), Some(GateKind::Directory), "{key}");
            assert!(GATED_KEYS.contains(&key), "{key}");
            // They always sync; only their changes are gated.
            assert!(!needs_hooks_opt_in(key), "{key}");
            assert!(!skipped_without_hooks_opt_in(key), "{key}");
        }
        let store = GateStore::default();
        let local = json!({"a.md": b64("old")});
        let server = json!({"a.md": b64("---\nallowed-tools: Bash\n---\n")});
        for (key, title) in [
            (AGENTS_KEY, "Agent ~/.claude/agents/a.md"),
            (COMMANDS_KEY, "Command ~/.claude/commands/a.md"),
            (SKILLS_KEY, "Skill file ~/.claude/skills/a.md"),
        ] {
            let pending = pending_items(
                GateKind::Directory,
                key,
                &server,
                &local,
                &store,
                GateSource::Personal,
            );
            assert_eq!(pending.len(), 1, "{key}");
            assert!(pending[0].title.contains(title), "{}", pending[0].title);
            let (written, held) = filter_incoming(
                GateKind::Directory,
                key,
                server.clone(),
                &local,
                &store,
                false,
            );
            assert_eq!(held, vec![format!("{key}/a.md")]);
            assert_eq!(written["a.md"], b64("old"));
        }
    }

    #[test]
    fn a_gated_deletion_waits_for_approval() {
        let mut store = GateStore::default();
        let local = json!({"a.md": b64("keep me"), "b.md": b64("b")});
        let server = json!({"b.md": b64("b"), "__deleted": {"a.md": 5}});

        let pending = pending_items(
            GateKind::Directory,
            AGENTS_KEY,
            &server,
            &local,
            &store,
            GateSource::Personal,
        );
        assert_eq!(pending.len(), 1);
        let p = &pending[0];
        assert_eq!(p.id, format!("personal:claude-agents/a.md{DELETE_ID_SUFFIX}"));
        assert_eq!(p.item, "claude-agents/a.md");
        assert_eq!(p.hash, deletion_hash(5));
        assert!(p.title.starts_with("Delete "));
        assert!(p.diff.contains("- keep me"));
        assert!(!p.new_item);

        // Unapproved: the file stays and the tombstone is not written here.
        let (written, held) = filter_incoming(
            GateKind::Directory,
            AGENTS_KEY,
            server.clone(),
            &local,
            &store,
            false,
        );
        assert_eq!(held, vec!["claude-agents/a.md"]);
        assert_eq!(written["a.md"], b64("keep me"));
        assert!(written.get("__deleted").is_none());
        assert!(!deletion_allowed(&store, &p.item, 5, false));
        assert!(deletion_allowed(&store, &p.item, 5, true));

        // Approved: the deletion goes through, and only that one.
        store.replace_pending(AGENTS_KEY, GateSource::Personal, pending.clone());
        assert_eq!(store.approve(std::slice::from_ref(&p.id)), vec![AGENTS_KEY]);
        assert!(deletion_allowed(&store, &p.item, 5, false));
        let (written, held) = filter_incoming(
            GateKind::Directory,
            AGENTS_KEY,
            server,
            &local,
            &store,
            false,
        );
        assert!(held.is_empty());
        assert!(written.get("a.md").is_none());
        assert_eq!(written["__deleted"]["a.md"], json!(5));
        assert!(pending_items(
            GateKind::Directory,
            AGENTS_KEY,
            &json!({"__deleted": {"a.md": 5}}),
            &local,
            &store,
            GateSource::Personal,
        )
        .is_empty());

        // A later deletion of the same file needs its own approval.
        let (written, held) = filter_incoming(
            GateKind::Directory,
            AGENTS_KEY,
            json!({"__deleted": {"a.md": 9}}),
            &local,
            &store,
            false,
        );
        assert_eq!(held, vec!["claude-agents/a.md"]);
        assert_eq!(written["a.md"], b64("keep me"));
    }

    #[test]
    fn a_dismissed_deletion_is_not_listed_and_not_applied() {
        let mut store = GateStore::default();
        let local = json!({"a.sh": b64("echo a")});
        let server = json!({"__deleted": {"a.sh": 7}});
        let pending = pending_items(
            GateKind::Directory,
            HOOKS_KEY,
            &server,
            &local,
            &store,
            GateSource::Team,
        );
        assert_eq!(pending.len(), 1);
        store.replace_pending(HOOKS_KEY, GateSource::Team, pending.clone());
        assert_eq!(store.dismiss(&[pending[0].id.clone()]), vec![HOOKS_KEY]);
        assert!(pending_items(
            GateKind::Directory,
            HOOKS_KEY,
            &server,
            &local,
            &store,
            GateSource::Team,
        )
        .is_empty());
        let (written, held) =
            filter_incoming(GateKind::Directory, HOOKS_KEY, server, &local, &store, false);
        assert_eq!(held, vec!["claude-hooks/a.sh"]);
        assert_eq!(written["a.sh"], b64("echo a"));
        assert!(written.get("__deleted").is_none());
    }

    #[test]
    fn a_gated_file_the_incoming_copy_lacks_is_kept() {
        let store = GateStore::default();
        let local = json!({"a.md": b64("a"), "mine.md": b64("only here")});
        let server = json!({"a.md": b64("a")});
        let (written, held) = filter_incoming(
            GateKind::Directory,
            SKILLS_KEY,
            server.clone(),
            &local,
            &store,
            false,
        );
        assert!(held.is_empty());
        assert_eq!(written["mine.md"], b64("only here"));
        // Trusting every gate (tests only) leaves the incoming copy as is.
        let (written, _) =
            filter_incoming(GateKind::Directory, SKILLS_KEY, server, &local, &store, true);
        assert!(written.get("mine.md").is_none());
    }
}

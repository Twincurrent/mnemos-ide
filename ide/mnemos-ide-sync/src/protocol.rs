// SPDX-License-Identifier: AGPL-3.0-only
//! Sync wire protocol: the version header every sync request carries, the
//! server's "too old" answer, and the `{schema, data}` value wrapper for
//! keys added from protocol 1 on.
//!
//! # Version header
//!
//! Every request this crate's [`SyncClient`](crate::SyncClient) sends carries
//! `X-Mnemos-Sync-Protocol: <SYNC_PROTOCOL_VERSION>`. A server may refuse a
//! version below its minimum with `426 Upgrade Required` and a JSON body
//! `{"error", "client_protocol", "min_protocol", "server_protocol"}`. The
//! client then stops syncing and the UI says "update Mnemos to keep
//! syncing" ([`crate::SyncAuthState::UpdateRequired`]). A 426 without that
//! body (a proxy's page, say) is treated as any other HTTP error.
//!
//! Builds from before the header send none, which the server counts as
//! protocol 0. Its default minimum is 0, so nothing is refused until an
//! operator raises it.
//!
//! # Value wrapper (new keys only)
//!
//! Values of keys that existed before protocol 1 stay exactly as they are,
//! including their in-band sentinels (`__deleted`, `__deleted_files`,
//! `__deleted_at`, `__revived`): older builds read and write those keys and
//! cannot read anything else. A key introduced from protocol 1 on stores
//! [`Envelope`]:
//!
//! ```json
//! {"schema": 1, "data": <payload>, "tombstones": {"<item>": <ms>}, "deleted_at": <ms>}
//! ```
//!
//! `schema` is the payload's format version for that key (starting at 1),
//! `tombstones` names deleted items of a map-shaped payload with the time
//! they were deleted (Unix ms), and `deleted_at` marks the whole key as
//! deleted. The last two are optional and omitted when empty. Readers use
//! [`decode_value`], which accepts both an envelope and a bare (legacy)
//! value, the latter as schema 0. A reader must leave a value whose schema
//! it does not know alone: never apply it, never overwrite it.

use std::collections::BTreeMap;

use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Request header carrying [`SYNC_PROTOCOL_VERSION`].
pub const SYNC_PROTOCOL_HEADER: &str = "x-mnemos-sync-protocol";
/// The sync protocol this build speaks. Bump it when a change to the wire
/// format means a server must be able to tell this build from older ones.
pub const SYNC_PROTOCOL_VERSION: u32 = 1;
/// The highest envelope `schema` any key of this build writes or reads.
/// Keys may use a lower one; see [`decode_value`].
pub const MAX_KNOWN_SCHEMA: u64 = 1;

/// Longest server message kept for the UI.
const MAX_REASON_CHARS: usize = 160;

/// The headers [`crate::SyncClient`] puts on every request.
pub fn default_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        SYNC_PROTOCOL_HEADER,
        HeaderValue::from(SYNC_PROTOCOL_VERSION),
    );
    headers
}

/// What a 426 from the sync server says, or `None` when the response is not
/// the sync server's own protocol refusal.
///
/// Only a 426 whose body carries `min_protocol` counts: a 426 in any other
/// shape came from something in between and is left to the normal error
/// path.
pub fn upgrade_required_reason(status: StatusCode, body: &str) -> Option<String> {
    if status != StatusCode::UPGRADE_REQUIRED {
        return None;
    }
    let parsed: Value = serde_json::from_str(body).ok()?;
    let min = parsed.get("min_protocol")?.as_u64()?;
    Some(update_message(SYNC_PROTOCOL_VERSION, min))
}

/// The sentence the UI shows for a refused protocol.
pub fn update_message(client: u32, min: u64) -> String {
    let text = format!(
        "The sync server needs sync protocol {min} or newer and this Mnemos speaks {client}. \
         Update Mnemos to keep syncing."
    );
    text.chars().take(MAX_REASON_CHARS).collect()
}

/// Whether a client at `client` may talk to a server whose minimum is `min`
/// (`None`: a server that does not advertise one, which accepts everyone).
pub fn protocol_accepted(client: u32, min: Option<u64>) -> bool {
    min.is_none_or(|min| u64::from(client) >= min)
}

/// The wrapper stored as the value of a key introduced from protocol 1 on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub schema: u64,
    pub data: Value,
    /// Deleted items of a map-shaped `data`: item id -> deleted at (Unix ms).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tombstones: BTreeMap<String, i64>,
    /// The whole key was deleted at this time (Unix ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<i64>,
}

impl Envelope {
    pub fn new(schema: u64, data: Value) -> Self {
        Self {
            schema,
            data,
            tombstones: BTreeMap::new(),
            deleted_at: None,
        }
    }

    pub fn to_value(&self) -> Value {
        // Serializing a struct of JSON values cannot fail.
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// Key prefixes whose values are stored as an [`Envelope`]. Only keys
/// introduced from protocol 1 on may be listed here; a key an older build
/// reads must never be moved to the wrapper. Empty today: no key of this
/// build is wrapped yet. A new key registers here and writes with
/// [`encode_value`]; the pull path then reads it with [`decode_value`].
pub const ENVELOPE_KEY_PREFIXES: &[&str] = &[];

/// Whether `key` stores its value as an [`Envelope`]. A key that does not is
/// a legacy key, and its value is read as it is even when it looks like an
/// envelope.
pub fn uses_envelope(key: &str) -> bool {
    ENVELOPE_KEY_PREFIXES.iter().any(|p| key.starts_with(p))
}

/// A pulled value, read in either form.
#[derive(Debug, Clone, PartialEq)]
pub enum Decoded {
    /// A bare value, as every key before protocol 1 stores it.
    Legacy(Value),
    /// An envelope whose schema this build knows.
    Known(Envelope),
    /// An envelope from a newer build. Leave it alone.
    UnknownSchema(u64),
}

/// The field names an envelope may carry. Anything else means the value is
/// not an envelope (a legacy value that happens to have a `schema` field).
const ENVELOPE_FIELDS: [&str; 4] = ["schema", "data", "tombstones", "deleted_at"];

/// Whether `value` is an envelope: an object with an integer `schema` of at
/// least 1, a `data` field, and no fields but the four envelope fields.
/// Strict on purpose, so no legacy value is ever mistaken for one.
pub fn is_envelope(value: &Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    obj.get("schema")
        .and_then(Value::as_u64)
        .is_some_and(|s| s >= 1)
        && obj.contains_key("data")
        && obj.keys().all(|k| ENVELOPE_FIELDS.contains(&k.as_str()))
}

/// Read a pulled value in either form. `max_schema` is the highest schema
/// the caller's key understands.
pub fn decode_value(value: &Value, max_schema: u64) -> Decoded {
    let obj: &Map<String, Value> = match value.as_object() {
        Some(obj) if is_envelope(value) => obj,
        _ => return Decoded::Legacy(value.clone()),
    };
    let schema = obj.get("schema").and_then(Value::as_u64).unwrap_or(0);
    if schema > max_schema {
        return Decoded::UnknownSchema(schema);
    }
    match serde_json::from_value::<Envelope>(value.clone()) {
        Ok(env) => Decoded::Known(env),
        // Envelope fields of the wrong type: treat as a newer format rather
        // than guess.
        Err(_) => Decoded::UnknownSchema(schema),
    }
}

/// Wrap `data` for a key introduced from protocol 1 on.
pub fn encode_value(schema: u64, data: Value) -> Value {
    Envelope::new(schema, data).to_value()
}

/// Merge two envelopes' tombstones: the later deletion time wins per item.
pub fn merge_tombstones(
    a: &BTreeMap<String, i64>,
    b: &BTreeMap<String, i64>,
) -> BTreeMap<String, i64> {
    let mut out = a.clone();
    for (id, at) in b {
        out.entry(id.clone())
            .and_modify(|cur| *cur = (*cur).max(*at))
            .or_insert(*at);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn header_carries_the_version() {
        let h = default_headers();
        assert_eq!(
            h.get(SYNC_PROTOCOL_HEADER).and_then(|v| v.to_str().ok()),
            Some(SYNC_PROTOCOL_VERSION.to_string().as_str())
        );
    }

    #[test]
    fn negotiation() {
        assert!(protocol_accepted(1, None));
        assert!(protocol_accepted(1, Some(0)));
        assert!(protocol_accepted(1, Some(1)));
        assert!(!protocol_accepted(1, Some(2)));
        // Builds without the header count as 0 on the server.
        assert!(protocol_accepted(0, Some(0)));
        assert!(!protocol_accepted(0, Some(1)));
    }

    #[test]
    fn only_the_servers_own_426_counts() {
        let body = r#"{"error":"x","client_protocol":1,"min_protocol":2,"server_protocol":2}"#;
        let reason = upgrade_required_reason(StatusCode::UPGRADE_REQUIRED, body).unwrap();
        assert!(reason.contains("Update Mnemos"), "{reason}");
        assert!(reason.contains('2'), "{reason}");
        assert!(reason.is_ascii());
        assert_eq!(
            upgrade_required_reason(StatusCode::UPGRADE_REQUIRED, "<html>"),
            None
        );
        assert_eq!(
            upgrade_required_reason(StatusCode::UPGRADE_REQUIRED, r#"{"error":"x"}"#),
            None
        );
        assert_eq!(upgrade_required_reason(StatusCode::BAD_REQUEST, body), None);
    }

    #[test]
    fn envelope_round_trip() {
        let mut env = Envelope::new(1, json!({"a": 1}));
        env.tombstones.insert("b".into(), 42);
        let v = env.to_value();
        assert_eq!(
            v,
            json!({"schema": 1, "data": {"a": 1}, "tombstones": {"b": 42}})
        );
        assert_eq!(decode_value(&v, MAX_KNOWN_SCHEMA), Decoded::Known(env));

        let bare = encode_value(1, json!([1, 2]));
        assert_eq!(bare, json!({"schema": 1, "data": [1, 2]}));
        let deleted = json!({"schema": 1, "data": null, "deleted_at": 7});
        match decode_value(&deleted, 1) {
            Decoded::Known(e) => assert_eq!(e.deleted_at, Some(7)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn legacy_values_are_read_as_they_are() {
        for v in [
            json!({"projects": []}),
            json!({"files": {}, "__deleted": {"a": 1}}),
            json!("text"),
            json!(null),
            // A legacy object that has a schema field is still legacy.
            json!({"schema": 1, "data": 2, "other": 3}),
            json!({"schema": "1", "data": 2}),
            json!({"schema": 0, "data": 2}),
            json!({"schema": 1}),
        ] {
            assert_eq!(
                decode_value(&v, MAX_KNOWN_SCHEMA),
                Decoded::Legacy(v.clone()),
                "{v}"
            );
            assert!(!is_envelope(&v), "{v}");
        }
    }

    #[test]
    fn newer_schema_is_left_alone() {
        let v = json!({"schema": 3, "data": {}});
        assert_eq!(decode_value(&v, 1), Decoded::UnknownSchema(3));
        assert_eq!(
            decode_value(&v, 3),
            Decoded::Known(Envelope::new(3, json!({})))
        );
        let bad = json!({"schema": 1, "data": {}, "tombstones": "x"});
        assert_eq!(decode_value(&bad, 1), Decoded::UnknownSchema(1));
    }

    #[test]
    fn no_existing_key_is_wrapped() {
        for key in [
            "projects",
            "settings",
            "mnemos-user-settings",
            "claude-session-index",
            "claude-memory:p:o/r",
            "agents-live:mac",
            "claude-json-mcp-servers",
            "claude-json-mcp-servers-v2",
            "mnemos-user-settings-v2",
        ] {
            assert!(!uses_envelope(key), "{key}");
        }
    }

    #[test]
    fn tombstones_merge_to_the_later_time() {
        let a = BTreeMap::from([("x".to_string(), 5), ("y".to_string(), 1)]);
        let b = BTreeMap::from([("x".to_string(), 3), ("z".to_string(), 9)]);
        let m = merge_tombstones(&a, &b);
        assert_eq!(m.get("x"), Some(&5));
        assert_eq!(m.get("y"), Some(&1));
        assert_eq!(m.get("z"), Some(&9));
    }
}

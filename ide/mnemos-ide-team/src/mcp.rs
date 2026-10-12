// SPDX-License-Identifier: AGPL-3.0-only
//! The MCP (Model Context Protocol) messages of the team server: JSON-RPC 2.0
//! over streamable HTTP, answered with plain `application/json` (no SSE, no
//! session id). Parsing and the tool schemas live here; the app runs the
//! tools.

use serde_json::{json, Value};

/// Versions this server speaks, newest last. A client asking for one of them
/// gets it back; any other request gets the newest, which the client may
/// refuse (the MCP version negotiation).
pub const PROTOCOL_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];

pub const SERVER_NAME: &str = "mnemos-team";

/// One parsed request body.
#[derive(Debug, PartialEq)]
pub enum Incoming {
    Request { id: Value, method: String, params: Value },
    /// No id: nothing is sent back (HTTP 202).
    Notification { method: String },
    /// Could not be understood; carries the error response to send.
    Invalid(Value),
}

pub fn parse(body: &[u8]) -> Incoming {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return Incoming::Invalid(error(Value::Null, -32700, "parse error"));
    };
    if value.is_array() {
        return Incoming::Invalid(error(Value::Null, -32600, "batches are not supported"));
    }
    let Some(method) = value.get("method").and_then(Value::as_str) else {
        // A response or garbage; a client never sends this server responses.
        let id = value.get("id").cloned().unwrap_or(Value::Null);
        return Incoming::Invalid(error(id, -32600, "invalid request"));
    };
    match value.get("id") {
        None | Some(Value::Null) => Incoming::Notification {
            method: method.to_string(),
        },
        Some(id) => Incoming::Request {
            id: id.clone(),
            method: method.to_string(),
            params: value.get("params").cloned().unwrap_or(Value::Null),
        },
    }
}

pub fn result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

pub fn initialize_result(params: &Value, version: &str) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str);
    let chosen = asked
        .filter(|v| PROTOCOL_VERSIONS.contains(v))
        .unwrap_or(PROTOCOL_VERSIONS[PROTOCOL_VERSIONS.len() - 1]);
    json!({
        "protocolVersion": chosen,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": SERVER_NAME, "version": version },
        "instructions": "Start and manage Claude Code sessions in panes next to yours in this Mnemos project tab. \
            Only a session the user allowed (a coordinator) can spawn; spawned sessions cannot. \
            Message sessions with SendMessage, not with these tools. \
            Team rules and the status board are files (see the mnemos-agents plugin's team.sh)."
    })
}

/// The text content of a tool result.
pub fn tool_text(text: impl Into<String>, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text.into() }], "isError": is_error })
}

/// A tool call, validated for shape (the values are validated where used).
#[derive(Debug, Clone, PartialEq)]
pub enum ToolCall {
    Spawn {
        name: String,
        brief: String,
        model: Option<String>,
        cwd: Option<String>,
        worktree_branch: Option<String>,
        worktree_base: Option<String>,
    },
    List,
    Focus { name: String },
    Rename { name: String, new_name: String },
    Retire { name: String, keep_worktree: bool },
    ListWorktrees,
    PruneWorktrees { dry_run: bool },
    Lease { resource: String, ttl_seconds: u64 },
    Release { lease_id: String },
    ListLeases,
}

fn arg_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn required(args: &Value, key: &str) -> Result<String, String> {
    arg_str(args, key).ok_or_else(|| format!("missing argument: {key}"))
}

pub fn parse_tool_call(params: &Value) -> Result<ToolCall, String> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or("missing tool name")?;
    let empty = json!({});
    let args = params.get("arguments").unwrap_or(&empty);
    Ok(match name {
        "spawn_session" => ToolCall::Spawn {
            name: required(args, "name")?,
            // Not trimmed: the brief is passed on as written.
            brief: args
                .get("brief")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or("missing argument: brief")?,
            model: arg_str(args, "model"),
            cwd: arg_str(args, "cwd"),
            worktree_branch: arg_str(args, "worktree_branch"),
            worktree_base: arg_str(args, "worktree_base"),
        },
        "list_sessions" => ToolCall::List,
        "focus_session" => ToolCall::Focus {
            name: required(args, "name")?,
        },
        "rename_session" => ToolCall::Rename {
            name: required(args, "name")?,
            new_name: required(args, "new_name")?,
        },
        "retire_session" => ToolCall::Retire {
            name: required(args, "name")?,
            keep_worktree: args.get("keep_worktree").and_then(Value::as_bool).unwrap_or(false),
        },
        "list_worktrees" => ToolCall::ListWorktrees,
        "prune_worktrees" => ToolCall::PruneWorktrees {
            dry_run: args.get("dry_run").and_then(Value::as_bool).unwrap_or(true),
        },
        "lease" => ToolCall::Lease {
            resource: required(args, "resource")?,
            ttl_seconds: args.get("ttl_seconds").and_then(Value::as_u64).unwrap_or(600),
        },
        "release" => ToolCall::Release {
            lease_id: required(args, "lease_id")?,
        },
        "list_leases" => ToolCall::ListLeases,
        other => return Err(format!("unknown tool: {other}")),
    })
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required },
    })
}

pub fn tools_list() -> Value {
    let name = json!({ "type": "string", "description": "Session name: 1-32 of a-z, 0-9, '-'." });
    json!({ "tools": [
        tool("spawn_session",
            "Split a new pane next to yours in this project tab and start Claude Code there with `brief` as its first prompt. \
             Only a coordinator the user allowed may call this; spawned sessions cannot. At most 6 spawned sessions live and 3 spawns a minute. \
             With worktree_branch, the session gets its own git worktree on that branch (created from worktree_base or the default branch), \
             which retire_session removes again once it is clean and merged.",
            json!({
                "name": name,
                "brief": { "type": "string", "description": "First prompt for the new session. Say its role, its task, how to report back (SendMessage to you) and the board row to keep." },
                "model": { "type": "string", "description": "Claude model alias or id, e.g. sonnet or opus. Default: the user's default." },
                "cwd": { "type": "string", "description": "Absolute directory to start in. Default: your current directory. Ignored with worktree_branch." },
                "worktree_branch": { "type": "string", "description": "Create a git worktree on this branch for the session." },
                "worktree_base": { "type": "string", "description": "Branch or commit a new worktree branch starts from. Default: origin/HEAD, main or master." },
            }),
            &["name", "brief"]),
        tool("list_sessions",
            "List the Claude Code sessions in your project tab: name, status, cwd, worktree, model, session id, and whether you spawned it.",
            json!({}), &[]),
        tool("focus_session", "Bring a session's pane to the front so the user can talk to it directly.",
            json!({ "name": name }), &["name"]),
        tool("rename_session", "Rename a spawned session (its pane title and its name in list_sessions).",
            json!({ "name": name, "new_name": name }), &["name", "new_name"]),
        tool("retire_session",
            "Close a spawned session's pane and remove its worktree. Refuses while the worktree is dirty or its branch is not merged \
             (say keep_worktree to close the pane and keep the worktree). A session that is still working asks the user first.",
            json!({ "name": name, "keep_worktree": { "type": "boolean" } }), &["name"]),
        tool("list_worktrees",
            "Every git worktree of this repository with size, dirty state, merged state, unpushed commits and the session that created it.",
            json!({}), &[]),
        tool("prune_worktrees",
            "Remove every worktree that is clean and merged, delete its branch, and report what was kept and why. dry_run (default true) only reports.",
            json!({ "dry_run": { "type": "boolean" } }), &[]),
        tool("lease",
            "Take an exclusive lease on a shared resource before using it: cpu (one heavy build or test run at a time), chrome (the one browser), \
             or port (returns a free port number). Fails with who holds it; retry later. Leases expire after ttl_seconds and when your pane closes.",
            json!({ "resource": { "type": "string", "enum": ["cpu", "chrome", "port"] }, "ttl_seconds": { "type": "integer", "minimum": 10, "maximum": 7200 } }),
            &["resource"]),
        tool("release", "Release a lease you hold.", json!({ "lease_id": { "type": "string" } }), &["lease_id"]),
        tool("list_leases", "Show the current leases and who holds them.", json!({}), &[]),
    ]})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_requests_and_notifications() {
        let req = parse(br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#);
        assert_eq!(
            req,
            Incoming::Request { id: json!(1), method: "tools/list".into(), params: Value::Null }
        );
        assert_eq!(
            parse(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
            Incoming::Notification { method: "notifications/initialized".into() }
        );
        assert!(matches!(parse(b"nope"), Incoming::Invalid(_)));
        assert!(matches!(parse(b"[]"), Incoming::Invalid(_)));
    }

    #[test]
    fn negotiates_versions() {
        let r = initialize_result(&json!({ "protocolVersion": "2025-06-18" }), "1");
        assert_eq!(r["protocolVersion"], "2025-06-18");
        let r = initialize_result(&json!({ "protocolVersion": "2099-01-01" }), "1");
        assert_eq!(r["protocolVersion"], "2025-11-25");
    }

    #[test]
    fn parses_tool_calls() {
        let call = parse_tool_call(&json!({
            "name": "spawn_session",
            "arguments": { "name": "tester", "brief": "  hi  ", "model": " sonnet ", "worktree_branch": "" }
        }))
        .unwrap();
        assert_eq!(
            call,
            ToolCall::Spawn {
                name: "tester".into(),
                brief: "  hi  ".into(),
                model: Some("sonnet".into()),
                cwd: None,
                worktree_branch: None,
                worktree_base: None,
            }
        );
        assert_eq!(
            parse_tool_call(&json!({ "name": "prune_worktrees" })).unwrap(),
            ToolCall::PruneWorktrees { dry_run: true }
        );
        assert!(parse_tool_call(&json!({ "name": "focus_session" })).is_err());
        assert!(parse_tool_call(&json!({ "name": "rm_rf" })).is_err());
    }

    #[test]
    fn every_listed_tool_parses() {
        let list = tools_list();
        for tool in list["tools"].as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            let mut args = json!({});
            for key in tool["inputSchema"]["required"].as_array().unwrap() {
                args[key.as_str().unwrap()] = json!("x");
            }
            assert!(parse_tool_call(&json!({ "name": name, "arguments": args })).is_ok(), "{name}");
        }
    }
}

// SPDX-License-Identifier: AGPL-3.0-only
//! The last exchange of a Claude Code session: the developer's last prompt and
//! the agent's last reply, read from the tail of its transcript.
//!
//! Used by the agents panel's "Suggest" button, which needs the full last
//! reply (the live session state only carries a short preview).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::model::is_safe_session_id;

/// How much of the transcript's end is read. Transcripts reach hundreds of
/// MB; the last exchange is near the end.
const TAIL_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastExchange {
    /// The developer's last typed prompt, if one is in the tail.
    pub prompt: Option<String>,
    /// The agent's last text reply.
    pub reply: String,
}

/// Read the last exchange of session `session_id` under `projects_dir`
/// (`~/.claude/projects`). `None` when the transcript is missing or has no
/// assistant text yet.
pub fn last_exchange(projects_dir: &Path, session_id: &str) -> Option<LastExchange> {
    if !is_safe_session_id(session_id) {
        return None;
    }
    let path = find_transcript(projects_dir, session_id)?;
    let tail = read_tail(&path, TAIL_BYTES).ok()?;
    parse_last_exchange(&tail)
}

/// `<slug>/<session_id>.jsonl`, newest if several slugs hold one (a renamed
/// checkout leaves the old slug behind).
fn find_transcript(projects_dir: &Path, session_id: &str) -> Option<PathBuf> {
    let file_name = format!("{session_id}.jsonl");
    std::fs::read_dir(projects_dir)
        .ok()?
        .flatten()
        .map(|slug| slug.path().join(&file_name))
        .filter_map(|p| {
            let modified = std::fs::metadata(&p).ok()?.modified().unwrap_or(UNIX_EPOCH);
            Some((modified, p))
        })
        .max_by_key(|(modified, _): &(SystemTime, PathBuf)| *modified)
        .map(|(_, p)| p)
}

fn read_tail(path: &Path, max: u64) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(max);
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity((len - start) as usize);
    file.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    // A cut in the middle of a line leaves a partial first line; drop it.
    Ok(if start > 0 {
        text.split_once('\n')
            .map(|(_, rest)| rest.to_string())
            .unwrap_or_default()
    } else {
        text
    })
}

/// Walk the transcript lines from the end: the newest assistant text is the
/// reply, and the newest real user prompt before it is the prompt.
pub fn parse_last_exchange(jsonl: &str) -> Option<LastExchange> {
    let mut reply: Option<String> = None;
    for line in jsonl.lines().rev() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if entry.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        match entry.get("type").and_then(Value::as_str) {
            Some("assistant") if reply.is_none() && !is_synthetic(&entry) => {
                if let Some(text) = assistant_text(&entry) {
                    reply = Some(text);
                }
            }
            Some("user") if reply.is_some() => {
                if let Some(prompt) = user_prompt(&entry) {
                    return Some(LastExchange {
                        prompt: Some(prompt),
                        reply: reply.unwrap_or_default(),
                    });
                }
            }
            _ => {}
        }
    }
    reply.map(|reply| LastExchange {
        prompt: None,
        reply,
    })
}

/// A message the harness wrote in the agent's name ("No response requested.",
/// "API Error: ..."), not a reply from the model.
fn is_synthetic(entry: &Value) -> bool {
    entry.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true)
        || entry.pointer("/message/model").and_then(Value::as_str) == Some("<synthetic>")
}

/// The text blocks of an assistant message, joined; `None` for a message that
/// is only tool calls.
fn assistant_text(entry: &Value) -> Option<String> {
    let content = entry.pointer("/message/content")?;
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// A prompt the developer typed: not a tool result, not harness metadata, not
/// a compaction summary, not a peer message or notification.
///
/// Newer transcripts tag each user entry with `origin.kind`; only `human` is a
/// typed prompt, and its pasted blocks and slash commands are unwrapped. Older
/// entries carry no origin, so markup there is taken as harness output.
fn user_prompt(entry: &Value) -> Option<String> {
    let flag = |key: &str| entry.get(key).and_then(Value::as_bool) == Some(true);
    if flag("isMeta") || flag("isCompactSummary") || flag("isVisibleInTranscriptOnly") {
        return None;
    }
    let origin = entry.get("origin");
    if origin.is_some()
        && origin
            .and_then(|o| o.pointer("/kind"))
            .and_then(Value::as_str)
            != Some("human")
    {
        return None;
    }
    let content = entry.pointer("/message/content")?;
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => {
            if blocks
                .iter()
                .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
            {
                return None;
            }
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        }
        _ => return None,
    };
    let text = if origin.is_some() {
        match slash_command(&text) {
            Some(command) => command,
            None => unwrap_pasted(&text),
        }
    } else {
        text
    };
    let text = text.trim();
    if text.is_empty() || (origin.is_none() && text.starts_with('<')) {
        return None;
    }
    Some(text.to_string())
}

/// Pasted content longer than this is shown as `[pasted]` instead.
const MAX_PASTED_CHARS: usize = 500;

/// The inner text of `tag` in `text` (`<tag ...>inner</tag ...>`), if present.
fn tag_inner<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = text.find(&format!("<{tag}"))?;
    let body_start = open + text[open..].find('>')? + 1;
    let body_len = text[body_start..].find(&format!("</{tag}"))?;
    Some(&text[body_start..body_start + body_len])
}

/// `<command-name>/x</command-name><command-args>a</command-args>` as `/x a`.
fn slash_command(text: &str) -> Option<String> {
    let name = tag_inner(text, "command-name")?.trim();
    let args = tag_inner(text, "command-args").map(str::trim).unwrap_or("");
    let name = if name.starts_with('/') {
        name.to_string()
    } else {
        format!("/{name}")
    };
    Some(if args.is_empty() {
        name
    } else {
        format!("{name} {args}")
    })
}

/// Replace each `<pasted_content ...>inner</pasted_content ...>` wrapper with
/// its inner text, or `[pasted]` when the paste is long.
fn unwrap_pasted(text: &str) -> String {
    const OPEN: &str = "<pasted_content";
    const CLOSE: &str = "</pasted_content";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find(OPEN) {
        let Some(gt) = rest[open..].find('>') else {
            break;
        };
        let body_start = open + gt + 1;
        let Some(close_len) = rest[body_start..].find(CLOSE) else {
            break;
        };
        let close = body_start + close_len;
        let Some(close_gt) = rest[close..].find('>') else {
            break;
        };
        out.push_str(&rest[..open]);
        let inner = rest[body_start..close].trim();
        if inner.chars().count() > MAX_PASTED_CHARS {
            out.push_str("[pasted]");
        } else {
            out.push_str(inner);
        }
        rest = &rest[close + close_gt + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(v: serde_json::Value) -> String {
        v.to_string()
    }

    #[test]
    fn finds_the_last_prompt_and_reply() {
        let jsonl = [
            line(serde_json::json!({"type":"user","message":{"role":"user","content":"old prompt"}})),
            line(serde_json::json!({"type":"assistant","message":{"content":[{"type":"text","text":"old reply"}]}})),
            line(serde_json::json!({"type":"user","message":{"role":"user","content":"fix the bug"}})),
            line(serde_json::json!({"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash"}]}})),
            line(serde_json::json!({"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}})),
            line(serde_json::json!({"type":"assistant","message":{"content":[{"type":"text","text":"Fixed. Run tests?"}]}})),
            line(serde_json::json!({"type":"last-prompt","lastPrompt":"x"})),
        ]
        .join("\n");
        let ex = parse_last_exchange(&jsonl).unwrap();
        assert_eq!(ex.reply, "Fixed. Run tests?");
        assert_eq!(ex.prompt.as_deref(), Some("fix the bug"));
    }

    #[test]
    fn skips_markup_meta_and_sidechains() {
        let jsonl = [
            line(serde_json::json!({"type":"user","message":{"content":"real prompt"}})),
            line(serde_json::json!({"type":"user","isMeta":true,"message":{"content":"meta"}})),
            line(serde_json::json!({"type":"user","message":{"content":"<task-notification>x</task-notification>"}})),
            line(serde_json::json!({"type":"assistant","isSidechain":true,"message":{"content":[{"type":"text","text":"subagent"}]}})),
            line(serde_json::json!({"type":"assistant","message":{"content":[{"type":"text","text":"main reply"}]}})),
        ]
        .join("\n");
        let ex = parse_last_exchange(&jsonl).unwrap();
        assert_eq!(ex.reply, "main reply");
        assert_eq!(ex.prompt.as_deref(), Some("real prompt"));
    }

    #[test]
    fn no_assistant_text_means_nothing_to_suggest_from() {
        let jsonl = line(serde_json::json!({"type":"user","message":{"content":"hello"}}));
        assert!(parse_last_exchange(&jsonl).is_none());
        assert!(parse_last_exchange("not json\n").is_none());
    }

    #[test]
    fn skips_compaction_summaries() {
        let jsonl = [
            line(serde_json::json!({"type":"user","origin":{"kind":"human"},"message":{"content":"real prompt"}})),
            line(serde_json::json!({"type":"user","isCompactSummary":true,"isVisibleInTranscriptOnly":true,"message":{"content":"This session is being continued..."}})),
            line(serde_json::json!({"type":"user","isVisibleInTranscriptOnly":true,"message":{"content":"visible only"}})),
            line(serde_json::json!({"type":"assistant","message":{"content":[{"type":"text","text":"reply"}]}})),
        ]
        .join("\n");
        let ex = parse_last_exchange(&jsonl).unwrap();
        assert_eq!(ex.prompt.as_deref(), Some("real prompt"));
    }

    #[test]
    fn skips_synthetic_and_api_error_replies() {
        let jsonl = [
            line(serde_json::json!({"type":"user","message":{"content":"do it"}})),
            line(serde_json::json!({"type":"assistant","message":{"model":"claude-opus-5-5","content":[{"type":"text","text":"real reply"}]}})),
            line(serde_json::json!({"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"No response requested."}]}})),
            line(serde_json::json!({"type":"assistant","isApiErrorMessage":true,"message":{"model":"claude-opus-5-5","content":[{"type":"text","text":"API Error: overloaded"}]}})),
        ]
        .join("\n");
        let ex = parse_last_exchange(&jsonl).unwrap();
        assert_eq!(ex.reply, "real reply");
        assert_eq!(ex.prompt.as_deref(), Some("do it"));
    }

    #[test]
    fn only_human_origin_is_a_prompt() {
        let jsonl = [
            line(serde_json::json!({"type":"user","origin":{"kind":"human"},"message":{"content":"human prompt"}})),
            line(serde_json::json!({"type":"user","origin":{"kind":"peer","from":"uds:x"},"message":{"content":"peer message"}})),
            line(serde_json::json!({"type":"user","origin":{"kind":"task-notification"},"message":{"content":"done"}})),
            line(serde_json::json!({"type":"assistant","message":{"content":[{"type":"text","text":"reply"}]}})),
        ]
        .join("\n");
        let ex = parse_last_exchange(&jsonl).unwrap();
        assert_eq!(ex.prompt.as_deref(), Some("human prompt"));
    }

    #[test]
    fn human_prompt_with_paste_is_unwrapped() {
        let short = "<pasted_content id=\"f6af\">error: boom</pasted_content id=\"f6af\">\n\nwhy?";
        let jsonl = [
            line(serde_json::json!({"type":"user","origin":{"kind":"human"},"message":{"content":"old"}})),
            line(serde_json::json!({"type":"user","origin":{"kind":"human"},"message":{"content":short}})),
            line(serde_json::json!({"type":"assistant","message":{"content":[{"type":"text","text":"reply"}]}})),
        ]
        .join("\n");
        let ex = parse_last_exchange(&jsonl).unwrap();
        assert_eq!(ex.prompt.as_deref(), Some("error: boom\n\nwhy?"));

        let long = format!(
            "<pasted_content id=\"a\">{}</pasted_content> look",
            "x".repeat(MAX_PASTED_CHARS + 1)
        );
        assert_eq!(unwrap_pasted(&long), "[pasted] look");
    }

    #[test]
    fn human_slash_command_becomes_command_line() {
        let cmd = "<command-message>design</command-message>\n<command-name>/design</command-name>\n<command-args>the home page</command-args>";
        let jsonl = [
            line(serde_json::json!({"type":"user","origin":{"kind":"human"},"message":{"content":"old"}})),
            line(serde_json::json!({"type":"user","origin":{"kind":"human"},"message":{"content":cmd}})),
            line(serde_json::json!({"type":"assistant","message":{"content":[{"type":"text","text":"reply"}]}})),
        ]
        .join("\n");
        let ex = parse_last_exchange(&jsonl).unwrap();
        assert_eq!(ex.prompt.as_deref(), Some("/design the home page"));
        assert_eq!(
            slash_command("<command-name>/clear</command-name><command-args></command-args>")
                .as_deref(),
            Some("/clear")
        );
    }
}

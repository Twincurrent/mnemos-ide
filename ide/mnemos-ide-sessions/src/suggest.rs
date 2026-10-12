// SPDX-License-Identifier: AGPL-3.0-only
//! The pure half of the agents panel's "Suggest": the instruction handed to
//! the user's own `claude` CLI, the command line it runs with, and the
//! cleanup of what it prints.
//!
//! Suggest runs on this machine, through the CLI the user already signed in
//! to (`claude -p`, prompt on stdin). Nothing is sent to the Mnemos server.
//! The app spawns the process (`app/src/workspace/view/agent_suggest.rs`);
//! everything that can be decided without a process lives here so it can be
//! tested.

/// Input caps. The reply is the useful part; the tail of a long reply carries
/// the conclusion, so long replies keep their end, not their start.
pub const MAX_PROMPT_CHARS: usize = 2_000;
pub const MAX_REPLY_CHARS: usize = 6_000;
const MAX_PROJECT_CHARS: usize = 80;
/// Output cap, after cleanup. A suggestion is one prompt, not an essay.
pub const MAX_SUGGESTION_CHARS: usize = 400;

/// The CLI used when the session names no custom command.
pub const DEFAULT_CLAUDE_CLI: &str = "claude";

/// Replaces Claude Code's own system prompt: a one-line answer needs none of
/// it, and a short one keeps the call quick. Plain ASCII without `%`, `^`,
/// quotes or newlines, so it survives a Windows `.cmd` shim's command line.
const SYSTEM_PROMPT: &str =
    "You predict the next message a developer will send to their AI coding \
agent. Reply with that one message only: a single short line, no quotes, no preamble.";

/// Hooks off for this one run: the user's SessionStart hooks would add
/// context and time, and a Stop hook could keep the run going.
const SETTINGS_JSON: &str = r#"{"disableAllHooks":true}"#;

/// The arguments for one headless suggestion run; the instruction goes on
/// stdin. Print mode, a small fast model, no tools (`--tools ""`), no MCP
/// servers, no hooks, and no transcript written to `~/.claude/projects`, so
/// the run never shows up in the sessions list or in transcript sync.
pub fn claude_print_args() -> Vec<&'static str> {
    vec![
        "-p",
        "--model",
        "haiku",
        "--output-format",
        "text",
        "--tools",
        "",
        "--strict-mcp-config",
        "--no-session-persistence",
        "--settings",
        SETTINGS_JSON,
        "--system-prompt",
        SYSTEM_PROMPT,
    ]
}

/// The CLI names to try, in order: the session's custom command (the first
/// word of the command the agent was started with, when the app detected it
/// through a custom toolbar pattern), then plain `claude`. A custom command
/// that is a shell alias or function resolves to no file; the caller then
/// falls through to `claude`.
pub fn cli_candidates(custom_command_prefix: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(custom) = custom_command_prefix.map(str::trim) {
        let usable = !custom.is_empty()
            && !custom
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '\'' | '`'));
        if usable {
            out.push(custom.to_string());
        }
    }
    if !out.iter().any(|c| c == DEFAULT_CLAUDE_CLI) {
        out.push(DEFAULT_CLAUDE_CLI.to_string());
    }
    out
}

/// The file names a command name can have on disk. On Windows a bare name
/// is tried as `.exe` (native installer), then `.cmd` / `.bat` (npm shims);
/// a name that already has one of those extensions is taken as is.
pub fn executable_file_names(name: &str, windows: bool) -> Vec<String> {
    if !windows {
        return vec![name.to_string()];
    }
    let lower = name.to_ascii_lowercase();
    if [".exe", ".cmd", ".bat"]
        .iter()
        .any(|ext| lower.ends_with(ext))
    {
        return vec![name.to_string()];
    }
    ["exe", "cmd", "bat"]
        .iter()
        .map(|ext| format!("{name}.{ext}"))
        .collect()
}

/// The instruction sent to the CLI on stdin.
pub fn build_instruction(project: Option<&str>, prompt: Option<&str>, reply: &str) -> String {
    let project = project
        .map(|p| head(p.trim(), MAX_PROJECT_CHARS))
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| "a software project".to_string());
    let prompt = prompt
        .map(|p| head(p.trim(), MAX_PROMPT_CHARS))
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| "(not available)".to_string());
    let reply = tail(reply.trim(), MAX_REPLY_CHARS);
    format!(
        "A developer is working with an AI coding agent (Claude Code) in {project}.\n\
         Write the single next prompt this developer would most likely send the agent.\n\
         Rules:\n\
         - Write it as the developer, in the same language as the developer's last prompt.\n\
         - If the agent asked a question or offered options, answer or pick sensibly.\n\
         - If the agent finished, ask for the natural next step (verify, commit, release, the next item).\n\
         - One short line, at most 25 words. No quotes, no preamble, no explanation.\n\n\
         Developer's last prompt:\n{prompt}\n\n\
         Agent's last reply:\n{reply}\n\n\
         Next prompt:"
    )
}

/// One line, no wrapping quotes or "Next prompt:" echo, capped.
pub fn clean_suggestion(raw: &str) -> String {
    let joined = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut s = joined.as_str();
    for prefix in ["Next prompt:", "next prompt:", "Prompt:"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.trim_start();
        }
    }
    let s = s
        .trim_matches(|c| matches!(c, '"' | '\'' | '`' | '\u{201c}' | '\u{201d}'))
        .trim();
    head(s, MAX_SUGGESTION_CHARS)
}

fn head(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

fn tail(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    s.chars().skip(n - max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestions_are_one_clean_line() {
        assert_eq!(clean_suggestion("  \"Run the tests\"  "), "Run the tests");
        assert_eq!(
            clean_suggestion("Next prompt: commit and push"),
            "commit and push"
        );
        assert_eq!(
            clean_suggestion("line one\nline two\n"),
            "line one line two"
        );
        assert_eq!(clean_suggestion("`ja, gør det`"), "ja, gør det");
        assert_eq!(
            clean_suggestion(&"x".repeat(1000)).chars().count(),
            MAX_SUGGESTION_CHARS
        );
        assert_eq!(clean_suggestion("   "), "");
    }

    #[test]
    fn long_replies_keep_their_end() {
        let reply = format!("{}THE CONCLUSION", "a".repeat(10_000));
        let instruction = build_instruction(None, None, &reply);
        assert!(instruction.contains("THE CONCLUSION"));
        assert!(instruction.chars().count() < MAX_REPLY_CHARS + 2_000);
    }

    #[test]
    fn missing_fields_have_placeholders() {
        let instruction = build_instruction(Some("  "), None, "done");
        assert!(instruction.contains("in a software project"));
        assert!(instruction.contains("(not available)"));
    }

    #[test]
    fn cuts_are_char_safe() {
        assert_eq!(head("æøåæøå", 3), "æøå");
        assert_eq!(tail("æøåæøå", 2), "øå");
    }

    #[test]
    fn args_are_print_mode_without_tools_or_persistence() {
        let args = claude_print_args();
        assert_eq!(args[0], "-p");
        let tools = args.iter().position(|a| *a == "--tools").unwrap();
        assert_eq!(args[tools + 1], "");
        assert!(args.contains(&"--no-session-persistence"));
        assert!(args.contains(&"--strict-mcp-config"));
        let settings = args.iter().position(|a| *a == "--settings").unwrap();
        let parsed: serde_json::Value = serde_json::from_str(args[settings + 1]).unwrap();
        assert_eq!(parsed["disableAllHooks"], true);
        // Nothing a Windows batch shim would mangle, and no prompt content:
        // the instruction travels on stdin.
        for arg in &args {
            assert!(!arg.contains('%') && !arg.contains('^') && !arg.contains('\n'));
        }
    }

    #[test]
    fn custom_command_comes_first_then_claude() {
        assert_eq!(cli_candidates(None), vec!["claude"]);
        assert_eq!(cli_candidates(Some("  ")), vec!["claude"]);
        assert_eq!(cli_candidates(Some("claude")), vec!["claude"]);
        assert_eq!(
            cli_candidates(Some("/opt/bin/claude-work")),
            vec!["/opt/bin/claude-work", "claude"]
        );
        assert_eq!(cli_candidates(Some("cc")), vec!["cc", "claude"]);
        // Not something a single path or name can be.
        assert_eq!(cli_candidates(Some("a b")), vec!["claude"]);
        assert_eq!(cli_candidates(Some("\"x\"")), vec!["claude"]);
    }

    #[test]
    fn windows_tries_exe_then_shims() {
        assert_eq!(executable_file_names("claude", false), vec!["claude"]);
        assert_eq!(
            executable_file_names("claude", true),
            vec!["claude.exe", "claude.cmd", "claude.bat"]
        );
        assert_eq!(
            executable_file_names("C:\\npm\\claude.CMD", true),
            vec!["C:\\npm\\claude.CMD"]
        );
    }
}

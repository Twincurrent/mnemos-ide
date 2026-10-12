//! Mnemos IDE: the agents panel's "Suggest", run on this machine through the
//! user's own `claude` CLI.
//!
//! The last exchange of the agent's session (read from its local transcript)
//! goes on stdin to `claude -p` with no tools, no MCP servers, no hooks and no
//! session persistence (`mnemos_ide_sessions::suggest::claude_print_args`).
//! Its stdout, cleaned to one line, is the suggestion. Nothing goes to the
//! Mnemos server; the model call is made by the CLI under the user's own
//! Claude login, the same account the agent itself runs on.
//!
//! The CLI is the session's custom command when the app detected the agent
//! through a custom toolbar pattern and that command is a file on PATH (or a
//! path), else `claude`. PATH is the one captured from the user's interactive
//! login shell when available, so a Finder- or Dock-launched app still finds
//! an npm or Homebrew install (and the `node` its shim needs).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A suggestion is interactive; give up rather than leave the row at
/// "thinking..." for long. CLI start-up is a few seconds of this.
#[cfg_attr(target_family = "wasm", allow(dead_code))]
const SUGGEST_TIMEOUT: Duration = Duration::from_secs(45);

/// Variables removed from the child's environment: the agents-panel plugin
/// hooks only report when they see Mnemos's terminal variables, and a
/// headless suggestion run must never show up as an agent. `CLAUDECODE` is
/// set inside a Claude Code session and would make the CLI treat this run as
/// nested when Mnemos itself was started from one.
#[cfg_attr(target_family = "wasm", allow(dead_code))]
const SCRUBBED_ENV: &[&str] = &[
    "MNEMOS_TERMINAL",
    "WARP_TERMINAL",
    "MNEMOS_CLI_AGENT_PROTOCOL_VERSION",
    "WARP_CLI_AGENT_PROTOCOL_VERSION",
    "MNEMOS_CLI_AGENT_TOKEN",
    "CLAUDECODE",
];

/// The first candidate (see `mnemos_ide_sessions::suggest::cli_candidates`)
/// that is a file: a name is looked up in `path_env` (else the process's
/// PATH), a path is taken as given, with `~/` expanded. When `path_env` finds
/// none, the process's PATH gets a second try.
pub(super) fn resolve_cli(candidates: &[String], path_env: Option<&str>) -> Option<PathBuf> {
    let process_path = std::env::var_os("PATH").unwrap_or_default();
    match path_env {
        Some(p) => resolve_in(candidates, &OsString::from(p))
            .or_else(|| resolve_in(candidates, &process_path)),
        None => resolve_in(candidates, &process_path),
    }
}

fn resolve_in(candidates: &[String], path_var: &OsString) -> Option<PathBuf> {
    let windows = cfg!(windows);
    candidates.iter().find_map(|name| {
        let name = expand_home(name);
        let is_path = name.contains('/') || (windows && name.contains('\\'));
        let names = mnemos_ide_sessions::suggest::executable_file_names(&name, windows);
        if is_path {
            names
                .into_iter()
                .map(PathBuf::from)
                .find(|p| is_runnable(p))
        } else {
            std::env::split_paths(path_var)
                .find_map(|dir| names.iter().map(|n| dir.join(n)).find(|p| is_runnable(p)))
        }
    })
}

fn expand_home(name: &str) -> String {
    let rest = name.strip_prefix("~/").or_else(|| name.strip_prefix("~\\"));
    match (rest, dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest).to_string_lossy().into_owned(),
        _ => name.to_string(),
    }
}

/// On Windows the extension decides (`executable_file_names` only offers
/// `.exe`, `.cmd` and `.bat`); elsewhere the execute bit does.
fn is_runnable(path: &Path) -> bool {
    if cfg!(windows) {
        path.is_file()
    } else {
        crate::util::path::file_exists_and_is_executable(path)
    }
}

/// Run one suggestion. `Err` carries a short reason for the panel row.
#[cfg(not(target_family = "wasm"))]
pub(super) async fn run_claude_suggest(
    cli: PathBuf,
    path_env: Option<String>,
    instruction: String,
) -> Result<String, String> {
    use command::r#async::Command;
    use command::Stdio;
    use futures::future::Either;
    use futures::AsyncWriteExt as _;

    let mut cmd = Command::new(&cli);
    cmd.args(mnemos_ide_sessions::suggest::claude_print_args())
        // A neutral folder: no project CLAUDE.md or settings, and nothing
        // that could tie the run to a project.
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for var in SCRUBBED_ENV {
        cmd.env_remove(var);
    }
    if let Some(path_env) = path_env.as_deref() {
        cmd.env("PATH", path_env);
    }

    let run = async move {
        let mut child = cmd.spawn().map_err(|e| {
            log::warn!(target: "mnemos_ide", "mnemos_ide_agents: suggest could not start {}: {e}", cli.display());
            "Could not start claude".to_string()
        })?;
        if let Some(mut stdin) = child.stdin.take() {
            // A write error means the CLI exited early; its exit status and
            // stderr below say why.
            let _ = stdin.write_all(instruction.as_bytes()).await;
            let _ = stdin.close().await;
        }
        let output = child.output().await.map_err(|e| {
            log::warn!(target: "mnemos_ide", "mnemos_ide_agents: suggest failed to run: {e}");
            "Suggestion failed".to_string()
        })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let short: String = stderr.chars().take(300).collect();
            log::warn!(
                target: "mnemos_ide",
                "mnemos_ide_agents: suggest: claude exited with {}: {short}",
                output.status
            );
            let lower = stderr.to_ascii_lowercase();
            if lower.contains("unknown option") || lower.contains("unknown argument") {
                return Err("Update Claude Code to use suggest".to_string());
            }
            if lower.contains("log in") || lower.contains("login") || lower.contains("api key") {
                return Err("Sign in to Claude Code to use suggest".to_string());
            }
            return Err("Suggestion failed".to_string());
        }
        let suggestion = mnemos_ide_sessions::suggest::clean_suggestion(&String::from_utf8_lossy(
            &output.stdout,
        ));
        if suggestion.is_empty() {
            return Err("No suggestion".to_string());
        }
        Ok(suggestion)
    };

    let timeout = mnemos_ui::r#async::Timer::after(SUGGEST_TIMEOUT);
    futures::pin_mut!(run);
    futures::pin_mut!(timeout);
    // Losing the race drops `run`, and with it the child (`kill_on_drop`).
    match futures::future::select(run, timeout).await {
        Either::Left((result, _)) => result,
        Either::Right(_) => Err("Suggestion timed out".to_string()),
    }
}

#[cfg(target_family = "wasm")]
pub(super) async fn run_claude_suggest(
    _cli: PathBuf,
    _path_env: Option<String>,
    _instruction: String,
) -> Result<String, String> {
    Err("Suggest needs the desktop app".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_cli_resolves_to_nothing() {
        let candidates = vec!["mnemos-no-such-cli-for-suggest".to_string()];
        assert_eq!(resolve_cli(&candidates, Some("")), None);
        assert_eq!(
            resolve_cli(&["/nonexistent/dir/claude".to_string()], None),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn falls_through_to_the_next_candidate_on_path() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("claude");
        std::fs::write(&claude, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path_env = dir.path().to_string_lossy().into_owned();
        // `cc` is an alias in the user's shell, not a file: skipped.
        let got = resolve_cli(&["cc".to_string(), "claude".to_string()], Some(&path_env));
        assert_eq!(got, Some(claude.clone()));
        // A path is taken as given.
        let as_path = claude.to_string_lossy().into_owned();
        assert_eq!(resolve_cli(&[as_path], Some("")), Some(claude));
    }

    #[cfg(unix)]
    #[test]
    fn a_file_without_the_execute_bit_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        // A name the process's PATH (the fallback) cannot have either.
        let name = "mnemos-suggest-test-not-runnable";
        std::fs::write(dir.path().join(name), "not runnable").unwrap();
        let path_env = dir.path().to_string_lossy().into_owned();
        assert_eq!(resolve_cli(&[name.to_string()], Some(&path_env)), None);
    }
}

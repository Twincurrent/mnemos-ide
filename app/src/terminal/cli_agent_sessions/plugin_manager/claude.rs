use std::collections::HashMap;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use async_trait::async_trait;
use serde_json::Value;

use super::{
    compare_versions, run_cli_command_logged, CliAgentPluginManager, PluginInstallError,
    PluginInstructionStep, PluginInstructions,
};
use crate::terminal::model::session::LocalCommandExecutor;
use crate::terminal::shell::ShellType;

// The upstream platform plugin (oz-harness-support) lived in an internal Warp marketplace; the
// rebrand pointed it at an unregistered GitHub org that anyone could claim, and it was
// installed without asking. It is gone, along with the trait method that installed it.

// Mnemos IDE: the plugin Mnemos installs and tells users to install is its own, `mnemos-agents`
// from the `mnemos` marketplace in this repository (`.claude-plugin/marketplace.json` at the repo
// root, plugin source in `ide/claude-plugin`). It writes its events to the agent's PTY with this
// terminal's token. Upstream Warp's `warp@claude-code-warp` is no longer installed; an existing
// install of it is still recognised (and its hook still patched for file ingest), so a machine
// that has only Warp's plugin is not told to install a second one.
const PLUGIN_KEY: &str = "mnemos-agents@mnemos";
const MARKETPLACE_NAME: &str = "mnemos";
/// The public source repository, which carries the `mnemos` marketplace at its root.
const MARKETPLACE_SOURCE: &str = concat!(env!("MNEMOS_SOURCE_URL"), ".git");
/// Check out only the marketplace and the plugin, not the whole repository.
const MARKETPLACE_SPARSE_PATHS: [&str; 2] = [".claude-plugin", "ide/claude-plugin"];
/// Lowest `mnemos-agents` release (its `plugin.json` version, which is what
/// `installed_plugins.json` records) that carries the PTY token.
const MINIMUM_INSTALLED_VERSION: &str = "1.2.0";

/// Upstream Warp's plugin, recognised only when it is already installed.
const WARP_PLUGIN_KEY: &str = "warp@claude-code-warp";
const WARP_MARKETPLACE_NAME: &str = "claude-code-warp";
const WARP_MINIMUM_INSTALLED_VERSION: &str = "2.0.0";

/// The event protocol generation a plugin reports in each event's `plugin_version`.
/// `mnemos-agents` reports 2.1.0 for compatibility with this scheme (see its `lib.sh`).
const MINIMUM_PLUGIN_VERSION: &str = "2.0.0";

const INSTALL_MARKETPLACE_COMMAND: &str = concat!(
    "claude plugin marketplace add ",
    env!("MNEMOS_SOURCE_URL"),
    ".git --sparse .claude-plugin ide/claude-plugin"
);
const INSTALL_PLUGIN_COMMAND: &str = "claude plugin install mnemos-agents@mnemos";

pub(super) struct ClaudeCodePluginManager {
    executor: LocalCommandExecutor,
    path_env_var: Option<String>,
}

impl ClaudeCodePluginManager {
    pub(super) fn new(
        shell_path: Option<PathBuf>,
        shell_type: Option<ShellType>,
        path_env_var: Option<String>,
    ) -> Self {
        let shell_type = shell_type.unwrap_or(ShellType::Bash);
        Self {
            executor: LocalCommandExecutor::new(shell_path, shell_type),
            path_env_var,
        }
    }

    async fn add_marketplace(&self, log: &mut String) -> Result<(), PluginInstallError> {
        let mut args = vec![
            "plugin",
            "marketplace",
            "add",
            MARKETPLACE_SOURCE,
            "--sparse",
        ];
        args.extend(MARKETPLACE_SPARSE_PATHS);
        self.run_logged(&args, log).await
    }

    async fn run_logged(&self, args: &[&str], log: &mut String) -> Result<(), PluginInstallError> {
        let env_vars = self
            .path_env_var
            .as_deref()
            .map(|path| HashMap::from([("PATH".to_owned(), path.to_owned())]));
        run_cli_command_logged("claude", args, &self.executor, env_vars, log).await
    }
}

#[async_trait]
impl CliAgentPluginManager for ClaudeCodePluginManager {
    fn minimum_plugin_version(&self) -> &'static str {
        MINIMUM_PLUGIN_VERSION
    }

    fn can_auto_install(&self) -> bool {
        true
    }

    fn is_installed(&self) -> bool {
        let Ok(claude_dir) = claude_home_dir() else {
            return false;
        };
        check_installed(&claude_dir)
    }

    /// Runs `claude plugin` CLI commands via the session shell.
    async fn install(&self) -> Result<(), PluginInstallError> {
        let mut log = String::new();
        self.add_marketplace(&mut log).await?;
        self.run_logged(&["plugin", "install", PLUGIN_KEY], &mut log)
            .await?;
        Ok(())
    }

    /// Mnemos IDE: refreshes the `mnemos` marketplace in place and updates the plugin. It
    /// never removes the marketplace: on a machine that registered it as a local directory
    /// (`script/install-claude-plugin`), removing it and re-adding it from the public
    /// repository loses the working source whenever that add fails, and leaves the plugin
    /// unlinked. Only a machine with no `mnemos` marketplace at all gets it added.
    async fn update(&self) -> Result<(), PluginInstallError> {
        let mut log = String::new();
        let claude_dir = claude_home_dir().ok();
        let registered = claude_dir
            .as_deref()
            .is_some_and(|dir| marketplace_registered(dir, MARKETPLACE_NAME));
        if registered {
            if let Err(mut err) = self
                .run_logged(
                    &["plugin", "marketplace", "update", MARKETPLACE_NAME],
                    &mut log,
                )
                .await
            {
                err.log.push_str(MARKETPLACE_UPDATE_FAILED_HINT);
                return Err(err);
            }
        } else {
            self.add_marketplace(&mut log).await?;
        }
        let plugin_installed = claude_dir
            .as_deref()
            .is_some_and(|dir| check_installed_key(dir, PLUGIN_KEY));
        if plugin_installed {
            self.run_logged(&["plugin", "update", PLUGIN_KEY], &mut log)
                .await?;
        } else {
            self.run_logged(&["plugin", "install", PLUGIN_KEY], &mut log)
                .await?;
        }

        // Sanity check: verify the on-disk version actually changed.
        let still_outdated = claude_home_dir()
            .ok()
            .and_then(|dir| installed_version_of(&dir, PLUGIN_KEY))
            .map(|v| compare_versions(&v, MINIMUM_INSTALLED_VERSION).is_lt())
            .unwrap_or(true);
        if still_outdated {
            log.push_str("Post-update version check: plugin is still outdated\n");
            log.push_str(STILL_OUTDATED_HINT);
            return Err(PluginInstallError {
                message: "Plugin update did not take effect".to_owned(),
                log,
            });
        }
        Ok(())
    }

    fn install_success_message(&self) -> &'static str {
        "Mnemos plugin installed. Please run /reload-plugins to activate."
    }

    fn update_success_message(&self) -> &'static str {
        "Mnemos plugin updated. Please run /reload-plugins to activate."
    }

    fn install_instructions(&self) -> &'static PluginInstructions {
        &INSTALL_INSTRUCTIONS
    }

    fn update_instructions(&self) -> &'static PluginInstructions {
        &UPDATE_INSTRUCTIONS
    }

    fn needs_update(&self) -> bool {
        matches!(
            plugin_install_state(),
            ClaudePluginInstallState::Outdated { .. }
        )
    }
}

static INSTALL_INSTRUCTIONS: LazyLock<PluginInstructions> = LazyLock::new(|| {
    PluginInstructions {
        title: "Install the Mnemos plugin for Claude Code",
        subtitle: "Ensure that jq is installed on your machine. Then, run these commands.",
        steps: &[
            PluginInstructionStep {
                description: "Add the Mnemos plugin marketplace (from the Mnemos source repository)",
                command: INSTALL_MARKETPLACE_COMMAND,
                executable: true,
                link: None,
            },
            PluginInstructionStep {
                description: "Install the mnemos-agents plugin",
                command: INSTALL_PLUGIN_COMMAND,
                executable: true,
                link: None,
            },
        ],
        post_install_notes: &[
            "Restart Claude Code to activate the plugin.",
            "There are some known issues with Claude Code's plugin system. \
             If the plugin is not found after step 1, you can try manually adding an \"extraKnownMarketplaces\" entry to ~/.claude/settings.json.",
        ],
    }
});

/// Mnemos IDE: the update steps refresh the existing `mnemos` marketplace in place; they
/// never remove it (see [`ClaudeCodePluginManager::update`]).
const UPDATE_MARKETPLACE_COMMAND: &str = "claude plugin marketplace update mnemos";
const UPDATE_PLUGIN_COMMAND: &str = "claude plugin update mnemos-agents@mnemos";

/// Logged when `claude plugin marketplace update mnemos` fails. The marketplace is left
/// registered as it was.
const MARKETPLACE_UPDATE_FAILED_HINT: &str = "The mnemos marketplace could not be refreshed and \
was left as it is. If its source moved (a local directory that is gone, for one), point it at \
the new location yourself: claude plugin marketplace remove mnemos, then claude plugin \
marketplace add <location>, then claude plugin install mnemos-agents@mnemos.\n";

/// Logged when the update ran but the installed plugin is still below the minimum, which
/// happens when the marketplace's source (a local checkout, for one) is itself older.
const STILL_OUTDATED_HINT: &str = "The mnemos marketplace's source still carries an older \
mnemos-agents. If it is a local checkout, update that checkout and try again.\n";

static UPDATE_INSTRUCTIONS: LazyLock<PluginInstructions> = LazyLock::new(|| PluginInstructions {
    title: "Update the Mnemos plugin for Claude Code",
    subtitle: "Run the following commands.",
    steps: &[
        PluginInstructionStep {
            description: "Refresh the Mnemos plugin marketplace",
            command: UPDATE_MARKETPLACE_COMMAND,
            executable: true,
            link: None,
        },
        PluginInstructionStep {
            description: "Update the mnemos-agents plugin",
            command: UPDATE_PLUGIN_COMMAND,
            executable: true,
            link: None,
        },
    ],
    post_install_notes: &[
        "Restart Claude Code to activate the update.",
        concat!(
            "If step 1 reports that the mnemos marketplace is not found, add it with ",
            "claude plugin marketplace add ",
            env!("MNEMOS_SOURCE_URL"),
            ".git --sparse .claude-plugin ide/claude-plugin",
            " and then run claude plugin install mnemos-agents@mnemos."
        ),
    ],
});

/// Health of the Claude Code plugin install. The Agents Pane uses this to
/// explain blank query/response previews: the plugin (`mnemos-agents@mnemos`,
/// or an existing install of upstream's `warp@claude-code-warp`) is what emits
/// the OSC 777 events Mnemos parses into row text. Pure read of
/// `installed_plugins.json`; never spawns the `claude` CLI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ClaudePluginInstallState {
    /// Neither plugin key is in `installed_plugins.json` (or the file is unreadable).
    Missing,
    /// Installed but below its minimum version (`found` = on-disk version, or
    /// `"unknown"` when the version field is missing, which is a very old plugin).
    Outdated { found: String },
    /// Installed at or above its minimum version.
    Current,
}

/// Reads the plugin install state from `~/.claude` (or `$CLAUDE_HOME`).
/// Mirrors the `is_installed()`/`needs_update()` logic without constructing a
/// shell-backed `ClaudeCodePluginManager`, so it's safe to call on the UI thread.
pub(crate) fn plugin_install_state() -> ClaudePluginInstallState {
    let Ok(claude_dir) = claude_home_dir() else {
        return ClaudePluginInstallState::Missing;
    };
    install_state_in(&claude_dir)
}

/// The install state in `claude_dir`: `mnemos-agents` when it is installed, else upstream's
/// Warp plugin, else [`ClaudePluginInstallState::Missing`].
fn install_state_in(claude_dir: &Path) -> ClaudePluginInstallState {
    let (key, minimum) = if check_installed_key(claude_dir, PLUGIN_KEY) {
        (PLUGIN_KEY, MINIMUM_INSTALLED_VERSION)
    } else if check_installed_key(claude_dir, WARP_PLUGIN_KEY) {
        (WARP_PLUGIN_KEY, WARP_MINIMUM_INSTALLED_VERSION)
    } else {
        return ClaudePluginInstallState::Missing;
    };
    match installed_version_of(claude_dir, key) {
        Some(v) if compare_versions(&v, minimum).is_lt() => {
            ClaudePluginInstallState::Outdated { found: v }
        }
        // Installed but no version field => very old plugin; treat as outdated.
        None => ClaudePluginInstallState::Outdated {
            found: "unknown".to_owned(),
        },
        Some(_) => ClaudePluginInstallState::Current,
    }
}

/// Mnemos IDE: whether the installed `mnemos-agents` predates the PTY token (below
/// [`MINIMUM_INSTALLED_VERSION`], or with no version recorded), so its hooks send bare
/// `mnemos://cli-agent` events. Pure read of `installed_plugins.json`. The terminal model
/// trusts such events, while the pane runs `claude`, only while this is true, so an
/// existing install keeps working until its plugin is updated.
pub(crate) fn mnemos_agents_predates_token() -> bool {
    claude_home_dir().is_ok_and(|dir| mnemos_agents_predates_token_in(&dir))
}

fn mnemos_agents_predates_token_in(claude_dir: &Path) -> bool {
    check_installed_key(claude_dir, PLUGIN_KEY)
        && installed_version_of(claude_dir, PLUGIN_KEY)
            .is_none_or(|v| compare_versions(&v, MINIMUM_INSTALLED_VERSION).is_lt())
}

/// Mnemos IDE: whether Claude Code has a marketplace called `name` registered, from
/// `plugins/known_marketplaces.json`.
fn marketplace_registered(claude_dir: &Path, name: &str) -> bool {
    let path = claude_dir.join("plugins").join("known_marketplaces.json");
    let Ok(contents) = fs::read_to_string(path) else {
        return false;
    };
    serde_json::from_str::<Value>(&contents)
        .ok()
        .and_then(|parsed| parsed.get(name).map(|entry| !entry.is_null()))
        .unwrap_or(false)
}

/// Whether either plugin (Mnemos's own, or upstream Warp's) is installed.
fn check_installed(claude_dir: &Path) -> bool {
    check_installed_key(claude_dir, PLUGIN_KEY) || check_installed_key(claude_dir, WARP_PLUGIN_KEY)
}

fn check_installed_key(claude_dir: &Path, key: &str) -> bool {
    let plugins_path = claude_dir.join("plugins").join("installed_plugins.json");
    let Ok(contents) = fs::read_to_string(plugins_path) else {
        return false;
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&contents) else {
        return false;
    };
    parsed
        .get("plugins")
        .and_then(|p| p.get(key))
        .and_then(|v| v.as_array())
        .map(|arr| !arr.is_empty())
        .unwrap_or(false)
}

/// Reads the installed version string for the Mnemos plugin, if present.
#[cfg(test)]
fn installed_version(claude_dir: &Path) -> Option<String> {
    installed_version_of(claude_dir, PLUGIN_KEY)
}

/// Reads the installed version string of the plugin `key`, if present.
fn installed_version_of(claude_dir: &Path, key: &str) -> Option<String> {
    let plugins_path = claude_dir.join("plugins").join("installed_plugins.json");
    let contents = fs::read_to_string(plugins_path).ok()?;
    let parsed: Value = serde_json::from_str(&contents).ok()?;
    parsed
        .get("plugins")?
        .get(key)?
        .as_array()?
        .first()?
        .get("version")?
        .as_str()
        .map(|s| s.to_owned())
}

/// Sentinel comment marking that `warp-notify.sh` has been patched for Option D
/// file-based event ingestion. Used to keep the patch idempotent: presence of
/// this exact line means the append block is already there.
const FILE_INGEST_PATCH_SENTINEL: &str = "# === MNEMOS file-ingest (Option D) ===";

/// The shell block appended to `warp-notify.sh` so the hook ALSO writes each
/// event as one NDJSON line to the per-terminal file Mnemos exports via
/// `MNEMOS_CLI_AGENT_EVENT_FILE`. This is the Windows fallback for when the
/// OSC-777 transport can't reach the ConPTY. On macOS/Linux, where OSC also
/// works, every event arrives twice; Mnemos drops the file copy for terminals
/// that have already received events over OSC (see
/// `CLIAgentSessionsModel::apply_ingested_event`). `$BODY` is the full event
/// JSON (set by the upstream script from arg 2).
///
/// Mnemos IDE: the append runs under `umask 077`, so a file the hook creates is
/// readable by this user only (Mnemos also pre-creates it 0600, see
/// `file_ingest::precreate_event_file`).
const FILE_INGEST_PATCH_BLOCK: &str = "\n# === MNEMOS file-ingest (Option D) ===\n\
# Additionally append the event JSON to the per-terminal NDJSON file Mnemos\n\
# tails, when one is exported. Best-effort; never fails the hook. The umask\n\
# keeps a file created here private to this user.\n\
if [ -n \"${MNEMOS_CLI_AGENT_EVENT_FILE:-}\" ]; then\n\
    (umask 077; printf '%s\\n' \"$BODY\" >> \"$MNEMOS_CLI_AGENT_EVENT_FILE\") 2>/dev/null || true\n\
fi\n";

/// Mnemos IDE: the append line earlier builds (and `script/install-claude-plugin`,
/// indented) wrote, without the umask. A script that still carries it is upgraded in
/// place to the umask form.
const LEGACY_FILE_INGEST_APPEND_LINE: &str =
    "printf '%s\\n' \"$BODY\" >> \"$MNEMOS_CLI_AGENT_EVENT_FILE\" 2>/dev/null || true";
const FILE_INGEST_APPEND_LINE: &str =
    "(umask 077; printf '%s\\n' \"$BODY\" >> \"$MNEMOS_CLI_AGENT_EVENT_FILE\") 2>/dev/null || true";

/// Idempotently patches every installed `warp-notify.sh` (upstream Warp's
/// plugin, which Mnemos no longer installs but still supports where it is
/// installed) so the hook appends events to the Mnemos per-terminal event file
/// (Option D). When the sentinel is already present only an old, umask-less
/// append line is upgraded.
///
/// `claude plugin install`/`update` overwrites `warp-notify.sh` from the
/// marketplace, wiping the patch. So this is re-run once at app start. It is
/// not re-run on agent detection: after an install or update done outside
/// Mnemos the hook stays unpatched until the next app start. Safe to call
/// repeatedly.
pub(crate) fn patch_warp_notify_for_file_ingest() {
    for script in warp_notify_script_paths() {
        if let Err(err) = patch_one_warp_notify(&script) {
            log::warn!(
                "Failed to patch {} for file ingestion: {err}",
                script.display()
            );
        }
    }
}

/// Returns every `warp-notify.sh` under the Claude plugin cache for the Warp
/// plugin, across all installed version directories.
fn warp_notify_script_paths() -> Vec<PathBuf> {
    let Ok(home) = claude_home_dir() else {
        return Vec::new();
    };
    // ~/.claude/plugins/cache/<MARKETPLACE_NAME>/warp/<version>/scripts/warp-notify.sh
    let versions_root = home
        .join("plugins")
        .join("cache")
        .join(WARP_MARKETPLACE_NAME)
        .join("warp");
    let Ok(entries) = fs::read_dir(&versions_root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path().join("scripts").join("warp-notify.sh"))
        .filter(|p| p.is_file())
        .collect()
}

/// Appends the file-ingest block to a single `warp-notify.sh` if absent.
fn patch_one_warp_notify(script: &Path) -> io::Result<()> {
    let contents = fs::read_to_string(script)?;
    if let Some(patched) = file_ingest_patched_contents(&contents) {
        fs::write(script, patched)?;
    }
    Ok(())
}

/// The patched script for `contents`, or `None` when it needs no change.
fn file_ingest_patched_contents(contents: &str) -> Option<String> {
    if contents.contains(FILE_INGEST_PATCH_SENTINEL) {
        // Mnemos IDE: upgrade an earlier block, which created the file without a umask.
        // The line's indentation is kept.
        let upgraded: Vec<String> = contents
            .split('\n')
            .map(|line| {
                if line.trim() == LEGACY_FILE_INGEST_APPEND_LINE {
                    let indent = &line[..line.len() - line.trim_start().len()];
                    format!("{indent}{FILE_INGEST_APPEND_LINE}")
                } else {
                    line.to_owned()
                }
            })
            .collect();
        let upgraded = upgraded.join("\n");
        return (upgraded != contents).then_some(upgraded);
    }
    let mut patched = contents.to_owned();
    if !patched.ends_with('\n') {
        patched.push('\n');
    }
    patched.push_str(FILE_INGEST_PATCH_BLOCK);
    Some(patched)
}

/// Checks `CLAUDE_HOME` env var first, falls back to `~/.claude`.
fn claude_home_dir() -> io::Result<PathBuf> {
    if let Ok(claude_home) = env::var("CLAUDE_HOME") {
        return Ok(PathBuf::from(claude_home));
    }
    dirs::home_dir()
        .map(|home| home.join(".claude"))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "could not determine home directory",
            )
        })
}

#[cfg(test)]
#[path = "claude_tests.rs"]
mod tests;

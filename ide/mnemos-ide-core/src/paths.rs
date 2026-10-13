// SPDX-License-Identifier: AGPL-3.0-only
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::error::IdeError;
use crate::registry;

/// Process-wide redirect for [`ide_data_dir`]. See [`override_data_dir`].
static DATA_DIR_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

/// Environment variable that redirects every mnemos-ide-* data path for this
/// process. Meant for dev builds that must not touch the daily-driver store.
/// [`LEGACY_DATA_DIR_ENV`] is still honoured; see [`data_dir_env`].
pub const MNEMOS_DATA_DIR_ENV: &str = "MNEMOS_DATA_DIR";

/// Pre-rename name of [`MNEMOS_DATA_DIR_ENV`], read when the new one is unset
/// or empty, so a shell, script or older `script/run` that still exports it
/// keeps its build isolated.
pub const LEGACY_DATA_DIR_ENV: &str = "TCB_DATA_DIR";

/// The value of the environment variable `name`, falling back to its
/// pre-rename name `legacy`. Precedence: `name` when it is set and non-empty,
/// else `legacy` when it is set and non-empty, else `None`. An empty value
/// counts as unset for both, so `MNEMOS_X= cmd` does not hide a set `TCB_X`.
///
/// Every `MNEMOS_*` variable that replaced a `TCB_*` one is read through this,
/// so the old names keep working after the rename.
pub fn env_var_os(name: &str, legacy: &str) -> Option<std::ffi::OsString> {
    pick_env(std::env::var_os(name), std::env::var_os(legacy))
}

/// [`env_var_os`] as a `String`, `None` when the chosen value is not UTF-8.
pub fn env_var(name: &str, legacy: &str) -> Option<String> {
    env_var_os(name, legacy).and_then(|v| v.into_string().ok())
}

/// The precedence rule of [`env_var_os`], apart from the process environment
/// so tests can cover it without mutating env vars under parallel tests.
fn pick_env(
    new: Option<std::ffi::OsString>,
    legacy: Option<std::ffi::OsString>,
) -> Option<std::ffi::OsString> {
    new.filter(|v| !v.is_empty())
        .or_else(|| legacy.filter(|v| !v.is_empty()))
}

/// The redirected data dir from the environment: [`MNEMOS_DATA_DIR_ENV`],
/// else [`LEGACY_DATA_DIR_ENV`] (see [`env_var_os`]).
fn data_dir_env() -> Option<std::ffi::OsString> {
    env_var_os(MNEMOS_DATA_DIR_ENV, LEGACY_DATA_DIR_ENV)
}

/// Redirect every mnemos-ide-* data path (`projects.json`, `workspace_state.json`,
/// ...) to `dir` for the rest of this process. First call wins; a later call
/// with a different directory returns `Err` carrying the directory already in
/// force. App tests use this so `Workspace::new` seeds from an empty project
/// store instead of the developer's real `%APPDATA%\Mnemos\store`.
pub fn override_data_dir(dir: PathBuf) -> Result<(), PathBuf> {
    match DATA_DIR_OVERRIDE.set(dir.clone()) {
        Ok(()) => Ok(()),
        Err(_) => {
            let existing = DATA_DIR_OVERRIDE.get().cloned().unwrap_or_default();
            if existing == dir {
                Ok(())
            } else {
                Err(existing)
            }
        }
    }
}

/// Cross-platform Mnemos user-data directory. Created on first call.
///
/// - macOS:   `~/Library/Application Support/Mnemos/`
/// - Linux:   `~/.local/share/Mnemos/`
/// - Windows: `%APPDATA%\Mnemos\`
///
/// Earlier installs used `twincurrent-build-ui` (v2 pre-rename) and
/// `TwinCurrent Build` (v1 Electron). Both are migrated in-place on first
/// run by `ProjectStore::load` — see `ide_legacy_v2_projects_file()` and
/// `ide_v1_projects_file()`.
///
/// Resolution order: [`override_data_dir`] (in-process), then the
/// [`MNEMOS_DATA_DIR_ENV`] environment variable (or its legacy name
/// [`LEGACY_DATA_DIR_ENV`]), then the platform default.
pub fn ide_data_dir() -> Result<PathBuf, IdeError> {
    let dir = resolve_data_dir()?;
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// [`ide_data_dir`] without creating it, so the tests can check where it
/// points without touching the real `%APPDATA%\Mnemos`.
fn resolve_data_dir() -> Result<PathBuf, IdeError> {
    Ok(match DATA_DIR_OVERRIDE.get() {
        Some(dir) => dir.clone(),
        None => match data_dir_env() {
            Some(env_dir) => PathBuf::from(env_dir),
            None => default_data_dir()?,
        },
    })
}

/// The platform default data dir, `<data dir>/Mnemos`, ignoring every
/// override. Split out so tests can check it whatever the environment says.
fn default_data_dir() -> Result<PathBuf, IdeError> {
    Ok(dirs::data_dir()
        .ok_or(IdeError::MissingDataDir)?
        .join("Mnemos"))
}

fn ide_store_dir() -> Result<PathBuf, IdeError> {
    let dir = ide_data_dir()?.join("store");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Path to the v1-compatible `projects.json`. Parent directory is created.
pub fn ide_projects_file() -> Result<PathBuf, IdeError> {
    Ok(ide_store_dir()?.join("projects.json"))
}

/// Path to the v1-compatible `settings.json`. Parent directory is created.
pub fn ide_settings_file() -> Result<PathBuf, IdeError> {
    Ok(ide_store_dir()?.join("settings.json"))
}

/// Path to the local-only `workspace_state.json`. Holds per-machine UI state
/// (open tabs, active tab, tab order, sync opt-outs). Deliberately NOT in the
/// synced set: each machine keeps its own tab list, and the set of projects
/// open on every machine is the synced `open` flag on each `projects.json`
/// record instead.
///
/// Builds before 2026-09-24 pushed this file as the `mnemos-workspace-state`
/// sync key, so one machine's tab list overwrote the other's. mnemos-ide-sync no
/// longer pushes that key and ignores it on pull (older builds may still
/// push it).
///
/// Parent directory is created.
pub fn ide_workspace_state_file() -> Result<PathBuf, IdeError> {
    Ok(ide_store_dir()?.join("workspace_state.json"))
}

/// Path to the local-only `agent_sessions.json`. Holds the pane-uuid →
/// Claude-session-id map written at shutdown so `mnemos-ide-agent-restore` can resume
/// those sessions on the next launch. Local-only for the same reason
/// `workspace_state.json` is: it describes panes on *this* machine, and the
/// pane uuids it keys on only mean anything against this machine's SQLite
/// workspace snapshot.
///
/// Parent directory is created.
pub fn ide_agent_sessions_file() -> Result<PathBuf, IdeError> {
    Ok(ide_store_dir()?.join("agent_sessions.json"))
}

/// The team sessions a coordinator spawned, by pane uuid, so their names and
/// worktrees survive an app restart (`app/src/workspace/team_control.rs`).
pub fn ide_team_sessions_file() -> Result<PathBuf, IdeError> {
    Ok(ide_store_dir()?.join("team_sessions.json"))
}

/// True when [`ide_data_dir`] is redirected away from the platform default,
/// by [`override_data_dir`], [`MNEMOS_DATA_DIR_ENV`] or [`LEGACY_DATA_DIR_ENV`].
/// Both variable names count, so a build started with only the old one still
/// gets its own keyring entry and store. A redirected process is
/// an isolated install: the one-time legacy seed sources below live at fixed
/// locations outside any data dir and must not leak into it, and mnemos-ide-sync
/// keeps its admin login under a separate keyring entry so it can never read
/// or replace the daily-driver one.
///
/// Public so every isolation decision asks this one function instead of
/// re-reading the environment with its own idea of what counts as set.
pub fn data_dir_is_redirected() -> bool {
    DATA_DIR_OVERRIDE.get().is_some()
        || data_dir_env().is_some()
}

/// Environment variable naming a Mnemos data profile: a dev instance that
/// keeps its own settings and state apart from the daily-driver install.
pub const MNEMOS_DATA_PROFILE_ENV: &str = "MNEMOS_DATA_PROFILE";

/// The data profile this process runs under ([`MNEMOS_DATA_PROFILE_ENV`]),
/// or `None` when it is unset or empty.
///
/// Unlike `mnemos_core::channel::ChannelState::data_profile`, this is honoured
/// in release builds too. Its callers use it to stay away from the default
/// install's state, and a stray variable on a release build then costs at
/// most a separate login, while ignoring it would let a build meant to be
/// isolated share the real one. A non-UTF-8 value is kept (lossily) rather
/// than dropped for the same reason.
pub fn data_profile() -> Option<String> {
    profile_from_env_value(std::env::var_os(MNEMOS_DATA_PROFILE_ENV))
}

/// [`data_profile`] for a given variable value, testable without mutating the
/// process environment under parallel tests.
fn profile_from_env_value(value: Option<std::ffi::OsString>) -> Option<String> {
    value
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string_lossy().into_owned())
}

/// The data profile a debug build defaults to when [`MNEMOS_DATA_PROFILE_ENV`]
/// is not set at all. See [`apply_dev_isolation_defaults`].
pub const DEFAULT_DEV_PROFILE: &str = "dev";

/// What [`apply_dev_isolation_defaults`] exports: each field is `Some` only
/// when that variable was missing and gets a default.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DevIsolation {
    /// Value for [`MNEMOS_DATA_PROFILE_ENV`].
    pub profile: Option<String>,
    /// Value for [`MNEMOS_DATA_DIR_ENV`].
    pub data_dir: Option<PathBuf>,
}

/// The rule behind [`apply_dev_isolation_defaults`], apart from the process
/// environment so tests can cover it.
///
/// - `profile_env` unset: the profile defaults to [`DEFAULT_DEV_PROFILE`].
///   Set but empty: the caller opted out, nothing is defaulted.
/// - `data_dir_env` (the new or legacy name, see [`env_var_os`]) unset or
///   empty, with a profile in force: the Mnemos IDE store moves to
///   `<data_base>/Mnemos-<profile>`, the same dir `script/run` picks, because
///   the store has to move exactly when the profile moves the app's SQLite
///   workspace snapshot (its pane uuids only mean something against it).
pub fn plan_dev_isolation(
    profile_env: Option<std::ffi::OsString>,
    data_dir_env: Option<std::ffi::OsString>,
    data_base: Option<PathBuf>,
) -> DevIsolation {
    let (profile, set_profile) = match profile_env {
        None => (DEFAULT_DEV_PROFILE.to_owned(), true),
        Some(v) if v.is_empty() => return DevIsolation::default(),
        Some(v) => (v.to_string_lossy().into_owned(), false),
    };
    let data_dir = match (data_dir_env.filter(|v| !v.is_empty()), data_base) {
        (None, Some(base)) => Some(base.join(format!("Mnemos-{profile}"))),
        _ => None,
    };
    DevIsolation {
        profile: set_profile.then_some(profile),
        data_dir,
    }
}

/// Isolates a dev build from the installed app without `script/run`: when
/// [`MNEMOS_DATA_PROFILE_ENV`] is unset, sets it to [`DEFAULT_DEV_PROFILE`],
/// and when no data dir is set, points [`MNEMOS_DATA_DIR_ENV`] at
/// `<data dir>/Mnemos-<profile>` (see [`plan_dev_isolation`]). The profile
/// gives the build its own upstream state dir, config dir and sync login
/// (`tcbv2-<profile>`, an existing keyring service name). Opt out with an
/// empty `MNEMOS_DATA_PROFILE=`.
///
/// The app binary calls this first thing in `main` for debug, non-bundle
/// builds only: it mutates the process environment, so it must run before any
/// thread starts or anything reads these variables. Child processes (shells in
/// the terminal) inherit the values, as they do under `script/run`.
pub fn apply_dev_isolation_defaults() -> DevIsolation {
    let plan = plan_dev_isolation(
        std::env::var_os(MNEMOS_DATA_PROFILE_ENV),
        data_dir_env(),
        dirs::data_dir(),
    );
    if let Some(profile) = &plan.profile {
        std::env::set_var(MNEMOS_DATA_PROFILE_ENV, profile);
    }
    if let Some(dir) = &plan.data_dir {
        std::env::set_var(MNEMOS_DATA_DIR_ENV, dir);
    }
    plan
}

/// Path to the **old TCB v1 (Electron)** `projects.json`, used as a one-time
/// seed source when this Mnemos install starts up empty. Returns `None` if
/// the OS has no config dir, or when the data dir is redirected (see
/// [`override_data_dir`]).
///
/// Electron's `app.getPath('userData')` resolves to a `productName`-derived
/// directory:
/// - macOS:   `~/Library/Application Support/TwinCurrent Build/store/projects.json`
/// - Linux:   `~/.config/TwinCurrent Build/store/projects.json`
/// - Windows: `%APPDATA%\TwinCurrent Build\store\projects.json`
///
/// `dirs::config_dir()` returns the right base on all three platforms.
/// This function does NOT create any directories — it only returns the path.
pub fn ide_v1_projects_file() -> Option<PathBuf> {
    if data_dir_is_redirected() {
        return None;
    }
    let base = dirs::config_dir()?;
    Some(
        base.join("TwinCurrent Build")
            .join("store")
            .join("projects.json"),
    )
}

/// Path to the **pre-rename v2** `projects.json` (when the data dir was named
/// `twincurrent-build-ui` rather than `Mnemos`). Used as a one-time seed
/// source when the Mnemos data dir is empty. Returns `None` if the OS has no
/// data dir, or when the data dir is redirected (see [`override_data_dir`]).
///
/// - macOS:   `~/Library/Application Support/twincurrent-build-ui/store/projects.json`
/// - Linux:   `~/.local/share/twincurrent-build-ui/store/projects.json`
/// - Windows: `%APPDATA%\twincurrent-build-ui\store\projects.json`
///
/// This function does NOT create any directories — it only returns the path.
pub fn ide_legacy_v2_projects_file() -> Option<PathBuf> {
    if data_dir_is_redirected() {
        return None;
    }
    let base = dirs::data_dir()?;
    Some(
        base.join("twincurrent-build-ui")
            .join("store")
            .join("projects.json"),
    )
}

/// Claude Code's directory name under `~/.claude/projects/` for a checkout at
/// `root` ([`registry::claude_project_slug`]), with trailing separators
/// trimmed first: Claude Code names the dir after the process cwd, which never
/// ends in one, while a picked or hand-entered path might.
///
/// `None` when the name would be too long for Claude Code to use.
pub fn claude_project_dir_name(root: &Path) -> Option<String> {
    let raw = root.to_string_lossy();
    let trimmed = raw.trim_end_matches(std::path::is_separator);
    let trimmed = if trimmed.is_empty() {
        raw.as_ref()
    } else {
        trimmed
    };
    registry::claude_project_slug(Path::new(trimmed))
}

/// `~/.claude/projects/<slug>/memory/` for the checkout at `repo_root` on
/// this machine, `<slug>` being the directory name Claude Code itself uses
/// ([`claude_project_dir_name`]: every char outside `[A-Za-z0-9]` becomes
/// `-`, the leading dash kept).
///
/// Pass this machine's checkout, never the path another machine registered:
/// that names a directory Claude Code here never reads. Errors with
/// `InvalidProjectId` when the path is too long for Claude Code to use.
pub fn ide_claude_memory_dir(repo_root: impl AsRef<Path>) -> Result<PathBuf, IdeError> {
    let home = dirs::home_dir().ok_or(IdeError::MissingDataDir)?;
    let root = repo_root.as_ref();
    let slug = claude_project_dir_name(root)
        .ok_or_else(|| IdeError::InvalidProjectId(root.to_string_lossy().into_owned()))?;
    Ok(home
        .join(".claude")
        .join("projects")
        .join(slug)
        .join("memory"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_new_name_wins_over_legacy() {
        let pick = |n: Option<&str>, l: Option<&str>| {
            pick_env(n.map(Into::into), l.map(Into::into)).map(|v| v.into_string().unwrap())
        };
        assert_eq!(pick(Some("new"), Some("old")).as_deref(), Some("new"));
        assert_eq!(pick(Some("new"), None).as_deref(), Some("new"));
        // The old name is still honoured when the new one is unset...
        assert_eq!(pick(None, Some("old")).as_deref(), Some("old"));
        // ...or empty, which counts as unset for both names.
        assert_eq!(pick(Some(""), Some("old")).as_deref(), Some("old"));
        assert_eq!(pick(Some("new"), Some("")).as_deref(), Some("new"));
        assert_eq!(pick(Some(""), Some("")), None);
        assert_eq!(pick(None, None), None);
    }

    #[test]
    fn data_dir_env_names_are_pinned() {
        // Scripts, shells and older binaries set these; renaming either one
        // silently un-isolates a dev build.
        assert_eq!(MNEMOS_DATA_DIR_ENV, "MNEMOS_DATA_DIR");
        assert_eq!(LEGACY_DATA_DIR_ENV, "TCB_DATA_DIR");
    }

    #[test]
    fn claude_dir_name_matches_claude_code() {
        // `_` and `.` are not kept (the old v1 encoder kept them, so memory
        // for these checkouts went to a directory Claude Code never reads).
        assert_eq!(
            claude_project_dir_name(Path::new(r"C:\code\Ledger_App")).as_deref(),
            Some("C--code-Ledger-App")
        );
        assert_eq!(
            claude_project_dir_name(Path::new(
                "/Users/alice/code/Admin/.claude/apps/TwinCurrent-Build"
            ))
            .as_deref(),
            Some("-Users-alice-code-Admin--claude-apps-TwinCurrent-Build")
        );
    }

    #[test]
    fn claude_dir_name_ignores_trailing_separators() {
        assert_eq!(
            claude_project_dir_name(Path::new("/Users/alice/code/TCB/")).as_deref(),
            Some("-Users-alice-code-TCB")
        );
        assert_eq!(claude_project_dir_name(Path::new("")), None);
    }

    #[test]
    fn memory_dir_uses_the_claude_slug() {
        let dir = ide_claude_memory_dir(r"C:\code\Ledger_App").unwrap();
        assert!(dir.ends_with(
            Path::new(".claude")
                .join("projects")
                .join("C--code-Ledger-App")
                .join("memory")
        ));
        let too_long = format!("/{}", "a".repeat(300));
        assert!(matches!(
            ide_claude_memory_dir(too_long),
            Err(IdeError::InvalidProjectId(_))
        ));
    }

    #[test]
    fn data_profile_ignores_an_empty_value() {
        assert_eq!(profile_from_env_value(None), None);
        assert_eq!(profile_from_env_value(Some("".into())), None);
        assert_eq!(
            profile_from_env_value(Some("dev".into())).as_deref(),
            Some("dev")
        );
        // Whitespace is still a profile: the caller asked for isolation.
        assert_eq!(
            profile_from_env_value(Some(" ".into())).as_deref(),
            Some(" ")
        );
    }

    #[test]
    fn dev_isolation_defaults_profile_and_store() {
        let base = PathBuf::from("/data");
        assert_eq!(
            plan_dev_isolation(None, None, Some(base.clone())),
            DevIsolation {
                profile: Some("dev".into()),
                data_dir: Some(base.join("Mnemos-dev")),
            }
        );
        // An explicit profile is kept, and the store follows it.
        assert_eq!(
            plan_dev_isolation(Some("qa".into()), None, Some(base.clone())),
            DevIsolation {
                profile: None,
                data_dir: Some(base.join("Mnemos-qa")),
            }
        );
        // An explicit data dir always wins; an empty one counts as unset.
        assert_eq!(
            plan_dev_isolation(None, Some("/x".into()), Some(base.clone())),
            DevIsolation {
                profile: Some("dev".into()),
                data_dir: None,
            }
        );
        assert_eq!(
            plan_dev_isolation(None, Some("".into()), Some(base.clone())).data_dir,
            Some(base.join("Mnemos-dev"))
        );
        // `MNEMOS_DATA_PROFILE=` opts out of both.
        assert_eq!(
            plan_dev_isolation(Some("".into()), None, Some(base)),
            DevIsolation::default()
        );
    }

    #[test]
    fn data_dir_ends_with_mnemos() {
        // The default, not resolve_data_dir(): script/run and dev shells set
        // MNEMOS_DATA_DIR, which would make this depend on the environment.
        let dir = default_data_dir().expect("default_data_dir");
        assert_eq!(
            dir.file_name().and_then(|s| s.to_str()),
            Some("Mnemos"),
            "ide_data_dir() should end in /Mnemos, got {:?}",
            dir
        );
    }

    #[test]
    fn legacy_v2_projects_file_points_at_old_dir_name() {
        // A redirected data dir (MNEMOS_DATA_DIR in a dev shell) disables the
        // legacy seed on purpose; that is covered by the function's contract.
        if data_dir_is_redirected() {
            return;
        }
        let p = ide_legacy_v2_projects_file().expect("legacy v2 path");
        let s = p.to_string_lossy();
        assert!(
            s.contains("twincurrent-build-ui"),
            "legacy v2 path should contain twincurrent-build-ui, got {s}"
        );
        assert!(
            s.ends_with("projects.json"),
            "legacy v2 path should end with projects.json, got {s}"
        );
    }
}

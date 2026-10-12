use std::fs;

use super::{
    check_installed, file_ingest_patched_contents, install_state_in, installed_version,
    marketplace_registered, mnemos_agents_predates_token_in, ClaudeCodePluginManager,
    ClaudePluginInstallState, CliAgentPluginManager, FILE_INGEST_APPEND_LINE,
    FILE_INGEST_PATCH_BLOCK, INSTALL_MARKETPLACE_COMMAND, LEGACY_FILE_INGEST_APPEND_LINE,
    UPDATE_INSTRUCTIONS,
};

#[test]
fn installed_when_plugin_present() {
    let dir = tempfile::tempdir().unwrap();
    let plugins_dir = dir.path().join("plugins");
    fs::create_dir_all(&plugins_dir).unwrap();

    let json = serde_json::json!({
        "plugins": {
            "mnemos-agents@mnemos": [{"version": "1.0.0"}]
        }
    });
    fs::write(
        plugins_dir.join("installed_plugins.json"),
        serde_json::to_string(&json).unwrap(),
    )
    .unwrap();

    assert!(check_installed(dir.path()));
}

#[test]
fn not_installed_when_plugin_key_absent() {
    let dir = tempfile::tempdir().unwrap();
    let plugins_dir = dir.path().join("plugins");
    fs::create_dir_all(&plugins_dir).unwrap();

    let json = serde_json::json!({
        "plugins": {
            "some-other-plugin": [{"version": "1.0.0"}]
        }
    });
    fs::write(
        plugins_dir.join("installed_plugins.json"),
        serde_json::to_string(&json).unwrap(),
    )
    .unwrap();

    assert!(!check_installed(dir.path()));
}

#[test]
fn not_installed_when_plugin_array_empty() {
    let dir = tempfile::tempdir().unwrap();
    let plugins_dir = dir.path().join("plugins");
    fs::create_dir_all(&plugins_dir).unwrap();

    let json = serde_json::json!({
        "plugins": {
            "mnemos-agents@mnemos": []
        }
    });
    fs::write(
        plugins_dir.join("installed_plugins.json"),
        serde_json::to_string(&json).unwrap(),
    )
    .unwrap();

    assert!(!check_installed(dir.path()));
}

#[test]
fn not_installed_when_file_missing() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!check_installed(dir.path()));
}

#[test]
fn not_installed_when_json_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let plugins_dir = dir.path().join("plugins");
    fs::create_dir_all(&plugins_dir).unwrap();
    fs::write(plugins_dir.join("installed_plugins.json"), "not json").unwrap();

    assert!(!check_installed(dir.path()));
}

#[test]
fn not_installed_when_plugins_key_missing() {
    let dir = tempfile::tempdir().unwrap();
    let plugins_dir = dir.path().join("plugins");
    fs::create_dir_all(&plugins_dir).unwrap();

    let json = serde_json::json!({"other_key": "value"});
    fs::write(
        plugins_dir.join("installed_plugins.json"),
        serde_json::to_string(&json).unwrap(),
    )
    .unwrap();

    assert!(!check_installed(dir.path()));
}

/// Tests `ClaudeCodePluginManager::is_installed` end-to-end by pointing
/// `CLAUDE_HOME` at a temp directory with a valid installed_plugins.json.
#[test]
#[serial_test::serial]
fn is_installed_via_trait_with_claude_home_env() {
    let dir = tempfile::tempdir().unwrap();
    let plugins_dir = dir.path().join("plugins");
    fs::create_dir_all(&plugins_dir).unwrap();

    let json = serde_json::json!({
        "plugins": {
            "mnemos-agents@mnemos": [{"version": "1.0.0"}]
        }
    });
    fs::write(
        plugins_dir.join("installed_plugins.json"),
        serde_json::to_string(&json).unwrap(),
    )
    .unwrap();

    std::env::set_var("CLAUDE_HOME", dir.path());
    let result = ClaudeCodePluginManager::new(None, None, None).is_installed();
    std::env::remove_var("CLAUDE_HOME");

    assert!(result);
}

#[test]
#[serial_test::serial]
fn not_installed_via_trait_when_claude_home_empty() {
    let dir = tempfile::tempdir().unwrap();

    std::env::set_var("CLAUDE_HOME", dir.path());
    let result = ClaudeCodePluginManager::new(None, None, None).is_installed();
    std::env::remove_var("CLAUDE_HOME");

    assert!(!result);
}

#[test]
fn can_auto_install_is_true() {
    assert!(ClaudeCodePluginManager::new(None, None, None).can_auto_install());
}

#[test]
fn minimum_version() {
    assert_eq!(
        ClaudeCodePluginManager::new(None, None, None).minimum_plugin_version(),
        "2.0.0"
    );
}

#[test]
fn installed_version_returns_version_when_present() {
    let dir = tempfile::tempdir().unwrap();
    let plugins_dir = dir.path().join("plugins");
    fs::create_dir_all(&plugins_dir).unwrap();

    let json = serde_json::json!({
        "plugins": {
            "mnemos-agents@mnemos": [{"version": "1.5.0"}]
        }
    });
    fs::write(
        plugins_dir.join("installed_plugins.json"),
        serde_json::to_string(&json).unwrap(),
    )
    .unwrap();

    assert_eq!(installed_version(dir.path()).as_deref(), Some("1.5.0"));
}

#[test]
fn installed_version_returns_none_when_no_version_field() {
    let dir = tempfile::tempdir().unwrap();
    let plugins_dir = dir.path().join("plugins");
    fs::create_dir_all(&plugins_dir).unwrap();

    let json = serde_json::json!({
        "plugins": {
            "mnemos-agents@mnemos": [{"scope": "user"}]
        }
    });
    fs::write(
        plugins_dir.join("installed_plugins.json"),
        serde_json::to_string(&json).unwrap(),
    )
    .unwrap();

    assert_eq!(installed_version(dir.path()), None);
}

#[test]
fn installed_version_returns_none_when_file_missing() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(installed_version(dir.path()), None);
}

// Mnemos IDE: the file-ingest patch writes under umask 077 and upgrades the old block.
#[test]
fn file_ingest_patch_appends_umask_block_once() {
    let script = "#!/bin/bash\necho hi";
    let patched = file_ingest_patched_contents(script).unwrap();
    assert!(patched.ends_with(FILE_INGEST_PATCH_BLOCK));
    assert!(patched.contains("umask 077"));
    assert_eq!(file_ingest_patched_contents(&patched), None);
}

#[test]
fn file_ingest_patch_upgrades_legacy_append_line() {
    // Both the app's old block (no indent) and the installer's (indented).
    for indent in ["", "    "] {
        let script = format!(
            "#!/bin/bash\n# === MNEMOS file-ingest (Option D) ===\nif [ x ]; then\n{indent}{LEGACY_FILE_INGEST_APPEND_LINE}\nfi\n"
        );
        let patched = file_ingest_patched_contents(&script).unwrap();
        assert!(!patched.contains(LEGACY_FILE_INGEST_APPEND_LINE));
        assert!(patched.contains(FILE_INGEST_APPEND_LINE));
        assert_eq!(patched.matches("MNEMOS file-ingest").count(), 1);
        assert_eq!(file_ingest_patched_contents(&patched), None);
    }
}

#[test]
fn file_ingest_block_uses_the_umask_line() {
    assert!(FILE_INGEST_PATCH_BLOCK.contains(FILE_INGEST_APPEND_LINE));
}

fn write_installed_plugins(dir: &std::path::Path, plugins: serde_json::Value) {
    let plugins_dir = dir.join("plugins");
    fs::create_dir_all(&plugins_dir).unwrap();
    fs::write(
        plugins_dir.join("installed_plugins.json"),
        serde_json::to_string(&serde_json::json!({ "plugins": plugins })).unwrap(),
    )
    .unwrap();
}

/// A machine that already has upstream Warp's plugin is not told to install a second one.
#[test]
fn upstream_warp_plugin_still_counts_as_installed() {
    let dir = tempfile::tempdir().unwrap();
    write_installed_plugins(
        dir.path(),
        serde_json::json!({ "warp@claude-code-warp": [{"version": "2.0.0"}] }),
    );
    assert!(check_installed(dir.path()));
    assert_eq!(
        install_state_in(dir.path()),
        ClaudePluginInstallState::Current
    );
}

#[test]
fn install_state_checks_each_plugin_against_its_own_minimum() {
    let dir = tempfile::tempdir().unwrap();
    write_installed_plugins(
        dir.path(),
        serde_json::json!({ "mnemos-agents@mnemos": [{"version": "1.2.0"}] }),
    );
    assert_eq!(
        install_state_in(dir.path()),
        ClaudePluginInstallState::Current
    );

    write_installed_plugins(
        dir.path(),
        serde_json::json!({ "mnemos-agents@mnemos": [{"version": "1.1.0"}] }),
    );
    assert_eq!(
        install_state_in(dir.path()),
        ClaudePluginInstallState::Outdated {
            found: "1.1.0".to_owned()
        }
    );

    write_installed_plugins(
        dir.path(),
        serde_json::json!({ "warp@claude-code-warp": [{"version": "1.5.0"}] }),
    );
    assert_eq!(
        install_state_in(dir.path()),
        ClaudePluginInstallState::Outdated {
            found: "1.5.0".to_owned()
        }
    );
}

#[test]
fn install_state_missing_without_either_plugin() {
    let dir = tempfile::tempdir().unwrap();
    write_installed_plugins(
        dir.path(),
        serde_json::json!({ "some-other-plugin": [{"version": "9.0.0"}] }),
    );
    assert_eq!(
        install_state_in(dir.path()),
        ClaudePluginInstallState::Missing
    );
}

/// The marketplace comes from Mnemos's own public repository, never Warp's.
#[test]
fn install_command_points_at_the_mnemos_repository() {
    assert!(INSTALL_MARKETPLACE_COMMAND.contains(env!("MNEMOS_SOURCE_URL")));
    assert!(!INSTALL_MARKETPLACE_COMMAND.to_lowercase().contains("warp"));
}

/// The repository root carries the `mnemos` marketplace that the install command adds; it must
/// list the same plugin and version as `ide/claude-plugin`, whose version is the one to bump.
#[test]
fn root_marketplace_matches_the_in_repo_plugin() {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let read = |path: &str| -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(repo.join(path)).unwrap()).unwrap()
    };
    let root = read(".claude-plugin/marketplace.json");
    let local = read("ide/claude-plugin/.claude-plugin/marketplace.json");
    let plugin = read("ide/claude-plugin/plugins/mnemos-agents/.claude-plugin/plugin.json");

    assert_eq!(root["name"], "mnemos");
    let root_entry = &root["plugins"][0];
    assert_eq!(root_entry["name"], "mnemos-agents");
    assert_eq!(
        root_entry["source"],
        "./ide/claude-plugin/plugins/mnemos-agents"
    );
    assert_eq!(root_entry["version"], local["plugins"][0]["version"]);
    assert_eq!(root_entry["version"], plugin["version"]);
    let version = plugin["version"].as_str().unwrap();
    assert!(super::compare_versions(version, super::MINIMUM_INSTALLED_VERSION).is_ge());
}

/// Mnemos IDE: only an installed mnemos-agents below 1.2.0 (or without a version) counts as
/// predating the token; Warp's plugin alone, a current mnemos-agents or none do not.
#[test]
fn mnemos_agents_predates_token_only_below_the_minimum() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!mnemos_agents_predates_token_in(dir.path()));

    write_installed_plugins(
        dir.path(),
        serde_json::json!({ "mnemos-agents@mnemos": [{"version": "1.1.0"}] }),
    );
    assert!(mnemos_agents_predates_token_in(dir.path()));

    write_installed_plugins(
        dir.path(),
        serde_json::json!({ "mnemos-agents@mnemos": [{"scope": "user"}] }),
    );
    assert!(mnemos_agents_predates_token_in(dir.path()));

    write_installed_plugins(
        dir.path(),
        serde_json::json!({ "mnemos-agents@mnemos": [{"version": "1.2.0"}] }),
    );
    assert!(!mnemos_agents_predates_token_in(dir.path()));

    write_installed_plugins(
        dir.path(),
        serde_json::json!({ "warp@claude-code-warp": [{"version": "1.5.0"}] }),
    );
    assert!(!mnemos_agents_predates_token_in(dir.path()));
}

#[test]
fn marketplace_registered_reads_known_marketplaces() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!marketplace_registered(dir.path(), "mnemos"));
    let plugins = dir.path().join("plugins");
    fs::create_dir_all(&plugins).unwrap();
    fs::write(
        plugins.join("known_marketplaces.json"),
        serde_json::json!({
            "mnemos": {"source": {"source": "directory", "path": "/somewhere/ide/claude-plugin"}},
            "claude-code-warp": {"source": {"source": "github", "repo": "warpdotdev/claude-code-warp"}}
        })
        .to_string(),
    )
    .unwrap();
    assert!(marketplace_registered(dir.path(), "mnemos"));
    assert!(!marketplace_registered(dir.path(), "other"));
}

/// Mnemos IDE: the manual update steps refresh the marketplace in place. Removing it first
/// lost a machine's local-directory marketplace whenever the re-add failed.
#[test]
fn update_instructions_never_remove_the_marketplace() {
    for step in UPDATE_INSTRUCTIONS.steps {
        assert!(
            !step.command.contains("marketplace remove"),
            "update step removes the marketplace: {}",
            step.command
        );
    }
    assert!(UPDATE_INSTRUCTIONS
        .steps
        .iter()
        .any(|step| step.command == "claude plugin marketplace update mnemos"));
}

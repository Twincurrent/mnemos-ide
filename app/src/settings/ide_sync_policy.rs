//! Mnemos IDE: which `settings.toml` paths mnemos-ide-sync syncs, and how.
//!
//! Built from the settings registry (`SettingSchemaEntry`, one per setting
//! with a TOML path): `SyncToCloud::Globally` syncs one value for every
//! machine, `PerPlatform` one per platform, `Never` stays here. The sync
//! crate also applies its own deny list on top (startup shells, Bedrock,
//! agent execution rights), whatever this says.

use std::collections::HashMap;
use std::sync::Arc;

use mnemos_ide_sync::{SettingsKeyScope, SettingsPolicy};
use settings::schema::SettingSchemaEntry;
use settings::SyncToCloud;

/// The full TOML path of a registry entry (`section.sub.key`).
fn toml_path_of(entry: &SettingSchemaEntry) -> String {
    match entry.hierarchy {
        Some(h) => format!("{h}.{}", entry.storage_key),
        None => entry.storage_key.to_string(),
    }
}

fn scope_of(sync: SyncToCloud) -> SettingsKeyScope {
    match sync {
        SyncToCloud::Globally(_) => SettingsKeyScope::Global,
        SyncToCloud::PerPlatform(_) => SettingsKeyScope::PerPlatform,
        SyncToCloud::Never => SettingsKeyScope::Local,
    }
}

/// The policy for every setting this build registers. A path it does not
/// know (a section, an unknown or removed key) is never synced.
pub fn settings_toml_sync_policy() -> SettingsPolicy {
    let mut scopes: HashMap<String, SettingsKeyScope> = HashMap::new();
    for entry in inventory::iter::<SettingSchemaEntry> {
        // Private settings live outside settings.toml.
        if entry.is_private {
            continue;
        }
        scopes.insert(toml_path_of(entry), scope_of((entry.sync_to_cloud_fn)()));
    }
    Arc::new(move |path: &str| scopes.get(path).copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_local_and_executable_settings_never_sync() {
        let policy = settings_toml_sync_policy();
        for path in [
            "session.startup_shell_override",
            "session.new_session_shell_override",
            "general.default_tab_config_path",
            "agents.mcp_servers.file_based_mcp_enabled",
            "cloud_platform.third_party_api_keys.aws_bedrock_auth_refresh_command",
            "cloud_platform.third_party_api_keys.aws_bedrock_profile",
            "cloud_platform.third_party_api_keys.aws_bedrock_auto_login",
            "cloud_platform.third_party_api_keys.aws_bedrock_credentials_enabled",
        ] {
            assert_eq!(
                mnemos_ide_sync::settings_toml::scope_of(&policy, path),
                Some(SettingsKeyScope::Local),
                "{path} must not sync"
            );
        }
        // The command denylists are global preferences to the settings code, so only the sync
        // deny list keeps them (and secret redaction) on this machine.
        assert!(matches!(
            policy("agents.profiles.agent_mode_command_execution_denylist"),
            Some(SettingsKeyScope::Global)
        ));
        for path in [
            "agents.profiles.agent_mode_command_execution_denylist",
            "agents.mnemos_agent.input.ai_command_denylist",
            "privacy.secret_redaction.enabled",
        ] {
            assert_eq!(
                mnemos_ide_sync::settings_toml::scope_of(&policy, path),
                Some(SettingsKeyScope::Local),
                "{path} must not sync"
            );
        }
    }
}

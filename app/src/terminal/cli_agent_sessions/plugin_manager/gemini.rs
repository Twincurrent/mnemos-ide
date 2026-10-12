use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use async_trait::async_trait;
use serde_json::Value;

use super::{compare_versions, CliAgentPluginManager, PluginInstructionStep, PluginInstructions};

const EXTENSION_NAME: &str = "gemini-warp";

// Keep in sync with the plugin version in warpdotdev/gemini-cli-warp.
const MINIMUM_PLUGIN_VERSION: &str = "1.0.0";

// Mnemos IDE: upstream Warp's extension, pinned to a reviewed commit (version 1.0.0) instead of
// the repository's HEAD, which Warp can change at any time. The repository has no tags. Bump the
// commit and MINIMUM_PLUGIN_VERSION together after reviewing the new code.
macro_rules! pinned_commit {
    () => {
        "953e05bd0cc25a56478056af73a8d8af8ca61131"
    };
}
#[cfg(test)]
const PINNED_COMMIT: &str = pinned_commit!();

pub(super) struct GeminiPluginManager;

#[async_trait]
impl CliAgentPluginManager for GeminiPluginManager {
    fn minimum_plugin_version(&self) -> &'static str {
        MINIMUM_PLUGIN_VERSION
    }

    // Mnemos does not control this extension, so the user installs it themselves and sees
    // Gemini's own install-consent prompt.
    fn can_auto_install(&self) -> bool {
        false
    }

    fn is_installed(&self) -> bool {
        let Ok(extensions_dir) = gemini_extensions_dir() else {
            return false;
        };
        check_installed(&extensions_dir)
    }

    fn needs_update(&self) -> bool {
        let Ok(extensions_dir) = gemini_extensions_dir() else {
            return false;
        };
        match installed_version(&extensions_dir) {
            Some(v) => compare_versions(&v, MINIMUM_PLUGIN_VERSION).is_lt(),
            // No version field means very old or malformed extension.
            None => check_installed(&extensions_dir),
        }
    }

    fn install_instructions(&self) -> &'static PluginInstructions {
        &INSTALL_INSTRUCTIONS
    }

    fn update_instructions(&self) -> &'static PluginInstructions {
        &UPDATE_INSTRUCTIONS
    }
}

static INSTALL_INSTRUCTIONS: LazyLock<PluginInstructions> = LazyLock::new(|| PluginInstructions {
    title: "Install Warp's extension for Gemini CLI",
    subtitle: "Mnemos reads the session events of upstream Warp's Gemini CLI extension. Run the \
               following command, then restart Gemini CLI.",
    steps: &[PluginInstructionStep {
        description: "Install Warp's gemini-cli-warp extension, pinned to a reviewed commit",
        command: concat!(
            "gemini extensions install https://github.com/warpdotdev/gemini-cli-warp --ref ",
            pinned_commit!()
        ),
        executable: true,
        link: None,
    }],
    post_install_notes: &["Restart Gemini CLI to activate the extension."],
});

static UPDATE_INSTRUCTIONS: LazyLock<PluginInstructions> = LazyLock::new(|| PluginInstructions {
    title: "Update Warp's extension for Gemini CLI",
    subtitle: "Reinstall the extension at the reviewed commit, then restart Gemini CLI.",
    steps: &[
        PluginInstructionStep {
            description: "Remove the installed extension",
            command: "gemini extensions uninstall gemini-warp",
            executable: true,
            link: None,
        },
        PluginInstructionStep {
            description: "Install Warp's gemini-cli-warp extension, pinned to a reviewed commit",
            command: concat!(
                "gemini extensions install https://github.com/warpdotdev/gemini-cli-warp --ref ",
                pinned_commit!()
            ),
            executable: true,
            link: None,
        },
    ],
    post_install_notes: &["Restart Gemini CLI to activate the update."],
});

fn check_installed(extensions_dir: &Path) -> bool {
    let manifest_path = extensions_dir
        .join(EXTENSION_NAME)
        .join("gemini-extension.json");
    let Ok(contents) = fs::read_to_string(manifest_path) else {
        return false;
    };
    serde_json::from_str::<Value>(&contents).is_ok()
}

/// Reads the installed version string for the Mnemos extension, if present.
fn installed_version(extensions_dir: &Path) -> Option<String> {
    let manifest_path = extensions_dir
        .join(EXTENSION_NAME)
        .join("gemini-extension.json");
    let contents = fs::read_to_string(manifest_path).ok()?;
    let parsed: Value = serde_json::from_str(&contents).ok()?;
    parsed.get("version")?.as_str().map(|s| s.to_owned())
}

/// Returns the path to `~/.gemini/extensions`.
fn gemini_extensions_dir() -> io::Result<PathBuf> {
    dirs::home_dir()
        .map(|home| home.join(".gemini").join("extensions"))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "could not determine home directory",
            )
        })
}

#[cfg(test)]
#[path = "gemini_tests.rs"]
mod tests;

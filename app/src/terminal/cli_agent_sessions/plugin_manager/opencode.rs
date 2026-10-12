use std::sync::LazyLock;

use async_trait::async_trait;

use super::{CliAgentPluginManager, PluginInstructionStep, PluginInstructions};

// The upstream opencode-warp npm package, pinned by version. OpenCode runs npm plugins on
// start, so an unpinned name would run whatever the scope publishes next. Install and update
// instructions both use the pinned spec; changing the pin also forces OpenCode's per-spec
// plugin cache to re-fetch. Keep MINIMUM_PLUGIN_VERSION equal to the pinned version.
macro_rules! pinned_package {
    () => {
        "@warp-dot-dev/opencode-warp@0.1.5"
    };
}
const MINIMUM_PLUGIN_VERSION: &str = "0.1.5";

pub(super) struct OpenCodePluginManager;

#[async_trait]
impl CliAgentPluginManager for OpenCodePluginManager {
    fn minimum_plugin_version(&self) -> &'static str {
        MINIMUM_PLUGIN_VERSION
    }

    fn can_auto_install(&self) -> bool {
        false
    }

    fn install_instructions(&self) -> &'static PluginInstructions {
        &INSTALL_INSTRUCTIONS
    }

    fn update_instructions(&self) -> &'static PluginInstructions {
        &UPDATE_INSTRUCTIONS
    }
}

static INSTALL_INSTRUCTIONS: LazyLock<PluginInstructions> = LazyLock::new(|| {
    PluginInstructions {
        // Mnemos IDE: this is upstream Warp's plugin; say so.
        title: "Install Warp's plugin for OpenCode",
        subtitle: "Mnemos reads the session events of upstream Warp's opencode-warp plugin. Add it \
                   to your OpenCode configuration, then restart OpenCode.",
        steps: &[
            PluginInstructionStep {
                description: "Open or create your opencode.json. This can be in your project root, or the global config path:",
                command: "~/.config/opencode/opencode.json",
                executable: false,
                link: None,
            },
            PluginInstructionStep {
                description: concat!("Add \"", pinned_package!(), "\" to the \"plugin\" array in the top-level JSON object:"),
                command: concat!("\"plugin\": [\"", pinned_package!(), "\"]"),
                executable: false,
                link: None,
            },
        ],
        post_install_notes: &["Restart OpenCode to activate the plugin."],
    }
});

static UPDATE_INSTRUCTIONS: LazyLock<PluginInstructions> = LazyLock::new(|| {
    PluginInstructions {
        title: "Update Warp's plugin for OpenCode",
        subtitle: "Pin the plugin to the latest version in your opencode.json. OpenCode caches plugins per version spec, so changing the pin forces it to re-fetch on restart.",
        steps: &[
            PluginInstructionStep {
                description: "Open or create your opencode.json. This can be in your project root, or the global config path:",
                command: "~/.config/opencode/opencode.json",
                executable: false,
                link: None,
            },
            PluginInstructionStep {
                description: "Replace the existing \"@warp-dot-dev/opencode-warp\" entry in the \"plugin\" array with the explicit version:",
                command: concat!("\"plugin\": [\"", pinned_package!(), "\"]"),
                executable: false,
                link: None,
            },
        ],
        post_install_notes: &["Restart OpenCode to load the updated plugin."],
    }
});

#[cfg(test)]
#[path = "opencode_tests.rs"]
mod tests;

use serde::{Deserialize, Serialize};

use crate::settings::{AISettings, InputSettings, TerminalSpacing};
use settings::{
    macros::define_settings_group, RespectUserSyncSetting, SupportedPlatforms, SyncToCloud,
};
use mnemos_ui::{units::Pixels, AppContext, SingletonEntity};

#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[schemars(description = "Terminal block spacing.", rename_all = "snake_case")]
pub enum SpacingMode {
    #[default]
    #[schemars(description = "Normal")]
    Normal,
    #[schemars(description = "Compact")]
    Compact,
}

impl SpacingMode {
    pub fn other_mode(&self) -> SpacingMode {
        match *self {
            SpacingMode::Normal => SpacingMode::Compact,
            SpacingMode::Compact => SpacingMode::Normal,
        }
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[schemars(
    description = "How padding is applied in full-screen terminal apps.",
    rename_all = "snake_case"
)]
pub enum AltScreenPaddingMode {
    #[schemars(description = "Use the same padding as the block list.")]
    MatchBlocklist,
    #[schemars(description = "Use a custom uniform padding value.")]
    Custom { uniform_padding: Pixels },
}

impl Default for AltScreenPaddingMode {
    fn default() -> Self {
        Self::Custom {
            uniform_padding: Pixels::zero(),
        }
    }
}

impl AltScreenPaddingMode {
    pub fn toggled(&self) -> Self {
        match self {
            Self::MatchBlocklist => Self::Custom {
                uniform_padding: Pixels::zero(),
            },
            Self::Custom { .. } => Self::MatchBlocklist,
        }
    }

    pub fn telemetry_string(&self) -> String {
        match self {
            Self::MatchBlocklist => "MatchBlocklist",
            Self::Custom { .. } => "Custom",
        }
        .to_string()
    }
}

define_settings_group!(TerminalSettings, settings: [
    use_audible_bell: UseAudibleBell {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::DESKTOP, /* Audible bell is not supported on web */
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        private: false,
        toml_path: "terminal.use_audible_bell",
        description: "Whether to play an audible bell sound on terminal bell events.",
    },
    // Mnemos IDE: OSC 52 clipboard reads hand the clipboard to whatever
    // printed the request (a remote host, a file being `cat`-ed), so they are
    // off unless the user opts in. Never synced: the choice is per machine.
    allow_osc52_clipboard_read: AllowOsc52ClipboardRead {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        private: false,
        toml_path: "terminal.allow_osc52_clipboard_read",
        description: "Whether programs in the terminal may read the clipboard with OSC 52.",
    },
    // Mnemos IDE: the toast for an OSC 52 clipboard write. tmux with
    // `set-clipboard on` and editors set up to yank over OSC 52 write on every
    // yank, so it can be turned off; it is also shown at most once a minute
    // per terminal (see TerminalView).
    notify_osc52_clipboard_write: NotifyOsc52ClipboardWrite {
        type: bool,
        default: true,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        private: false,
        toml_path: "terminal.notify_osc52_clipboard_write",
        description: "Whether to show a notice when a program in the terminal writes the clipboard with OSC 52.",
    },
    spacing_mode: Spacing {
        type: SpacingMode,
        default: SpacingMode::default(),
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        private: false,
        toml_path: "appearance.spacing",
        description: "Controls the spacing between terminal blocks.",
    }
    maximum_grid_size: MaximumGridSize {
        type: usize,
        default: 50_000,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        private: false,
        toml_path: "terminal.maximum_grid_size",
        description: "The maximum number of rows in the terminal grid.",
    },
    // Mnemos IDE: how long finished blocks and commands stay in the local session store.
    stored_history_retention_days: StoredHistoryRetentionDays {
        type: usize,
        default: 0,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        private: false,
        toml_path: "terminal.stored_history_retention_days",
        description: "Days to keep finished blocks (command and output) and command history in the local session store. Older entries are deleted at startup. 0 keeps them until their pane closes or the history is cleared.",
    },
    alt_screen_padding: AltScreenPadding {
        type: AltScreenPaddingMode,
        default: AltScreenPaddingMode::default(),
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        private: false,
        toml_path: "appearance.full_screen_apps.alt_screen_padding",
        max_table_depth: 0,
        description: "Controls padding around full-screen terminal applications.",
    },
    // This field should not be referenced directly to check zero state block visibility -- use
    // the `should_show_zero_state_block()` getter, which also considers global AI enablement.
    show_terminal_zero_state_block: ShowTerminalZeroStateBlock {
        type: bool,
        default: true,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        private: false,
        toml_path: "terminal.show_terminal_zero_state_block",
        description: "Whether to show the AI zero-state block in new terminal sessions.",
    },
]);

impl TerminalSettings {
    /// Spacing for the terminal blocks.
    pub fn terminal_spacing(&self, line_height_ratio: f32, ctx: &AppContext) -> TerminalSpacing {
        match *self.spacing_mode {
            SpacingMode::Normal => TerminalSpacing::normal(line_height_ratio, ctx),
            SpacingMode::Compact => TerminalSpacing::compact(line_height_ratio, ctx),
        }
    }

    /// Whether the terminal zero state block should be shown.
    /// Checks both the user setting and the global AI enablement.
    pub fn should_show_zero_state_block(&self, ctx: &AppContext) -> bool {
        *self.show_terminal_zero_state_block && AISettings::as_ref(ctx).is_any_ai_enabled(ctx)
    }

    /// Spacing for the input box.
    pub fn terminal_input_spacing(
        &self,
        line_height_ratio: f32,
        ctx: &AppContext,
    ) -> TerminalSpacing {
        let should_force_normal_spacing =
            InputSettings::as_ref(ctx).is_universal_developer_input_enabled(ctx);
        if should_force_normal_spacing {
            return TerminalSpacing::normal(line_height_ratio, ctx);
        }
        match *self.spacing_mode {
            SpacingMode::Normal => TerminalSpacing::normal(line_height_ratio, ctx),
            SpacingMode::Compact => TerminalSpacing::compact(line_height_ratio, ctx),
        }
    }
}

#[cfg(test)]
mod tests {
    use settings::Setting as _;

    /// Mnemos IDE: OSC 52 clipboard reads stay off unless the user opts in,
    /// and the choice never syncs to another machine.
    #[test]
    fn osc52_clipboard_reads_are_off_by_default() {
        assert!(!super::AllowOsc52ClipboardRead::default_value());
        assert_eq!(
            super::AllowOsc52ClipboardRead::sync_to_cloud(),
            settings::SyncToCloud::Never
        );
        assert!(super::NotifyOsc52ClipboardWrite::default_value());
    }
}

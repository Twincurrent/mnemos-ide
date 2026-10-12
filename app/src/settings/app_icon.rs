use enum_iterator::Sequence;
use serde::{Deserialize, Serialize};
use mnemos_core::{
    channel::{Channel, ChannelState},
    settings::{macros::define_settings_group, SupportedPlatforms, SyncToCloud},
};

/// The app icon to use (mac-only).
///
/// IMPORTANT NOTE: If you add a new icon, you will need to update the logic in MnemosDockTilePlugin.m
/// to read the new icon and also add the icon to app/DockTilePlugin/Resources.
///
/// Mnemos IDE: every alternate icon upstream shipped is a Warp logo, so they were deleted and
/// only `Default` is selectable. The other variants stay (skipped in the schema) so a saved
/// `app_icon` value still parses: a value that fails to parse inhibits writes to the key. Each
/// of them renders as `Default` (see [`AppIcon::resolved`]).
#[derive(
    Default,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Serialize,
    Deserialize,
    Sequence,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[schemars(
    description = "The app icon displayed in the dock.",
    rename_all = "snake_case"
)]
pub enum AppIcon {
    #[default]
    #[schemars(description = "Default")]
    Default,
    #[schemars(skip)]
    Aurora,
    #[schemars(skip)]
    Classic1,
    #[schemars(skip)]
    Classic2,
    #[schemars(skip)]
    Classic3,
    #[schemars(skip)]
    Comets,
    #[schemars(skip)]
    Cow,
    #[schemars(skip)]
    GlassSky,
    #[schemars(skip)]
    Glitch,
    #[schemars(skip)]
    Glow,
    #[schemars(skip)]
    Holographic,
    #[schemars(skip)]
    Mono,
    #[schemars(skip)]
    Neon,
    #[schemars(skip)]
    Original,
    #[schemars(skip)]
    Starburst,
    #[schemars(skip)]
    Sticker,
    #[schemars(skip)]
    MnemosOne,
}

impl AppIcon {
    /// Whether this is one of upstream's removed alternate icons, kept only so saved values parse.
    pub fn is_legacy(self) -> bool {
        self != AppIcon::Default
    }

    /// The icon to actually show: a legacy value renders as `Default`.
    pub fn resolved(self) -> AppIcon {
        if self.is_legacy() {
            AppIcon::Default
        } else {
            self
        }
    }
}

impl std::fmt::Display for AppIcon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Every value resolves to `Default`, the only icon left.
        write!(f, "Default")
    }
}

impl AppIconSettings {
    /// The dock tile plugin image for `icon`. Every value resolves to `Default` (see
    /// [`AppIcon::resolved`]), whose image depends on the channel.
    pub fn get_base_icon_file_name(_icon: AppIcon) -> &'static str {
        match ChannelState::channel() {
            Channel::Dev => "dev",
            Channel::Preview => "preview",
            Channel::Local => "local",
            _ => "mnemos_2",
        }
    }
}

define_settings_group!(AppIconSettings, settings: [
    app_icon: AppIconState {
        type: AppIcon,
        default: AppIcon::Default,
        supported_platforms: SupportedPlatforms::MAC,
        sync_to_cloud: SyncToCloud::Never,
        private: false,
        storage_key: "AppIcon",
        toml_path: "appearance.icon.app_icon",
        description: "The app icon displayed in the dock.",
    },
]);

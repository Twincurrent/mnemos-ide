use settings::{
    macros::define_settings_group, RespectUserSyncSetting, SupportedPlatforms, SyncToCloud,
};

use super::DriveSortOrder;

pub const HAS_AUTO_OPENED_WELCOME_FOLDER: &str = "HasAutoOpenedWelcomeFolder";

define_settings_group!(MnemosDriveSettings, settings: [
    sorting_choice: MnemosDriveSortingChoice {
        type: DriveSortOrder,
        default: DriveSortOrder::ByObjectType,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        private: false,
        toml_path: "mnemos_drive.sorting_choice",
        description: "The sort order for items in Mnemos Drive.",
    },
    sharing_onboarding_block_shown: MnemosDriveSharingOnboardingBlockShown {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        private: true,
    },
    // Controls whether Mnemos Drive appears in the tools panel, command palette, and command search.
    enable_mnemos_drive: EnableMnemosDrive {
        type: bool,
        default: true,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        private: false,
        toml_path: "mnemos_drive.enabled",
        description: "Whether Mnemos Drive is enabled.",
    },
]);

impl MnemosDriveSettings {
    /// Mnemos IDE: whether the Drive panel itself is offered (tools panel tab,
    /// hotkey, palette entry). Only the user setting decides: the panel's
    /// Mnemos sections (the Claude session picker, sync status) are local and
    /// need no account.
    pub fn is_drive_panel_enabled(app: &mnemos_ui::AppContext) -> bool {
        use mnemos_ui::SingletonEntity as _;
        *Self::as_ref(app).enable_mnemos_drive
    }

    /// Returns whether Mnemos Drive's cloud objects (workflows, notebooks,
    /// env vars, Drive search and context) should be considered enabled.
    /// Returns `false` when the user is anonymous or fully logged out,
    /// regardless of the user setting. For the panel itself use
    /// [`Self::is_drive_panel_enabled`].
    pub fn is_mnemos_drive_enabled(app: &mnemos_ui::AppContext) -> bool {
        use mnemos_ui::SingletonEntity as _;
        let is_anonymous_or_logged_out = crate::auth::AuthStateProvider::as_ref(app)
                .get()
                .is_anonymous_or_logged_out();
        *Self::as_ref(app).enable_mnemos_drive && !is_anonymous_or_logged_out
    }
}

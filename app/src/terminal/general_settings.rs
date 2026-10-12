use std::collections::HashSet;

use crate::{banner::BannerState, resource_center::Tip};
use mnemos_core::settings::{
    macros::define_settings_group, RespectUserSyncSetting, SupportedPlatforms, SyncToCloud,
};

define_settings_group!(GeneralSettings, settings: [
    show_warning_before_quitting: ShowWarningBeforeQuitting {
        type: bool,
        default: true,
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        private: false,
        toml_path: "general.show_warning_before_quitting",
        description: "Whether to show a warning dialog before quitting Mnemos.",
    },
    quit_on_last_window_closed: QuitOnLastWindowClosed {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::MAC,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        private: false,
        toml_path: "general.quit_on_last_window_closed",
        description: "Whether to quit Mnemos when the last window is closed.",
    },
    restore_session: RestoreSession {
        type: bool,
        default: true,
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        private: false,
        toml_path: "general.restore_session",
        description: "Whether to restore the previous session when Mnemos starts up.",
    },
    // Mnemos IDE: off by default; a new install does not add itself to the OS startup
    // list. Installs that registered under the old default keep it through the private
    // `legacy_login_item_default` (see `GeneralSettings::launches_at_login`). Read the
    // effective value with `launches_at_login`, not this setting.
    add_app_as_login_item: LoginItem {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::OR(
            Box::new(SupportedPlatforms::MAC),
            Box::new(SupportedPlatforms::WINDOWS),
        ),
        sync_to_cloud: SyncToCloud::Never,
        private: false,
        toml_path: "general.login_item",
        description: "Whether to launch Mnemos automatically when you log in.",
    },
    // Records whether the app has been added as a login item.
    // If it has, we don't try to add it again unless the user explicitly
    // retoggles the setting. This is to allow a user to remove the login item
    // directly from their OS's startup UI and not have it re-added when they
    // next start Mnemos.
    app_added_as_login_item: AppAddedAsLoginItem {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::OR(
            Box::new(SupportedPlatforms::MAC),
            Box::new(SupportedPlatforms::WINDOWS),
        ),
        sync_to_cloud: SyncToCloud::Never,
        private: true,
    },
    // Mnemos IDE: set once on an install that registered itself at login under the
    // old `general.login_item` default (`true`) and never set that key. It stands in
    // for the unset key so the install keeps launching at login. Private on purpose:
    // writing `general.login_item = true` instead would land in settings.toml, which
    // mnemos-ide-sync uploads verbatim, and register the app at login on every
    // machine that pulls it.
    legacy_login_item_default: LegacyLoginItemDefault {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::OR(
            Box::new(SupportedPlatforms::MAC),
            Box::new(SupportedPlatforms::WINDOWS),
        ),
        sync_to_cloud: SyncToCloud::Never,
        private: true,
    },
    // Mnemos IDE: whether this machine has an admin (mnemos-ide-sync) login that the
    // user has not signed out of. Not a secret: it only tells "the keyring lost a
    // login" (a "Sync signed out" chip) apart from "nobody ever signed in here"
    // (`SyncAuthState::NeverConfigured`, no chip). Set on a successful admin
    // redirect, cleared by `AuthManager::sign_out_of_admin`.
    admin_login_seen: AdminLoginSeen {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        private: true,
    },
    // Mnemos IDE: the live agents feed (`agents-live:<host>`), which publishes
    // this machine's running agents to the sync account so the user's other
    // machines (and Iris) can list them. Off by default. Read the effective
    // value with `GeneralSettings::live_feed_enabled`, not this setting.
    live_feed_enabled: LiveAgentsFeedEnabled {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        private: false,
        toml_path: "sync.live_feed.enabled",
        description: "Whether this machine shares its running agents with your other signed-in machines.",
    },
    // Mnemos IDE: whether the live feed carries the last prompt and reply text
    // (redacted for secrets) instead of status only.
    live_feed_share_text: LiveAgentsFeedShareText {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        private: false,
        toml_path: "sync.live_feed.share_prompt_text",
        description: "Whether the live agents feed includes prompt and reply text (secrets redacted).",
    },
    // Mnemos IDE: the live feed used to be always on. An install that already
    // had an admin login when it first ran a build with the setting keeps it
    // on through this private pin (see `GeneralSettings::live_feed_enabled`),
    // set once by `AuthManager::pin_live_feed_for_existing_login`. Private
    // so it never lands in the synced settings.toml.
    live_feed_legacy_default: LiveAgentsFeedLegacyDefault {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        private: true,
    },
    // Mnemos IDE: the check above has run on this install (whatever it found).
    live_feed_default_checked: LiveAgentsFeedDefaultChecked {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        private: true,
    },
    link_tooltip: LinkTooltip {
        type: bool,
        default: true,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        private: false,
        toml_path: "general.link_tooltip",
        description: "Whether to show a tooltip when hovering over links.",
    },
    welcome_tips_features_used: WelcomeTipsFeaturesUsed {
        type: HashSet<Tip>,
        default: HashSet::new(),
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        private: true,
    },
    welcome_tips_skipped_or_completed: WelcomeTipsCompleted {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        private: true,
    },
    agent_mode_onboarding_block_shown: AgentModeOnboardingBlockShown {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        private: true,
    },
    telemetry_banner_dismissed: TelemetryBannerDismissed {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        private: true,
    },
    user_default_shell_unsupported_banner_state: UserDefaultShellUnsupportedBannerState {
        type: BannerState,
        default: BannerState::default(),
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        private: true,
    },
    open_in_mnemos_banner_dismissed_for_markdown: OpenInMnemosBannerDismissedMarkdown {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        private: true,
    },
    open_in_mnemos_banner_dismissed_for_code_and_text: OpenInMnemosBannerDismissedCode {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        private: true,
    },
    did_non_anonymous_user_log_in: DidNonAnonymousUserLogIn {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        private: true,
    },
    anonymous_user_ai_sign_up_banner_shown: AnonymousUserAISignUpBannerShown {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        private: true,
    },
    auto_open_code_review_pane_on_first_agent_change: AutoOpenCodeReviewPaneOnFirstAgentChange {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        private: false,
        toml_path: "code.editor.auto_open_code_review_pane_on_first_agent_change",
        description: "Whether to automatically open the code review pane when the agent makes its first change.",
    },
    bonus_grants_shown: BonusGrantsShown {
        type: HashSet<String>,
        default: HashSet::new(),
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        private: true,
    },
]);

impl GeneralSettings {
    /// Mnemos IDE: whether Mnemos should launch at login. An explicit
    /// `general.login_item` wins; unset, an install pinned to the old default
    /// (`legacy_login_item_default`) stays on and every other install gets the new
    /// default, off.
    pub fn launches_at_login(&self) -> bool {
        use ::settings::Setting as _;
        if self.add_app_as_login_item.is_value_explicitly_set() {
            *self.add_app_as_login_item
        } else {
            *self.legacy_login_item_default || *self.add_app_as_login_item
        }
    }

    /// Mnemos IDE: whether the live agents feed is on. An explicit
    /// `sync.live_feed.enabled` wins; unset, an install that was signed in
    /// when the setting arrived (`live_feed_legacy_default`) stays on and
    /// every other install gets the default, off.
    pub fn live_feed_enabled(&self) -> bool {
        use ::settings::Setting as _;
        live_feed_effective(
            self.live_feed_enabled
                .is_value_explicitly_set()
                .then_some(*self.live_feed_enabled),
            *self.live_feed_legacy_default,
        )
    }

    /// Mnemos IDE: whether the live feed carries prompt and reply text. An
    /// explicit `sync.live_feed.share_prompt_text` wins; unset, an install
    /// that was signed in when the setting arrived keeps sending the text
    /// it always sent (older machines and Iris read it), every other
    /// install gets the default, off.
    pub fn live_feed_share_text_enabled(&self) -> bool {
        use ::settings::Setting as _;
        live_feed_effective(
            self.live_feed_share_text
                .is_value_explicitly_set()
                .then_some(*self.live_feed_share_text),
            *self.live_feed_legacy_default,
        )
    }
}

/// The live feed's effective state: an explicit choice, else the legacy pin.
fn live_feed_effective(explicit: Option<bool>, legacy_default: bool) -> bool {
    explicit.unwrap_or(legacy_default)
}

#[cfg(test)]
mod live_feed_tests {
    use super::live_feed_effective;

    #[test]
    fn explicit_choice_wins_over_the_legacy_pin() {
        assert!(!live_feed_effective(None, false), "new installs default off");
        assert!(live_feed_effective(None, true), "signed-in installs stay on");
        assert!(!live_feed_effective(Some(false), true), "turning it off sticks");
        assert!(live_feed_effective(Some(true), false));
    }
}

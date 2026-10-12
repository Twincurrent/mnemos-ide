use lazy_static::lazy_static;
use parking_lot::Mutex;
use std::{borrow::Cow, collections::HashSet};
use url::{Origin, ParseError, Url};

use crate::AppId;
use crate::{
    channel::config::{
        ChannelConfig, McpOAuthProviderConfig, OzConfig, RudderStackDestination, MnemosServerConfig,
    },
    features::FeatureFlag,
};

use super::Channel;

lazy_static! {
    static ref CHANNEL_STATE: Mutex<ChannelState> = Mutex::new(ChannelState::init());
}

#[cfg(feature = "test-util")]
lazy_static! {
    // Mnemos IDE: started on its own thread. `Server::new` blocks on a runtime, so the first
    // test to read the URL from inside a tokio runtime (a `#[tokio::test]`) panicked and
    // poisoned this static for every test after it.
    static ref MOCK_SERVER: mockito::ServerGuard = std::thread::spawn(mockito::Server::new)
        .join()
        .expect("the mock server should start");
    static ref MOCK_SERVER_URL: String = MOCK_SERVER.url();
    static ref APP_VERSION: Mutex<Option<&'static str>> = Mutex::new(None);
    // Mnemos IDE: see `ChannelState::unit_test_home_dir`.
    static ref UNIT_TEST_HOME_DIR: std::path::PathBuf =
        std::env::temp_dir().join(format!("mnemos-unit-test-home-{}", std::process::id()));
}

#[derive(Debug)]
pub struct ChannelState {
    channel: Channel,

    /// The set of additional features to enable (on top of default-enabled ones).
    additional_features: HashSet<FeatureFlag>,

    config: ChannelConfig,
}

impl ChannelState {
    pub fn init() -> Self {
        // Mnemos IDE: the state before a binary sets its channel. Upstream defaulted to the
        // retired `Oss` channel; `Local` keeps such state apart from the installed app's.
        let channel = Channel::Local;
        let app_id = AppId::new("dev", "mnemos", "MnemosLocal");
        Self {
            channel,
            additional_features: Default::default(),
            config: ChannelConfig {
                app_id,
                logfile_name: "".into(),
                server_config: MnemosServerConfig::production(),
                oz_config: OzConfig::production(),
                telemetry_config: None,
                autoupdate_config: None,
                crash_reporting_config: None,
                mcp_static_config: None,
            },
        }
    }

    /// Mnemos IDE: the home directory that test builds use in place of the real one while no
    /// binary has picked a channel, i.e. while [`Self::init`]'s `Local` default is in effect.
    ///
    /// Unit tests never set a channel, so without this they read and wrote the real
    /// `~/.mnemos-local` (and the `MnemosLocal` project dirs), and their results depended on
    /// what that machine had there. It is a per-process directory under the system temp dir;
    /// nothing creates it until a test writes there. Integration tests set their own channel
    /// and are unaffected.
    #[cfg(feature = "test-util")]
    pub fn unit_test_home_dir() -> Option<std::path::PathBuf> {
        (Self::channel() == Channel::Local).then(|| UNIT_TEST_HOME_DIR.clone())
    }

    pub fn new(channel: Channel, mut config: ChannelConfig) -> Self {
        if let Some(app_id) = app_id_from_bundle() {
            config.app_id = app_id;
        }
        Self {
            channel,
            additional_features: Default::default(),
            config,
        }
    }

    pub fn with_additional_features(mut self, overrides: &[FeatureFlag]) -> Self {
        self.additional_features.extend(overrides);
        self
    }

    pub fn set(state: ChannelState) {
        *CHANNEL_STATE.lock() = state;
    }

    pub fn is_release_bundle() -> bool {
        cfg!(feature = "release_bundle")
    }

    pub fn enable_debug_features() -> bool {
        cfg!(debug_assertions) || matches!(Self::channel(), Channel::Local | Channel::Dev)
    }

    pub fn override_server_root_url(url: impl Into<Cow<'static, str>>) -> Result<(), ParseError> {
        let url = url.into();
        Url::parse(&url)?;
        CHANNEL_STATE.lock().config.server_config.server_root_url = url;
        Ok(())
    }

    pub fn override_ws_server_url(url: impl Into<Cow<'static, str>>) -> Result<(), ParseError> {
        let url = url.into();
        Url::parse(&url)?;
        CHANNEL_STATE.lock().config.server_config.rtc_server_url = url;
        Ok(())
    }

    pub fn override_session_sharing_server_url(
        url: impl Into<Cow<'static, str>>,
    ) -> Result<(), ParseError> {
        let url = url.into();
        Url::parse(&url)?;
        CHANNEL_STATE
            .lock()
            .config
            .server_config
            .session_sharing_server_url = Some(url);
        Ok(())
    }

    /// Returns the canonical identifier for the application.
    ///
    /// This should not be used for namespacing persisted data - such use cases
    /// should make use of [`Self::data_domain`] instead.
    pub fn app_id() -> AppId {
        CHANNEL_STATE.lock().config.app_id.clone()
    }

    /// Returns a profile name for isolating user data. This should be used to
    /// sandbox how user data is stored.
    ///
    /// This is a debugging tool for isolating development instances of Mnemos, and is not
    /// supported in release builds.
    ///
    /// An empty `MNEMOS_DATA_PROFILE` counts as unset, matching
    /// `mnemos_ide_core::paths::data_profile`, so data dirs and the keyring service agree on it.
    /// Side effect: a debug build started with `MNEMOS_DATA_PROFILE=` now uses the
    /// unprofiled data domain (the one an installed app with the same bundle id uses)
    /// instead of a domain with a trailing "-". This only affects debug builds.
    pub fn data_profile() -> Option<String> {
        if cfg!(debug_assertions) {
            std::env::var("MNEMOS_DATA_PROFILE")
                .ok()
                .filter(|profile| !profile.is_empty())
        } else {
            None
        }
    }

    /// Returns a value that should be used for namespacing persisted data.
    ///
    /// In release builds, this is identical to the app ID; in debug builds,
    /// it optionally includes a suffix derived from the `MNEMOS_DATA_PROFILE`
    /// environment variable.
    pub fn data_domain() -> String {
        match Self::data_profile() {
            Some(profile) => format!("{}-{profile}", Self::app_id()),
            None => Self::app_id().to_string(),
        }
    }

    /// Returns the data domain if overridden from the default, otherwise None.
    pub fn data_domain_if_not_default() -> Option<String> {
        Self::data_profile().map(|_| Self::data_domain())
    }

    pub fn additional_features() -> HashSet<FeatureFlag> {
        CHANNEL_STATE
            .lock()
            .additional_features
            .iter()
            .cloned()
            .collect()
    }

    pub fn debug_str() -> String {
        format!("{:?}", *CHANNEL_STATE.lock())
    }

    /// Returns the name of this process's log file.
    ///
    /// On macOS every build logs to the same directory, so debug builds and data profiles
    /// get their own name to keep them from rotating or overwriting the installed app's
    /// log: `mnemos-<profile>.log` with a data profile, `mnemos-debug.log` for a debug
    /// build without one. Release builds use the configured name unchanged.
    pub fn logfile_name() -> Cow<'static, str> {
        // Clone and drop the guard before anything else runs: the mutex is not reentrant.
        let configured = CHANNEL_STATE.lock().config.logfile_name.clone();
        let tag = Self::data_profile().or_else(|| cfg!(debug_assertions).then(|| "debug".into()));
        match tag {
            Some(tag) if !configured.is_empty() => {
                Cow::Owned(tagged_logfile_name(&configured, Some(&tag)))
            }
            _ => configured,
        }
    }

    pub fn telemetry_file_name() -> Cow<'static, str> {
        CHANNEL_STATE
            .lock()
            .config
            .telemetry_config
            .as_ref()
            .map(|tc| tc.telemetry_file_name.clone())
            .unwrap_or_default()
    }

    /// Returns whether this build has a telemetry config and can therefore ship
    /// telemetry events. Builds like OpenMnemos intentionally ship with
    /// `telemetry_config: None`, in which case UI that controls telemetry
    /// should be hidden since the toggle has no effect.
    ///
    /// Mnemos IDE: also false unless the build enables the non-default `telemetry` feature.
    pub fn is_telemetry_available() -> bool {
        cfg!(feature = "telemetry") && CHANNEL_STATE.lock().config.telemetry_config.is_some()
    }

    /// Returns whether this build has a crash reporting config and can therefore
    /// ship crash reports. Builds like OpenMnemos intentionally ship with
    /// `crash_reporting_config: None`, in which case UI that controls crash
    /// reporting should be hidden since the toggle has no effect.
    pub fn is_crash_reporting_available() -> bool {
        CHANNEL_STATE.lock().config.crash_reporting_config.is_some()
    }

    pub fn releases_base_url() -> Cow<'static, str> {
        CHANNEL_STATE
            .lock()
            .config
            .autoupdate_config
            .as_ref()
            .map(|ac| ac.releases_base_url.clone())
            .unwrap_or_default()
    }

    /// Mnemos IDE: the Terms of Service URL from the channel config, else
    /// [`super::DEFAULT_TERMS_OF_SERVICE_URL`].
    pub fn terms_of_service_url() -> Cow<'static, str> {
        CHANNEL_STATE
            .lock()
            .config
            .server_config
            .terms_of_service_url
            .clone()
            .filter(|url| !url.is_empty())
            .unwrap_or(Cow::Borrowed(super::DEFAULT_TERMS_OF_SERVICE_URL))
    }

    /// Mnemos IDE: the privacy policy URL from the channel config, if it names one. The app
    /// falls back to its own default (`util::links::privacy_policy_url`).
    pub fn privacy_policy_url_override() -> Option<Cow<'static, str>> {
        CHANNEL_STATE
            .lock()
            .config
            .server_config
            .privacy_policy_url
            .clone()
            .filter(|url| !url.is_empty())
    }

    pub fn firebase_api_key() -> Cow<'static, str> {
        CHANNEL_STATE
            .lock()
            .config
            .server_config
            .firebase_auth_api_key
            .clone()
    }

    pub fn ws_server_url() -> Cow<'static, str> {
        CHANNEL_STATE
            .lock()
            .config
            .server_config
            .rtc_server_url
            .clone()
    }

    /// Returns the HTTP(S) root URL for the RTC server. Used for HTTP endpoints
    /// served by mnemos-server-rtc (e.g. the agent event SSE stream).
    ///
    /// Derived from [`ws_server_url`] by rewriting the scheme (`wss`→`https`,
    /// `ws`→`http`) and stripping the path. Falls back to [`server_root_url`]
    /// when the WS URL cannot be parsed or uses an unexpected scheme — this
    /// keeps override paths (e.g. `MNEMOS_WS_SERVER_URL=...`) working without a
    /// separate override for the HTTP variant.
    pub fn rtc_http_url() -> Cow<'static, str> {
        cfg_if::cfg_if! {
            if #[cfg(feature = "test-util")] {
                Cow::Owned(MOCK_SERVER_URL.clone())
            } else {
                match derive_http_origin_from_ws_url(&Self::ws_server_url()) {
                    Some(origin) => Cow::Owned(origin),
                    None => Self::server_root_url(),
                }
            }
        }
    }

    pub fn session_sharing_server_url() -> Option<Cow<'static, str>> {
        cfg_if::cfg_if! {
            if #[cfg(feature = "test-util")] {
                Some(Cow::Borrowed("fake_session_sharing_url"))
            } else {
                CHANNEL_STATE.lock().config.server_config.session_sharing_server_url.clone()
            }
        }
    }

    pub fn oz_root_url() -> Cow<'static, str> {
        CHANNEL_STATE.lock().config.oz_config.oz_root_url.clone()
    }

    pub fn server_root_url() -> Cow<'static, str> {
        cfg_if::cfg_if! {
            if #[cfg(feature = "test-util")] {
                Cow::Owned(MOCK_SERVER_URL.clone())
            } else {
                CHANNEL_STATE.lock().config.server_config.server_root_url.clone()
            }
        }
    }

    pub fn workload_audience_url() -> Cow<'static, str> {
        let state = CHANNEL_STATE.lock();
        match &state.config.oz_config.workload_audience_url {
            Some(url) => url.clone(),
            None => {
                drop(state);
                Self::server_root_url()
            }
        }
    }

    // Returns the origin url, with scheme, domain, and ports (if any)
    pub fn server_root_domain() -> Origin {
        Url::parse(&Self::server_root_url())
            .expect("Server root URL should be valid")
            .origin()
    }

    /// Returns the rudderstack destination for all events that don't contain user-generated content.
    pub fn rudderstack_non_ugc_destination() -> RudderStackDestination {
        let state = CHANNEL_STATE.lock();

        state
            .config
            .telemetry_config
            .as_ref()
            .and_then(|tc| tc.rudderstack_config.as_ref())
            .map(|rs| rs.non_ugc_destination())
            .unwrap_or_default()
    }

    /// Returns the rudderstack destination for all events that contain user-generated content.
    pub fn rudderstack_ugc_destination() -> RudderStackDestination {
        let state = CHANNEL_STATE.lock();

        state
            .config
            .telemetry_config
            .as_ref()
            .and_then(|tc| tc.rudderstack_config.as_ref())
            .map(|rs| rs.ugc_destination())
            .unwrap_or_default()
    }

    pub fn channel() -> Channel {
        CHANNEL_STATE.lock().channel
    }

    #[cfg(feature = "test-util")]
    pub fn app_version() -> Option<&'static str> {
        let version = APP_VERSION.lock();

        version.or_else(|| option_env!("GIT_RELEASE_TAG"))
    }

    #[cfg(feature = "test-util")]
    pub fn set_app_version(version: Option<&'static str>) {
        *APP_VERSION.lock() = version;
    }

    #[cfg(not(feature = "test-util"))]
    pub fn app_version() -> Option<&'static str> {
        option_env!("GIT_RELEASE_TAG")
    }

    pub fn sentry_url() -> Cow<'static, str> {
        CHANNEL_STATE
            .lock()
            .config
            .crash_reporting_config
            .as_ref()
            .map(|crc| crc.sentry_url.clone())
            .unwrap_or_default()
    }

    pub fn show_autoupdate_menu_items() -> bool {
        Self::is_autoupdate_enabled()
            && CHANNEL_STATE
                .lock()
                .config
                .autoupdate_config
                .as_ref()
                .map(|ac| ac.show_autoupdate_menu_items)
                .unwrap_or_default()
    }

    /// Mnemos IDE: whether this build checks for and installs updates at all. Only builds that
    /// carry an autoupdate config do (official releases, see app/build.rs), and
    /// `MNEMOS_UPDATER=off` in the environment turns it off at runtime, for packagers who ship
    /// updates themselves.
    pub fn is_autoupdate_enabled() -> bool {
        let disabled_by_env = std::env::var("MNEMOS_UPDATER")
            .is_ok_and(|value| value.trim().eq_ignore_ascii_case("off"));
        !disabled_by_env && CHANNEL_STATE.lock().config.autoupdate_config.is_some()
    }

    /// Mnemos IDE: the baked-in public keys update manifests must be signed with.
    pub fn update_public_keys() -> Vec<String> {
        CHANNEL_STATE
            .lock()
            .config
            .autoupdate_config
            .as_ref()
            .map(|ac| {
                ac.update_public_keys
                    .iter()
                    .map(|key| key.to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Returns the MCP OAuth provider config matching the given client ID, if any.
    pub fn mcp_oauth_provider_by_client_id(client_id: &str) -> Option<McpOAuthProviderConfig> {
        CHANNEL_STATE
            .lock()
            .config
            .mcp_static_config
            .as_ref()
            .and_then(|c| c.providers.iter().find(|p| p.client_id == client_id))
            .cloned()
    }

    /// Returns the MCP OAuth provider config matching the given issuer URL, if any.
    pub fn mcp_oauth_provider_by_issuer(issuer: &str) -> Option<McpOAuthProviderConfig> {
        CHANNEL_STATE
            .lock()
            .config
            .mcp_static_config
            .as_ref()
            .and_then(|c| c.providers.iter().find(|p| p.issuer == issuer))
            .cloned()
    }

    pub fn url_scheme() -> &'static str {
        match Self::channel() {
            Channel::Stable => "mnemos",
            Channel::Preview => "mnemospreview",
            Channel::Dev => "mnemosdev",
            // Dummy value--integration tests shouldn't support URL schemes.
            Channel::Integration => "mnemosintegration",
            Channel::Local => "mnemoslocal",
        }
    }
}

/// Derives an HTTP(S) origin URL from a WebSocket URL by rewriting the scheme
/// (`wss`→`https`, `ws`→`http`) and stripping the path, query, and fragment.
/// Returns [`None`] when the input cannot be parsed as a URL or uses a scheme
/// other than `ws` or `wss`.
#[cfg(not(feature = "test-util"))]
fn derive_http_origin_from_ws_url(ws_url: &str) -> Option<String> {
    let url = Url::parse(ws_url).ok()?;
    let http_scheme = match url.scheme() {
        "wss" => "https",
        "ws" => "http",
        _ => return None,
    };
    let host = url.host_str()?;
    let mut origin = format!("{http_scheme}://{host}");
    if let Some(port) = url.port() {
        origin.push_str(&format!(":{port}"));
    }
    Some(origin)
}

/// Inserts `-<tag>` into a log file name, before a trailing `.log` or at the end when there
/// is none: `mnemos.log` with tag `dev` becomes `mnemos-dev.log`.
///
/// Characters outside `[A-Za-z0-9_-]` in the tag become `_`, so a profile can never add
/// a path separator or a dot to the name. `None`, an empty tag or an empty base returns
/// the base unchanged.
fn tagged_logfile_name(base: &str, tag: Option<&str>) -> String {
    let Some(tag) = tag.filter(|tag| !tag.is_empty()) else {
        return base.to_owned();
    };
    if base.is_empty() {
        return String::new();
    }

    let tag: String = tag
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    match base.strip_suffix(".log") {
        Some(stem) => format!("{stem}-{tag}.log"),
        None => format!("{base}-{tag}"),
    }
}

#[cfg(all(test, not(feature = "test-util")))]
#[path = "state_tests.rs"]
mod tests;

fn app_id_from_bundle() -> Option<AppId> {
    // On macOS, attempt to determine the app ID from the containing bundle,
    // falling back to the channel-keyed "default" ID if we cannot retrieve
    // bundle information.
    //
    // We skip this for tests, as the call to `mainBundle` can take 30+ms,
    // which is a significant portion of the total test runtime.
    #[cfg(all(target_os = "macos", not(feature = "test-util")))]
    #[allow(deprecated)]
    unsafe {
        use cocoa::{
            base::{id, nil},
            foundation::NSBundle,
        };
        use objc::{msg_send, sel, sel_impl};
        use mnemos_ui::platform::mac::utils::nsstring_as_str;

        let bundle = id::mainBundle();
        if bundle != nil {
            let nsstring: id = msg_send![bundle, bundleIdentifier];
            if nsstring != nil {
                let app_id = nsstring_as_str(nsstring)
                    .expect("bundle IDs should always be valid UTF-8 strings");

                if !app_id.is_empty() {
                    return Some(
                        AppId::parse(app_id)
                            .expect("macOS bundle identifier has an unexpected format"),
                    );
                }
            }
        }
    }

    None
}

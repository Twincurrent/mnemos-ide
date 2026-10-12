//! Login-item registration — makes Mnemos start automatically when the user
//! signs in to their OS.
//!
//! The user-facing toggle and "already registered" bookkeeping live on
//! [`crate::terminal::general_settings::GeneralSettings`]. This module owns
//! the platform-specific register/unregister logic for each OS where the
//! feature is supported.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(any(target_os = "macos", target_os = "windows"))]
use crate::report_if_error;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use crate::terminal::general_settings::GeneralSettings;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use ::settings::Setting as _;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use mnemos_core::channel::ChannelState;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use mnemos_ui::{AppContext, SingletonEntity as _};

/// Reconciles whether Mnemos is registered to launch at login with the user's
/// current preference.
///
/// Respects the existing `app_added_as_login_item` bookkeeping so a user who
/// removed Mnemos from their OS's startup UI isn't silently re-added — the
/// platform backends only run the registration flow when the setting was
/// explicitly re-toggled.
///
/// Skipped entirely when the `MNEMOS_INTEGRATION` env var is set, so integration
/// tests never touch the user's real login items / registry. Also skipped for
/// non-release-bundle builds (e.g. `cargo run`), so developer machines don't
/// auto-launch `target/debug/{mnemos,openmnemos,...}` at sign-in.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn maybe_register_app_as_login_item(ctx: &mut AppContext) {
    if std::env::var("MNEMOS_INTEGRATION").is_ok() {
        log::debug!("Not registering as a login item in integration tests");
        return;
    }
    if !ChannelState::is_release_bundle() {
        log::debug!("Not a release bundle, skipping login-item registration");
        return;
    }
    keep_login_item_registered_under_old_default(ctx);
    #[cfg(target_os = "macos")]
    macos::maybe_register_app_as_login_item(ctx);
    #[cfg(target_os = "windows")]
    windows::maybe_register_app_as_login_item(ctx);
}

/// Mnemos IDE: `general.login_item` used to default to `true`, and it now defaults to
/// `false`. An install that registered itself under the old default never wrote the
/// setting, so the new default would unregister it on the next launch. Pin those
/// installs (registered, setting never set) with the private
/// `legacy_login_item_default`, which `GeneralSettings::launches_at_login` reads in
/// place of the unset key, so only new installs get the new default. The public
/// `general.login_item` is left unset: settings.toml syncs verbatim, and an explicit
/// `true` there would register the app at login on every other machine.
/// A user who removed Mnemos from the OS startup list by hand still has
/// `app_added_as_login_item` set, so the platform backends leave the OS entry alone.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn keep_login_item_registered_under_old_default(ctx: &mut AppContext) {
    GeneralSettings::handle(ctx).update(ctx, |settings, ctx| {
        if !settings.add_app_as_login_item.is_value_explicitly_set()
            && *settings.app_added_as_login_item
            && !*settings.legacy_login_item_default
        {
            report_if_error!(settings.legacy_login_item_default.set_value(true, ctx));
        }
    });
}

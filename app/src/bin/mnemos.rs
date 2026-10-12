// On Windows, we don't want to display a console window when the application is running in release
// builds. See https://doc.rust-lang.org/reference/runtime.html#the-windows_subsystem-attribute.
#![cfg_attr(feature = "release_bundle", windows_subsystem = "windows")]

#[path = "channel_config.rs"]
mod channel_config;

use anyhow::Result;
use mnemos_core::channel::{Channel, ChannelState};

// Simple wrapper around mnemos::run() for stable channel builds.
fn main() -> Result<()> {
    // Mnemos IDE: a debug build that is not a release bundle keeps its own data profile and
    // Mnemos IDE store by default, also when started without script/run (plain `cargo run`).
    // Must run before anything reads the environment or starts a thread.
    #[cfg(all(debug_assertions, not(feature = "release_bundle")))]
    {
        let plan = mnemos_ide_core::paths::apply_dev_isolation_defaults();
        if plan.profile.is_some() || plan.data_dir.is_some() {
            eprintln!(
                "Note: dev build isolated from the installed app (data profile {:?}, Mnemos IDE store {:?}); opt out with MNEMOS_DATA_PROFILE=",
                std::env::var("MNEMOS_DATA_PROFILE").unwrap_or_default(),
                std::env::var_os("MNEMOS_DATA_DIR").unwrap_or_default(),
            );
        }
    }

    ChannelState::set(ChannelState::new(
        Channel::Stable,
        channel_config::load_config!("stable"),
    ));

    mnemos::run()
}

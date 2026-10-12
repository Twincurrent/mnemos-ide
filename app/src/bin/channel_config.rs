//! Tools for loading a [`ChannelConfig`].
//!
//! Bundled (`release_bundle`) builds embed the config at compile time via the build script.
//! Other builds invoke the optional external config generator at runtime when it is on PATH, and
//! otherwise fall back to the dev config the build script embeds for them.
use mnemos_core::channel::ChannelConfig;

/// The name of the config generator binary, expected to be on PATH.
const CONFIG_BIN_NAME: &str = "mnemos-channel-config";

#[macro_export]
#[cfg(windows)]
macro_rules! path_concat {
    ($path:expr, $file:expr) => {
        concat!($path, "\\", $file)
    };
}
#[macro_export]
#[cfg(not(windows))]
macro_rules! path_concat {
    ($path:expr, $file:expr) => {
        concat!($path, "/", $file)
    };
}

#[macro_export]
macro_rules! load_config {
    ($channel:expr) => {{
        #[cfg(feature = "release_bundle")]
        {
            channel_config::load_config_from_embedded(include_str!($crate::path_concat!(
                env!("OUT_DIR"),
                concat!($channel, "_config.json")
            )))
        }

        #[cfg(not(feature = "release_bundle"))]
        {
            channel_config::load_config_from_generator($channel).unwrap_or_else(|| {
                channel_config::load_config_from_embedded(include_str!($crate::path_concat!(
                    env!("OUT_DIR"),
                    concat!($channel, "_config.json")
                )))
            })
        }
    }};
}
pub use load_config;

/// Invokes the config generator binary at runtime and deserializes its JSON output into a
/// [`ChannelConfig`]. Returns `None` when the generator is not on PATH, so the caller can use the
/// embedded config instead.
#[cfg_attr(feature = "release_bundle", expect(dead_code))]
pub fn load_config_from_generator(channel: &str) -> Option<ChannelConfig> {
    let target_family = if cfg!(target_family = "wasm") {
        "wasm"
    } else {
        "native"
    };

    let target_os = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    };

    let output = match command::blocking::Command::new(CONFIG_BIN_NAME)
        .arg("--channel")
        .arg(channel)
        .arg("--target-family")
        .arg(target_family)
        .arg("--target-os")
        .arg(target_os)
        .output()
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // The logger is not initialised yet at this point, so a log call would be dropped.
            eprintln!(
                "'{CONFIG_BIN_NAME}' was not found on PATH; using the built-in dev config for \
                 channel '{channel}'."
            );
            return None;
        }
        Err(err) => panic!("Failed to execute '{CONFIG_BIN_NAME}': {err}"),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        panic!("Config generator failed for channel '{channel}':\n{stderr}");
    }

    let config = serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        let stdout = String::from_utf8_lossy(&output.stdout);
        panic!("Failed to parse config generator output for channel '{channel}': {err}\nOutput:\n{stdout}")
    });
    Some(config)
}

/// Deserializes a [`ChannelConfig`] from a JSON string embedded at compile time.
///
/// Release bundles always load their configuration this way. Other builds use it when the
/// config generator is not on PATH.
pub fn load_config_from_embedded(json: &str) -> ChannelConfig {
    serde_json::from_str(json)
        .unwrap_or_else(|err| panic!("Failed to parse embedded channel config: {err}"))
}

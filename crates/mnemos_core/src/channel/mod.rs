mod config;
mod state;

use std::fmt;

pub use config::*;
pub use state::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// The official/first-party stable release.
    Stable,
    /// The official/first-party feature preview release.
    Preview,

    /// The internal-only nightly build.
    Dev,
    /// The internal-only HEAD build, and the channel of a build that has not set one.
    ///
    /// Mnemos IDE: upstream also had `Oss`, a separate open-source channel with its own binary
    /// (`mnemos-oss`) and app id. A plain `cargo build` of the stable binary is that build here:
    /// it carries no telemetry, crash reporting or updater (app/build.rs), so `Oss` was retired.
    Local,

    /// The integration test build.
    Integration,
}

impl Channel {
    /// Whether or not this channel is for internal use only
    pub fn is_dogfood(&self) -> bool {
        match self {
            Channel::Dev | Channel::Local => true,
            Channel::Stable | Channel::Preview | Channel::Integration => false,
        }
    }

    /// Whether this channel honors the `--server-root-url` / `--ws-server-url` /
    /// `--session-sharing-server-url` flags (and their `MNEMOS_*` env-var equivalents).
    ///
    /// Release channels (`Stable`, `Preview`) ignore these overrides so shipped
    /// builds can't be redirected away from their baked-in server URLs. Internal-only channels
    /// (`Dev`, `Local`, `Integration`) continue to honor them for local development and testing.
    pub fn allows_server_url_overrides(&self) -> bool {
        match self {
            Channel::Dev | Channel::Local | Channel::Integration => true,
            Channel::Stable | Channel::Preview => false,
        }
    }

    /// Returns the CLI command name corresponding to this channel.
    ///
    /// Mnemos IDE: upstream named these `oz`, which is Warp's own CLI command; installing
    /// it would take over (and on uninstall remove) a Warp user's `oz`.
    pub fn cli_command_name(&self) -> &'static str {
        match self {
            Channel::Stable => "mnemos",
            Channel::Dev => "mnemos-dev",
            Channel::Preview => "mnemos-preview",
            Channel::Local => "mnemos-local",
            Channel::Integration => "mnemos-integration",
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match self {
            Channel::Stable => "stable",
            Channel::Preview => "preview",
            Channel::Dev => "dev",
            Channel::Integration => "integration",
            Channel::Local => "local",
        })
    }
}

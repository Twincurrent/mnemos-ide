//! Mnemos IDE: settings for how far CLI agent events (OSC 777 `mnemos://cli-agent`) are
//! trusted.
//!
//! Events carrying this terminal's token (`MNEMOS_CLI_AGENT_TOKEN`) are always accepted.
//! A bare sentinel without a token, including upstream's legacy `warp://cli-agent`, can
//! be printed by anything that runs in the pane (a `cat` of a file, a tool's output
//! inside an agent session), so it is ignored unless the user opts in here. The plugins
//! that still send bare sentinels are the upstream Gemini and OpenCode extensions and
//! mnemos-agents before 1.2.0.
//!
//! Without the opt-in, bare Claude Code events are still trusted while the pane runs
//! `claude` on a machine whose installed mnemos-agents predates the token
//! (`event::installed_plugin_predates_token`), so installs set up before the token keep
//! their agents panel until the plugin is updated, after which bare events are dropped.

use settings::{macros::define_settings_group, SupportedPlatforms, SyncToCloud};

define_settings_group!(CliAgentEventSettings, settings: [
    accept_untokened_cli_agent_events: AcceptUntokenedCliAgentEvents {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        private: false,
        toml_path: "agents.third_party.accept_untokened_cli_agent_events",
        description: "Whether to accept CLI agent events that do not carry this terminal's token (bare mnemos://cli-agent or legacy warp://cli-agent titles), while the pane runs that agent. Needed by agent plugins that predate the token, such as the Gemini and OpenCode extensions.",
    },
]);

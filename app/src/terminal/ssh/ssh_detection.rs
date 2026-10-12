use serde::{Deserialize, Serialize};
use mnemos_core::{features::FeatureFlag, settings::Setting};
use mnemos_util::path::ShellFamily;

use crate::terminal::mnemosify::settings::MnemosifySettings;

/// The different possible outcomes of detecting an interactive SSH session.
/// Also the payload for the [`crate::server::telemetry::TelemetryEvent::SshInteractiveSessionDetected`] event.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SshInteractiveSessionDetected {
    #[serde(rename = "feature_disabled")]
    FeatureDisabled,
    #[serde(rename = "host_denylisted")]
    HostDenylisted,
    #[serde(rename = "mnemosify_prompt")]
    ShouldPromptMnemosification {
        #[serde(skip)]
        command: String,
        #[serde(skip)]
        host: Option<String>,
    },
}

/// Determines whether a host could be mnemosified.
pub fn evaluate_mnemosify_ssh_host(
    command: &str,
    ssh_host: Option<&str>,
    shell_family: ShellFamily,
    mnemosify_settings: &MnemosifySettings,
) -> SshInteractiveSessionDetected {
    let should_prompt_ssh_tmux_wrapper = *mnemosify_settings.enable_ssh_mnemosification.value()
        && *mnemosify_settings.use_ssh_tmux_wrapper.value();
    let matches_subshell = mnemosify_settings.is_denylisted_subshell_command(command)
        || mnemosify_settings.is_compatible_subshell_command(command, shell_family);
    if !should_prompt_ssh_tmux_wrapper
        || matches_subshell
        || !FeatureFlag::SSHTmuxWrapper.is_enabled()
    {
        return SshInteractiveSessionDetected::FeatureDisabled;
    }

    if let Some(ssh_host) = ssh_host {
        if mnemosify_settings.is_ssh_host_denylisted(ssh_host) {
            return SshInteractiveSessionDetected::HostDenylisted;
        }
    }

    SshInteractiveSessionDetected::ShouldPromptMnemosification {
        host: ssh_host.map(|host| host.to_owned()),
        command: command.to_string(),
    }
}

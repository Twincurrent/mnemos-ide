use std::sync::Arc;

use anyhow::Result;
use channel_versions::{Changelog, ChannelVersions};

use crate::{
    channel::{Channel, ChannelState},
    server::server_api::ServerApi,
};

use super::channel_versions::fetch_channel_versions;
use super::release_assets_directory_url;

/// Mnemos IDE: reads the changelog from the static release manifest (or, on Dev, the per-version
/// changelog.json) with the same anonymous request as the update check. A build without an
/// updater fetches nothing. At launch the caller only asks when this version has not been
/// launched before (`root_view.rs`); otherwise only when the person opens the changelog.
pub async fn get_current_changelog(server_api: Arc<ServerApi>) -> Result<Option<Changelog>> {
    if !ChannelState::is_autoupdate_enabled() {
        return Ok(None);
    }

    let channel = ChannelState::channel();

    if should_fetch_changelog_json(channel) {
        log::info!("Attempting to fetch changelog.json");
        match fetch_current_changelog(server_api.http_client()).await {
            changelog_result @ Ok(_) => {
                return changelog_result.map(Option::Some);
            }
            Err(error) => log::error!("Failed to fetch changelog.json: {error}"),
        };
    }

    let versions: ChannelVersions = fetch_channel_versions(server_api.http_client()).await?;

    let res = versions.changelogs.and_then(|changelogs| {
        match channel {
            Channel::Stable => Some(changelogs.stable),
            Channel::Preview => Some(changelogs.preview),
            Channel::Dev | Channel::Local => Some(changelogs.dev),
            // Integration tests don't support autoupdate.
            Channel::Integration => None,
        }
        .and_then(|versions| {
            ChannelState::app_version()
                .and_then(|running_version| versions.get(running_version))
                .cloned()
        })
    });
    Ok(res)
}

/// Fetches the changelog for the running release bundle.
async fn fetch_current_changelog(client: &http_client::Client) -> Result<Changelog> {
    let app_version = ChannelState::app_version().unwrap_or_default();
    let url = changelog_url(ChannelState::channel(), app_version);
    let res = client.get(url.as_str()).send().await?;
    let changelog: Changelog = res.json().await?;
    log::info!("Received changelog.json for {app_version}");
    Ok(changelog)
}

/// Returns the URL to the changelog for the given version of this release
/// bundle.
fn changelog_url(channel: Channel, version: &str) -> String {
    format!(
        "{}/changelog.json",
        release_assets_directory_url(channel, version)
    )
}

/// Returns whether the app should fetch changelog.json for the current
/// build (true), or use the changelog information embedded in
/// channel_versions.json (false).
pub fn should_fetch_changelog_json(channel: Channel) -> bool {
    channel == Channel::Dev
}

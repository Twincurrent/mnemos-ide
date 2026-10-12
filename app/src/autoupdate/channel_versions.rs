use std::{env, fs::read_to_string};

use anyhow::{bail, Context as _, Result};
use channel_versions::ChannelVersions;

use crate::{channel::ChannelState, server::server_api::FETCH_CHANNEL_VERSIONS_TIMEOUT};

/// Fetches the release manifest.
///
/// Mnemos IDE: this is an anonymous GET of the static `{releases_base_url}/channel_versions.json`
/// and nothing else. Upstream first asked the server's `/client_version/daily` endpoint, sending
/// the persistent install id (`X-Mnemos-Experiment-Id`) and the signed-in user's bearer token;
/// the static file was only its fallback. No install id, token or nonce is sent now, so the
/// request says nothing about who is asking beyond what any HTTP client does. What the manifest
/// says is only trusted once its signature verifies (see `fetch_version` in `mod.rs`).
pub async fn fetch_channel_versions(client: &http_client::Client) -> Result<ChannelVersions> {
    if let Ok(path) = env::var("MNEMOS_CHANNEL_VERSIONS_PATH") {
        // Load channel versions from local filesystem. Used for testing both
        // autoupdate and changelog behavior.
        let path = shellexpand::tilde(&path);
        let channel_versions_string = read_to_string::<&str>(&path)?;
        return serde_json::from_str(channel_versions_string.as_str())
            .context("Failed to parse channel versions JSON");
    }

    let releases_base_url = ChannelState::releases_base_url();
    if releases_base_url.is_empty() {
        bail!("This build has no release host to check for updates");
    }
    let url = format!("{releases_base_url}/channel_versions.json");
    log::info!("Fetching the release manifest from {url}");
    let res = client
        .get(url.as_str())
        .timeout(FETCH_CHANNEL_VERSIONS_TIMEOUT)
        .send()
        .await?
        .error_for_status()?;
    let versions: ChannelVersions = res.json().await?;
    log::info!("Received release manifest: {versions}");
    Ok(versions)
}

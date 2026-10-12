use std::path::Path;
use std::sync::Arc;

use crate::language_server_candidate::{LanguageServerCandidate, LanguageServerMetadata};
use crate::CommandBuilder;
use async_trait::async_trait;

/// Mnemos IDE: the pinned gopls version. `go install` verifies the module against the
/// Go checksum database (sum.golang.org), so pinning the version pins the content.
#[cfg(feature = "local_fs")]
const PINNED_GOPLS_VERSION: &str = "v0.23.0";

pub struct GoPlsCandidate {
    // Mnemos IDE: unused since the version is pinned; kept so every candidate is
    // constructed the same way.
    #[allow(dead_code)]
    client: Arc<http_client::Client>,
}

impl GoPlsCandidate {
    pub fn new(client: Arc<http_client::Client>) -> Self {
        Self { client }
    }
}

#[async_trait]
#[cfg(feature = "local_fs")]
impl LanguageServerCandidate for GoPlsCandidate {
    async fn should_suggest_for_repo(&self, path: &Path, executor: &CommandBuilder) -> bool {
        if !path.join("go.mod").exists() {
            return false;
        }

        // Check if Go is installed
        executor
            .command("go")
            .arg("version")
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    async fn is_installed_in_data_dir(&self, _executor: &CommandBuilder) -> bool {
        // gopls doesn't support custom installation yet
        false
    }

    async fn is_installed_on_path(&self, executor: &CommandBuilder) -> bool {
        executor
            .command("gopls")
            .arg("version")
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    async fn install(
        &self,
        _metadata: LanguageServerMetadata,
        executor: &CommandBuilder,
    ) -> anyhow::Result<()> {
        let output = executor
            .command("go")
            .args([
                "install".to_owned(),
                format!("golang.org/x/tools/gopls@{PINNED_GOPLS_VERSION}"),
            ])
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to install gopls: {}", stderr);
        }

        Ok(())
    }

    async fn fetch_latest_server_metadata(&self) -> anyhow::Result<LanguageServerMetadata> {
        // gopls doesn't provide prebuilt binaries; it must be installed via `go install`.
        // Mnemos IDE: pinned version, no network lookup.
        Ok(LanguageServerMetadata {
            version: PINNED_GOPLS_VERSION.to_owned(),
            url: None,
            digest: None,
        })
    }
}

#[async_trait]
#[cfg(not(feature = "local_fs"))]
impl LanguageServerCandidate for GoPlsCandidate {
    async fn should_suggest_for_repo(&self, _path: &Path, _executor: &CommandBuilder) -> bool {
        false
    }

    async fn is_installed_in_data_dir(&self, _executor: &CommandBuilder) -> bool {
        false
    }

    async fn is_installed_on_path(&self, _executor: &CommandBuilder) -> bool {
        false
    }

    async fn install(
        &self,
        _metadata: LanguageServerMetadata,
        _executor: &CommandBuilder,
    ) -> anyhow::Result<()> {
        // Mnemos IDE: an error, not a panic, in builds without local filesystem access.
        anyhow::bail!("Installing language servers needs the local_fs feature")
    }

    async fn fetch_latest_server_metadata(&self) -> anyhow::Result<LanguageServerMetadata> {
        anyhow::bail!("Installing language servers needs the local_fs feature")
    }
}

use std::path::Path;
#[cfg(feature = "local_fs")]
use std::path::PathBuf;
use std::sync::Arc;

#[cfg(feature = "local_fs")]
use command::r#async::Command;

#[cfg(feature = "local_fs")]
use crate::install::{install_from_github, pinned_github_metadata, AssetKind, PinnedAsset};
use crate::language_server_candidate::{LanguageServerCandidate, LanguageServerMetadata};
use crate::CommandBuilder;
use async_trait::async_trait;

#[cfg(feature = "local_fs")]
const SERVER_NAME: &str = "clangd";

#[cfg_attr(not(feature = "local_fs"), allow(dead_code))]
pub struct ClangdCandidate {
    client: Arc<http_client::Client>,
}

impl ClangdCandidate {
    pub fn new(client: Arc<http_client::Client>) -> Self {
        Self { client }
    }

    #[cfg(feature = "local_fs")]
    pub async fn find_installed_binary_in_data_dir() -> Option<PathBuf> {
        let install_root = mnemos_core::paths::data_dir().join(SERVER_NAME);
        if !install_root.is_dir() {
            return None;
        }

        let Ok(entries) = std::fs::read_dir(&install_root) else {
            return None;
        };

        for entry in entries.flatten() {
            let version_dir = entry.path();
            if !version_dir.is_dir() {
                continue;
            }

            let Some(binary_path) = find_binary_in_dir(&version_dir) else {
                continue;
            };

            if binary_is_working(&binary_path).await {
                return Some(binary_path);
            }
        }

        None
    }
}

/// Mnemos IDE: the pinned clangd release. Bump the tag and every digest together
/// (the `digest` field of each asset in the GitHub release API response).
#[cfg(feature = "local_fs")]
const PINNED_TAG: &str = "23.1.0";

/// The pinned clangd asset for the current platform (`clangd-{os}-{tag}.zip`).
#[cfg(feature = "local_fs")]
fn pinned_asset() -> anyhow::Result<PinnedAsset> {
    let (name, sha256) = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", _) => (
            "clangd-mac-23.1.0.zip",
            "1082e6638223b785ca2daf0939f13afcd0bb95c84ee9a4bbaff4745365159253",
        ),
        ("linux", "x86_64") => (
            "clangd-linux-23.1.0.zip",
            "e53b1a96196095faedb7642cf64964f7fb9ad4a0c1f00dd2c172a3d9dcbafdfd",
        ),
        ("windows", "x86_64") => (
            "clangd-windows-23.1.0.zip",
            "23412a240756a162e7b98a282f36aa2a23a88db5ce16a0cbc4fef7253768c810",
        ),
        (os, arch) => anyhow::bail!("Unsupported platform for clangd: {os}/{arch}"),
    };
    Ok(PinnedAsset { name, sha256 })
}

#[cfg(feature = "local_fs")]
fn is_c_or_cpp_extension(extension: &str) -> bool {
    matches!(
        extension,
        "c" | "C" | "cc" | "cpp" | "cxx" | "h" | "hh" | "hpp" | "hxx" | "H"
    )
}

#[cfg(feature = "local_fs")]
async fn binary_is_working(binary_path: &Path) -> bool {
    let mut command = Command::new(binary_path);
    command.arg("--version");
    command
        .output()
        .await
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(feature = "local_fs")]
fn is_bin_clangd_path(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };

    let expected_name = if cfg!(windows) {
        "clangd.exe"
    } else {
        "clangd"
    };

    if file_name != expected_name {
        return false;
    }

    path.parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        == Some("bin")
}

#[cfg(feature = "local_fs")]
fn find_binary_in_dir(root: &Path) -> Option<PathBuf> {
    let mut directories = vec![root.to_path_buf()];

    while let Some(dir) = directories.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                directories.push(path);
                continue;
            }

            if is_bin_clangd_path(&path) {
                return Some(path);
            }
        }
    }

    None
}

#[async_trait]
#[cfg(feature = "local_fs")]
impl LanguageServerCandidate for ClangdCandidate {
    async fn should_suggest_for_repo(&self, path: &Path, _executor: &CommandBuilder) -> bool {
        let repo_markers = [
            "compile_commands.json",
            "compile_flags.txt",
            ".clangd",
            "CMakeLists.txt",
        ];

        if repo_markers.iter().any(|marker| path.join(marker).exists()) {
            return true;
        }

        let Ok(entries) = std::fs::read_dir(path) else {
            return false;
        };

        entries.flatten().any(|entry| {
            let file_path = entry.path();
            file_path.is_file()
                && file_path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(is_c_or_cpp_extension)
        })
    }

    async fn is_installed_in_data_dir(&self, _executor: &CommandBuilder) -> bool {
        Self::find_installed_binary_in_data_dir().await.is_some()
    }

    async fn is_installed_on_path(&self, executor: &CommandBuilder) -> bool {
        executor
            .command(SERVER_NAME)
            .arg("--version")
            .output()
            .await
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    async fn install(
        &self,
        metadata: LanguageServerMetadata,
        _executor: &CommandBuilder,
    ) -> anyhow::Result<()> {
        let binary_path = install_from_github(
            &self.client,
            &metadata,
            SERVER_NAME,
            AssetKind::Zip,
            Some(find_binary_in_dir),
        )
        .await?;

        // Verify the installed binary works
        if !binary_is_working(&binary_path).await {
            anyhow::bail!(
                "Installed clangd binary at {} failed version check",
                binary_path.display()
            );
        }

        Ok(())
    }

    async fn fetch_latest_server_metadata(&self) -> anyhow::Result<LanguageServerMetadata> {
        // Mnemos IDE: pinned, not the latest GitHub release (see `PinnedAsset`).
        Ok(pinned_github_metadata(
            "clangd",
            "clangd",
            PINNED_TAG,
            pinned_asset()?,
        ))
    }
}

#[async_trait]
#[cfg(not(feature = "local_fs"))]
impl LanguageServerCandidate for ClangdCandidate {
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

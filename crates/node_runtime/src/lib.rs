//! Node.js and npm runtime management for Mnemos.
//!
//! This module provides functionality to install and manage Node.js/npm,
//! supporting multiple platforms (macOS, Linux, Windows) and architectures
//! (x64, arm64).

cfg_if::cfg_if! {
    if #[cfg(feature = "local_fs")] {
        use anyhow::{bail, Context, Result};
        use flate2::read::GzDecoder;
        use futures::io::AsyncWriteExt;
        use std::ffi::OsStr;
        use std::io::Read;
        use std::path::{Path, PathBuf};
        use tar::Archive;
        use command::r#async::Command;
        use semver::Version;
        use sha2::{Digest, Sha256};
    }
}

/// The pinned Node.js version to install.
#[cfg(feature = "local_fs")]
const NODE_VERSION: &str = "v22.12.0";

/// Mnemos IDE: SHA256 of each [`NODE_VERSION`] archive, from
/// `https://nodejs.org/dist/<version>/SHASUMS256.txt`. Upstream downloaded the archive
/// and ran the Node.js inside it without checking it. Bump together with the version.
#[cfg(feature = "local_fs")]
const NODE_SHA256: &[(&str, &str)] = &[
    (
        "node-v22.12.0-darwin-arm64.tar.gz",
        "293dcc6c2408da21562d135b0412525e381bb6fe150d688edb58fe850d0f3e13",
    ),
    (
        "node-v22.12.0-darwin-x64.tar.gz",
        "52bc25dd026db7247c3c00439afdb83e95087248267f02d6c1a7250d1f896173",
    ),
    (
        "node-v22.12.0-linux-arm64.tar.gz",
        "9e7905fdee722f9650a03ae644b51c4c6effd3b98ac93c588700072ab35c9ddb",
    ),
    (
        "node-v22.12.0-linux-x64.tar.gz",
        "e05a4d65232ae2b27b3d77da2e368522fb46b923335b8e0d5f77624c32484044",
    ),
    (
        "node-v22.12.0-win-arm64.zip",
        "17401720af48976e3f67c41e8968a135fb49ca1f88103a92e0e8c70605763854",
    ),
    (
        "node-v22.12.0-win-x64.zip",
        "2b8f2256382f97ad51e29ff71f702961af466c4616393f767455501e6aece9b8",
    ),
];

/// Mnemos IDE: the pinned SHA256 for the Node.js archive `file_name`, if any.
#[cfg(feature = "local_fs")]
fn pinned_node_sha256(file_name: &str) -> Option<&'static str> {
    NODE_SHA256
        .iter()
        .find(|(name, _)| *name == file_name)
        .map(|(_, sha256)| *sha256)
}

/// Mnemos IDE: refuses `bytes` unless they hash to the pinned digest of `file_name`.
#[cfg(feature = "local_fs")]
fn verify_node_archive(file_name: &str, bytes: &[u8]) -> Result<()> {
    let expected = pinned_node_sha256(file_name).with_context(|| {
        format!("Refusing to install Node.js: no pinned SHA256 for {file_name}")
    })?;
    let actual = format!("{:x}", Sha256::digest(bytes));
    if !actual.eq_ignore_ascii_case(expected) {
        bail!("SHA256 checksum mismatch for {file_name}. Expected: {expected}, Got: {actual}");
    }
    Ok(())
}

/// Minimum supported Node.js version for system-installed Node.
#[cfg(feature = "local_fs")]
const MIN_NODE_VERSION: Version = Version::new(20, 0, 0);

// Platform-specific paths for Node.js binaries
cfg_if::cfg_if! {
    if #[cfg(all(feature = "local_fs", windows))] {
        const NODE_BINARY_PATH: &str = "node.exe";
        const NPM_BINARY_PATH: &str = "node_modules/npm/bin/npm-cli.js";
    } else if #[cfg(feature = "local_fs")] {
        const NODE_BINARY_PATH: &str = "bin/node";
        const NPM_BINARY_PATH: &str = "bin/npm";
    }
}

/// Archive type for Node.js distribution.
#[cfg(feature = "local_fs")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveType {
    /// A gzip-compressed tarball (for macOS and Linux)
    TarGz,
    /// A zip archive (for Windows)
    Zip,
}

/// Platform-specific Node.js distribution information.
#[cfg(feature = "local_fs")]
struct NodeDistribution {
    os: &'static str,
    arch: &'static str,
    archive_type: ArchiveType,
}

#[cfg(feature = "local_fs")]
impl NodeDistribution {
    /// Determines the Node.js distribution for the current platform.
    fn current() -> Result<Self> {
        let os = match std::env::consts::OS {
            "macos" => "darwin",
            "linux" => "linux",
            "windows" => "win",
            other => bail!("Unsupported operating system: {}", other),
        };

        let arch = match std::env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            other => bail!("Unsupported architecture: {}", other),
        };

        let archive_type = match std::env::consts::OS {
            "windows" => ArchiveType::Zip,
            _ => ArchiveType::TarGz,
        };

        Ok(Self {
            os,
            arch,
            archive_type,
        })
    }

    /// Returns the folder name for the extracted Node.js distribution.
    fn folder_name(&self, version: &str) -> String {
        format!("node-{}-{}-{}", version, self.os, self.arch)
    }

    /// Returns the file extension for the archive.
    fn file_extension(&self) -> &'static str {
        match self.archive_type {
            ArchiveType::TarGz => "tar.gz",
            ArchiveType::Zip => "zip",
        }
    }

    /// Returns the archive file name of the Node.js distribution.
    fn archive_file_name(&self, version: &str) -> String {
        format!(
            "node-{}-{}-{}.{}",
            version,
            self.os,
            self.arch,
            self.file_extension()
        )
    }

    /// Returns the download URL for the Node.js distribution.
    fn download_url(&self, version: &str) -> String {
        format!(
            "https://nodejs.org/dist/{}/{}",
            version,
            self.archive_file_name(version)
        )
    }
}

/// Returns the path to the Node.js installation directory.
#[cfg(feature = "local_fs")]
pub fn node_installation_dir() -> Result<PathBuf> {
    let dist = NodeDistribution::current()?;
    let folder_name = dist.folder_name(NODE_VERSION);
    Ok(mnemos_core::paths::data_dir().join("node").join(folder_name))
}

/// Returns the path to the installed node binary.
#[cfg(feature = "local_fs")]
pub fn node_binary_path() -> Result<PathBuf> {
    Ok(node_installation_dir()?.join(NODE_BINARY_PATH))
}

/// Returns the path to the installed npm binary/script.
#[cfg(feature = "local_fs")]
pub fn npm_binary_path() -> Result<PathBuf> {
    Ok(node_installation_dir()?.join(NPM_BINARY_PATH))
}

/// Installs Node.js and npm if not already installed.
///
/// This function will:
/// 1. Check if a valid Node.js installation already exists
/// 2. Download the appropriate Node.js distribution for the current platform
/// 3. Extract it to the Mnemos data directory
///
/// # Returns
/// Returns the path to the Node.js installation directory on success.
///
/// # Errors
/// Returns an error if:
/// - The current platform/architecture is not supported
/// - Network errors occur during download
/// - Extraction fails
#[cfg(feature = "local_fs")]
pub async fn install_npm(client: &http_client::Client) -> Result<PathBuf> {
    log::info!("Node.js runtime install_npm called");

    let dist = NodeDistribution::current()?;
    let version = NODE_VERSION;

    let folder_name = dist.folder_name(version);
    let node_containing_dir = mnemos_core::paths::data_dir().join("node");
    let node_dir = node_containing_dir.join(&folder_name);
    let node_binary = node_dir.join(NODE_BINARY_PATH);

    // Check if we already have a valid installation
    if is_valid_installation(&node_binary).await {
        log::info!(
            "Using existing Node.js installation at {}",
            node_dir.display()
        );
        return Ok(node_dir);
    }

    // Remove any existing (potentially corrupted) installation
    if async_fs::metadata(&node_containing_dir).await.is_ok() {
        log::info!("Removing existing Node.js directory for clean installation");
        async_fs::remove_dir_all(&node_containing_dir)
            .await
            .context("Failed to remove existing Node.js directory")?;
    }

    // Create the containing directory
    async_fs::create_dir_all(&node_containing_dir)
        .await
        .context("Failed to create Node.js containing directory")?;

    // Download Node.js
    let url = dist.download_url(version);
    log::info!("Downloading Node.js from {}", url);

    let response = client
        .get(&url)
        .send()
        .await
        .context("Failed to download Node.js")?;

    if !response.status().is_success() {
        bail!(
            "Node.js download failed with status {}: {}",
            response.status(),
            response.text().await.unwrap_or_default()
        );
    }

    let bytes = response
        .bytes()
        .await
        .context("Failed to read Node.js download response")?;

    // Mnemos IDE: verify the archive against the pinned digest before extracting it.
    verify_node_archive(&dist.archive_file_name(version), &bytes)?;

    log::info!("Download complete, extracting...");

    // Extract the archive
    match dist.archive_type {
        ArchiveType::TarGz => {
            extract_tar_gz(&bytes, &node_containing_dir)?;
        }
        ArchiveType::Zip => {
            extract_zip(&bytes, &node_containing_dir, None::<fn(&str) -> bool>).await?;
        }
    }

    log::info!("Node.js extracted successfully to {}", node_dir.display());

    Ok(node_dir)
}

/// Checks if an existing Node.js installation is valid.
#[cfg(feature = "local_fs")]
async fn is_valid_installation(node_binary: &Path) -> bool {
    // Check if the binary exists
    if async_fs::metadata(node_binary).await.is_err() {
        return false;
    }

    // Try to run node --version to verify it works
    let result = Command::new(node_binary).arg("--version").output().await;

    match result {
        Ok(output) => {
            if output.status.success() {
                log::debug!(
                    "Node.js version check passed: {}",
                    String::from_utf8_lossy(&output.stdout).trim()
                );
                true
            } else {
                log::warn!(
                    "Node.js version check failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                false
            }
        }
        Err(err) => {
            log::warn!("Failed to run Node.js binary: {}", err);
            false
        }
    }
}

/// Extracts a gzip-compressed tarball to the destination directory.
///
/// This is a simple wrapper around tar::Archive that extracts all contents.
#[cfg(feature = "local_fs")]
pub fn extract_tar_gz(data: &[u8], dest_dir: &Path) -> Result<()> {
    let decoder = GzDecoder::new(data);
    let mut archive = Archive::new(decoder);
    archive
        .unpack(dest_dir)
        .context("Failed to extract tar.gz archive")?;
    Ok(())
}

/// Extracts a single gzip-compressed file to the destination path.
///
/// This decompresses a `.gz` file (not a `.tar.gz` archive) directly to a file.
#[cfg(feature = "local_fs")]
pub async fn extract_gz(data: &[u8], dest_path: &Path) -> Result<()> {
    let mut decoder = GzDecoder::new(data);
    let mut decompressed = Vec::new();
    decoder
        .read_to_end(&mut decompressed)
        .context("Failed to decompress gzip data")?;

    let mut file = async_fs::File::create(dest_path)
        .await
        .with_context(|| format!("Failed to create file: {:?}", dest_path))?;
    file.write_all(&decompressed)
        .await
        .with_context(|| format!("Failed to write to {:?}", dest_path))?;
    file.flush()
        .await
        .with_context(|| format!("Failed to flush {:?}", dest_path))?;

    Ok(())
}

/// Extracts files from a zip archive to the destination directory.
///
/// # Arguments
/// * `data` - The zip archive data
/// * `dest_dir` - The destination directory to extract to
/// * `file_filter` - Optional filter function that takes a file name and returns true if it should be extracted.
///   If None, all files are extracted.
///
/// # Returns
/// Returns Ok(()) on success, or an error if extraction fails or if a filter is provided but no files match.
///
/// Note: This function performs synchronous zip reading (which is CPU-bound) followed by
/// async file I/O. Similar to `extract_gz`, this approach is acceptable for reasonably-sized
/// archives since the zip reading is relatively fast.
#[cfg(feature = "local_fs")]
pub async fn extract_zip<F>(data: &[u8], dest_dir: &Path, file_filter: Option<F>) -> Result<()>
where
    F: Fn(&str) -> bool,
{
    // Extract all file data synchronously first
    let files_to_write: Vec<(PathBuf, Vec<u8>)> = {
        let cursor = std::io::Cursor::new(data);
        let mut archive = zip::ZipArchive::new(cursor).context("Failed to read zip archive")?;
        let mut result = Vec::new();

        for i in 0..archive.len() {
            let mut file = archive.by_index(i).context("Failed to read zip entry")?;
            let file_name = file.name().to_string();

            if let Some(ref filter) = file_filter {
                if !filter(&file_name) {
                    continue;
                }
            }

            let outpath = match file.enclosed_name() {
                Some(path) => dest_dir.join(path),
                None => continue,
            };

            if !file.is_dir() {
                let mut data = Vec::new();
                file.read_to_end(&mut data)
                    .context("Failed to read file from zip")?;
                result.push((outpath, data));
            }
        }
        result
    }; // archive is dropped here

    // If a filter was provided but no files matched, return an error
    if file_filter.is_some() && files_to_write.is_empty() {
        bail!("No files matched the filter in the zip archive");
    }

    // Now do async file I/O
    for (outpath, data) in files_to_write {
        if let Some(parent) = outpath.parent() {
            async_fs::create_dir_all(parent).await.ok();
        }

        let mut outfile = async_fs::File::create(&outpath)
            .await
            .with_context(|| format!("Failed to create file: {:?}", outpath))?;
        outfile
            .write_all(&data)
            .await
            .with_context(|| format!("Failed to write file: {:?}", outpath))?;
        outfile
            .flush()
            .await
            .with_context(|| format!("Failed to flush file: {:?}", outpath))?;
    }

    Ok(())
}

/// Finds a working Node.js binary, preferring our custom installation over system node.
///
/// This function checks:
/// 1. First, our custom Node.js installation in the Mnemos data directory
/// 2. Falls back to system Node.js if custom isn't available
///
/// # Arguments
/// * `path_env_var` - The PATH environment variable to use when checking for system node.
///   If None, system node will not be checked.
///
/// # Returns
/// Returns the path to a working node binary, or `None` if no working node is found.
/// For system node, returns `PathBuf::from("node")` to let PATH resolution handle it.
#[cfg(feature = "local_fs")]
pub async fn find_working_node_binary(path_env_var: Option<&str>) -> Option<PathBuf> {
    // First, try our custom node installation
    if let Ok(custom_node) = node_binary_path() {
        if custom_node.is_file() {
            let mut cmd = Command::new(&custom_node);
            cmd.arg("--version");
            if let Ok(output) = cmd.output().await {
                if output.status.success() {
                    log::info!(
                        "Using custom node installation at {}",
                        custom_node.display()
                    );
                    return Some(custom_node);
                }
            }
        }
    }

    // Fall back to system node if available
    if let Some(path_env_var) = path_env_var {
        if detect_system_node(path_env_var).await.is_ok() {
            // System node is available and meets version requirements.
            // Use "node" and let the PATH resolve it.
            log::info!("Using system node");
            return Some(PathBuf::from("node"));
        }
    }

    log::info!("No working node binary found");
    None
}

/// Detects and validates the system-installed Node.js meets the minimum version requirement.
///
/// This function runs `node --version` using the provided PATH environment variable
/// and verifies that the installed version meets the minimum requirement.
///
/// # Arguments
/// * `path_env_var` - The PATH environment variable value to use when running node
///
/// # Errors
/// Returns an error if Node.js is not found or doesn't meet the minimum version.
#[cfg(feature = "local_fs")]
pub async fn detect_system_node(path_env_var: impl AsRef<OsStr>) -> Result<()> {
    let path_env_var = path_env_var.as_ref();

    // On Windows, we must use `cmd.exe /c node` so that the provided PATH
    // (set via `.env("PATH", ...)`) is used for executable search.
    // `CreateProcessW` uses the parent process's PATH, not the child's
    // `lpEnvironment` PATH, so running `node` directly would find node.exe
    // via Mnemos's inherited env rather than the captured interactive PATH.
    #[cfg(windows)]
    let output = Command::new("cmd.exe")
        .args(["/c", "node", "--version"])
        .env("PATH", path_env_var)
        .output()
        .await
        .context("Failed to run node --version. Is Node.js installed?")?;

    #[cfg(not(windows))]
    let output = Command::new("node")
        .env("PATH", path_env_var)
        .arg("--version")
        .output()
        .await
        .context("Failed to run node --version. Is Node.js installed?")?;

    if !output.status.success() {
        bail!(
            "node --version failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let version_str = String::from_utf8_lossy(&output.stdout);
    let version_str = version_str.trim().trim_start_matches('v');
    let version = Version::parse(version_str)
        .with_context(|| format!("Failed to parse Node.js version: {}", version_str))?;

    if version < MIN_NODE_VERSION {
        bail!(
            "System Node.js version {} is too old. Minimum required: {}",
            version,
            MIN_NODE_VERSION
        );
    }

    log::info!("Detected system Node.js {}", version);

    Ok(())
}

#[cfg(all(test, feature = "local_fs"))]
mod pinned_node_tests {
    use super::*;

    #[test]
    fn every_supported_platform_has_a_pinned_digest() {
        for (os, arch, ext) in [
            ("darwin", "arm64", "tar.gz"),
            ("darwin", "x64", "tar.gz"),
            ("linux", "arm64", "tar.gz"),
            ("linux", "x64", "tar.gz"),
            ("win", "arm64", "zip"),
            ("win", "x64", "zip"),
        ] {
            let name = format!("node-{NODE_VERSION}-{os}-{arch}.{ext}");
            let digest =
                pinned_node_sha256(&name).unwrap_or_else(|| panic!("no digest for {name}"));
            assert_eq!(digest.len(), 64);
        }
    }

    #[test]
    fn current_platform_archive_name_is_pinned() {
        if let Ok(dist) = NodeDistribution::current() {
            assert!(pinned_node_sha256(&dist.archive_file_name(NODE_VERSION)).is_some());
        }
    }

    #[test]
    fn mismatched_archive_is_refused() {
        let name = "node-v22.12.0-darwin-arm64.tar.gz";
        assert!(verify_node_archive(name, b"not node").is_err());
        assert!(verify_node_archive("node-unknown.tar.gz", b"x").is_err());
    }
}

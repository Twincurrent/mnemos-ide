use std::io::Write;
use std::path::PathBuf;

use anyhow::{bail, Context as _, Result};
use channel_versions::VersionInfo;
use instant::Duration;
use mnemos_core::channel::{Channel, ChannelState};
use mnemos_ui::{AppContext, ViewContext};

use crate::workspace::Workspace;

use super::release_assets_directory_url;
use super::{DownloadReady, ReadyForRelaunch};

lazy_static::lazy_static! {
    /// Stores the path to the current executable.
    ///
    /// We cache this before running auto-update because the returned path for
    /// a deleted file includes " (deleted)" _in the file name_, which breaks
    /// the relaunch logic.
    static ref CURRENT_EXE: std::io::Result<PathBuf> = std::env::current_exe();
}

// Mnemos IDE: upstream updated deb/rpm/pacman installs by pre-filling a package manager command
// (`sudo apt install mnemos-terminal`, ...) in a new tab. Mnemos publishes no package repository,
// so that command would fail, or install whatever a third-party repository publishes under that
// name. Packaged installs now get the "Update Mnemos manually" banner, which opens the release
// downloads; only AppImages update in place.

pub(super) async fn download_update_and_cleanup(
    version_info: &VersionInfo,
    _update_id: &str,
    client: &http_client::Client,
) -> Result<DownloadReady> {
    match UpdateMethod::detect() {
        UpdateMethod::Unknown => Ok(DownloadReady::NeedsAuthorization),
        UpdateMethod::AppImage(appimage_path) => {
            appimage::download_update_and_cleanup(version_info, &appimage_path, client).await
        }
        UpdateMethod::PackageManager(package_manager) => {
            log::info!(
                "Mnemos was installed with {package_manager}; it is updated by downloading the new release"
            );
            Ok(DownloadReady::NeedsAuthorization)
        }
    }
}

pub(super) fn apply_update(
    _initiating_workspace: &mut Workspace,
    _update_id: &str,
    _ctx: &mut ViewContext<Workspace>,
) -> Result<ReadyForRelaunch> {
    // Make sure CURRENT_EXE is initialized before we actually apply the update.
    let _ = CURRENT_EXE.as_ref();

    match UpdateMethod::detect() {
        UpdateMethod::AppImage(_) => Ok(ReadyForRelaunch::Yes),
        UpdateMethod::Unknown | UpdateMethod::PackageManager(_) => {
            bail!("Only an AppImage install can apply an update in place")
        }
    }
}

pub(super) fn relaunch() -> Result<()> {
    match UpdateMethod::detect() {
        UpdateMethod::AppImage(appimage_path) => appimage::relaunch(&appimage_path),
        UpdateMethod::Unknown | UpdateMethod::PackageManager(_) => {
            bail!("Only an AppImage install relaunches after an update")
        }
    }
}

/// Opens the page where the new release can be downloaded.
pub(super) fn manually_download_version(
    _channel: &Channel,
    _version_info: &VersionInfo,
    ctx: &mut AppContext,
) {
    ctx.open_url(crate::util::links::RELEASES_URL);
}

mod appimage {
    use std::path::Path;

    use super::*;

    pub(super) async fn download_update_and_cleanup(
        version_info: &VersionInfo,
        appimage_path: &Path,
        client: &http_client::Client,
    ) -> Result<DownloadReady> {
        const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

        // Compute the URL where we can download the new release.
        let Some(appimage_name) = option_env!("APPIMAGE_NAME") else {
            bail!("APPIMAGE_NAME environment variable was not set at compile time!");
        };

        // Mnemos IDE: nothing is downloaded unless the verified manifest gives its digest.
        let expected_digest = crate::autoupdate::signed_asset_digest(version_info, appimage_name)?;

        let url = format!(
            "{}/{}",
            release_assets_directory_url(ChannelState::channel(), &version_info.version),
            appimage_name
        );

        // Create a temporary file that we'll write the download into.
        let mut new_appimage = tempfile::NamedTempFile::new()?;

        log::info!("Downloading {url} to {}...", new_appimage.path().display());

        let response = client
            .get(&url)
            .timeout(DOWNLOAD_TIMEOUT)
            .send()
            .await?
            .error_for_status()?;
        new_appimage
            .as_file_mut()
            .write_all(&response.bytes().await?)?;
        new_appimage.as_file_mut().sync_all()?;

        // Mnemos IDE: the AppImage replaces the running one, so it has to be exactly what the
        // signed manifest names first.
        crate::autoupdate::verify_downloaded_asset(
            new_appimage.path(),
            appimage_name,
            &expected_digest,
        )?;

        log::info!(
            "Copying downloaded AppImage from {} to {}",
            new_appimage.path().display(),
            appimage_path.display()
        );

        // Copy permissions to new app before moving it to ensure we don't leave it
        // in a bad state if the move succeeds but we are unable to update the
        // permissions afterwards.
        new_appimage
            .as_file_mut()
            .set_permissions(appimage_path.metadata()?.permissions())?;

        // Move new AppImage over the one that launched the current Mnemos instance.
        let new_appimage_path = new_appimage.into_temp_path();
        let mv_status = command::r#async::Command::new("mv")
            .arg(new_appimage_path.as_os_str())
            .arg(appimage_path)
            .output()
            .await?
            .status;
        if !mv_status.success() {
            bail!("Failed to move new AppImage over the old one: {mv_status}");
        }

        // Ensure we don't accidentally drop `new_appimage_path` before we finish
        // moving it to its final location.
        let _ = new_appimage_path;

        Ok(DownloadReady::Yes)
    }

    pub(super) fn relaunch(appimage_path: &Path) -> Result<()> {
        let mut command = command::blocking::Command::new(appimage_path);
        // Pass a flag to the app to let it know it was restarted as part of the
        // autoupdate process.
        command.arg(mnemos_cli::finish_update_flag());
        // If we're testing with a local copy of channel_versions.json, have the
        // newly-started binary also reference that same file (so we can test
        // displaying an updated changelog after an autoupdate).
        if let Ok(path) = std::env::var("MNEMOS_CHANNEL_VERSIONS_PATH") {
            command.env("MNEMOS_CHANNEL_VERSIONS_PATH", path);
        }

        log::info!("Relaunching mnemos for update...");
        command.spawn()?;
        Ok(())
    }
}

/// Returns which method should be used to update Mnemos.
#[derive(Debug)]
pub(crate) enum UpdateMethod {
    /// We don't know how to update Mnemos.
    Unknown,
    /// Mnemos is running as an AppImage and should be updated in-place.
    AppImage(PathBuf),
    /// Mnemos can be updated using the given package manager.
    PackageManager(PackageManager),
}

impl UpdateMethod {
    pub(crate) fn detect() -> Self {
        if let Some(appimage_path) = std::env::var_os("APPIMAGE").map(PathBuf::from) {
            return Self::AppImage(appimage_path);
        }
        if let Ok(package_manager) = PackageManager::detect() {
            return Self::PackageManager(package_manager);
        }
        Self::Unknown
    }
}

/// Package managers whose installed packages we can recognise. Used only to tell a packaged
/// install apart from an AppImage or a bare binary.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum PackageManager {
    Apt,
    Yum,
    Dnf,
    Zypper,
    Pacman,
}

impl PackageManager {
    fn package_name() -> &'static str {
        package_name(ChannelState::channel())
    }

    fn detect() -> Result<Self> {
        let package_name = Self::package_name();

        let detect_script = r#"
            command -p pacman -Qi $PACKAGE_NAME >/dev/null 2>/dev/null
            if [ $? -eq 0 ]; then
              echo "pacman"
              exit
            fi

            command -p zypper search --match-exact --installed-only $PACKAGE_NAME >/dev/null 2>/dev/null
            if [ $? -eq 0 ]; then
              echo "zypper"
              exit
            fi

            command -p dnf list --installed $PACKAGE_NAME >/dev/null 2>/dev/null
            if [ $? -eq 0 ]; then
              echo "dnf"
              exit
            fi

            command -p yum list installed $PACKAGE_NAME >/dev/null 2>/dev/null
            if [ $? -eq 0 ]; then
              echo "yum"
              exit
            fi

            if [ "$(command -p dpkg-query --show --showformat='${db:Status-Status}' $PACKAGE_NAME 2>/dev/null)" = "installed" ]; then
              echo "apt"
              exit
            fi

            exit 1
        "#;

        let output = command::blocking::Command::new("sh")
            .args(["-c", detect_script])
            .env("PACKAGE_NAME", package_name)
            .output();
        match output {
            Ok(output) => {
                if !output.status.success() {
                    bail!("Failed to determine which package manager was used to install mnemos");
                }
                let Ok(stdout) = std::str::from_utf8(&output.stdout) else {
                    bail!("Could not parse package manager detection script output as UTF-8");
                };
                match stdout.trim() {
                    "pacman" => Ok(Self::Pacman),
                    "zypper" => Ok(Self::Zypper),
                    "dnf" => Ok(Self::Dnf),
                    "yum" => Ok(Self::Yum),
                    "apt" => Ok(Self::Apt),
                    _ => bail!(
                        "Received unexpected output from the package manager detection script"
                    ),
                }
            }
            Err(err) => Err(err).context("Failed to run package manager detection script"),
        }
    }
}

impl std::fmt::Display for PackageManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackageManager::Apt => write!(f, "apt"),
            PackageManager::Yum => write!(f, "yum"),
            PackageManager::Dnf => write!(f, "dnf"),
            PackageManager::Zypper => write!(f, "zypper"),
            PackageManager::Pacman => write!(f, "pacman"),
        }
    }
}

fn package_name(channel: Channel) -> &'static str {
    match channel {
        Channel::Stable => "mnemos-terminal",
        Channel::Preview => "mnemos-terminal-preview",
        Channel::Dev => "mnemos-terminal-dev",
        Channel::Integration => "mnemos-terminal-integration",
        Channel::Local => "mnemos-terminal-local",
    }
}

#[cfg(test)]
#[path = "linux_test.rs"]
mod tests;

use crate::server::telemetry::TelemetryEvent;
use anyhow::anyhow;
use anyhow::{bail, Result};
use channel_versions::{signed::AssetDigest, VersionInfo};
use command::blocking::Command;
use lazy_static::lazy_static;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;
use std::{fs, io};
use std::{io::Write as _, time::Duration};
use tempfile::TempPath;
use mnemos_core::channel::{Channel, ChannelState};
use mnemos_ui::AppContext;

use super::{release_assets_directory_url, DownloadReady};
use crate::util::windows::install_dir;

/// A downloaded installer and the digest the signed manifest gave for it.
struct DownloadedInstaller {
    path: TempPath,
    name: String,
    digest: AssetDigest,
}

lazy_static! {
    /// The temporary file that stores the installer for the new update.
    static ref INSTALLER: Arc<Mutex<Option<DownloadedInstaller>>> = Default::default();
}

/// Download the Inno Setup install wizard, the same one users run on the first Mnemos install, and
/// place it into the "data dir".
///
/// Mnemos IDE: upstream wrote to a fixed `%TEMP%\<version>-MnemosSetup.exe` and reused any file
/// already at that path without checking it, then ran it with /SILENT. The file now gets a random
/// name, is always downloaded fresh, and must match the digest from the signed manifest, both here
/// and again right before it runs (`relaunch`).
pub(super) async fn download_update_and_cleanup(
    version_info: &VersionInfo,
    _update_id: &str,
    client: &http_client::Client,
) -> Result<DownloadReady> {
    const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

    let installer_file_name = installer_file_name()?;
    let expected_digest = super::signed_asset_digest(version_info, &installer_file_name)?;
    let url = format!(
        "{}/{}",
        release_assets_directory_url(ChannelState::channel(), &version_info.version),
        installer_file_name
    );

    // A fresh, randomly named file that only this process created. Inno Setup needs the .exe
    // extension to run it.
    let mut new_installer = tempfile::Builder::new()
        .prefix("mnemos-update-")
        .suffix(&format!("-{installer_file_name}"))
        .tempfile()?;

    log::info!("Downloading {url} to {}...", new_installer.path().display());

    let response = client
        .get(&url)
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await?
        .error_for_status()?;
    new_installer
        .as_file_mut()
        .write_all(&response.bytes().await?)?;
    new_installer.as_file_mut().sync_all()?;

    super::verify_downloaded_asset(new_installer.path(), &installer_file_name, &expected_digest)?;

    *INSTALLER.lock() = Some(DownloadedInstaller {
        path: new_installer.into_temp_path(),
        name: installer_file_name,
        digest: expected_digest,
    });

    Ok(DownloadReady::Yes)
}

const UPDATE_LOG_FILENAME: &str = "mnemos_update.log";

fn autoupdate_log_file() -> Result<PathBuf> {
    mnemos_logging::log_directory().map(|dir| dir.join(UPDATE_LOG_FILENAME))
}

/// Checks the autoupdate log file from a previous update attempt.
/// Sends telemetry for specific known issues, and sends a Sentry event if errors are found.
/// The log file is renamed after processing to avoid duplicate reports on subsequent launches.
pub(super) fn check_and_report_update_errors(ctx: &mut AppContext) {
    let log_path = match autoupdate_log_file() {
        Ok(path) => path,
        Err(e) => {
            log::warn!("Failed to determine autoupdate log file path: {e:#}");
            return;
        }
    };

    // Inno Setup logs use the system's active codepage (often Windows-1252), not UTF-8.
    // We read as raw bytes to avoid silently skipping non-UTF-8 log files.
    let contents = match fs::read(&log_path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            log::info!("No autoupdate logs found");
            return;
        }
        Err(e) => {
            log::warn!("Failed to read autoupdate log file: {e:#}");
            return;
        }
    };

    let contents_lowercase = contents.to_ascii_lowercase();

    let has_unable_to_close = memchr::memmem::find(
        &contents_lowercase,
        b"setup was unable to automatically close all applications",
    )
    .is_some();
    if has_unable_to_close {
        crate::send_telemetry_sync_from_app_ctx!(
            TelemetryEvent::AutoupdateUnableToCloseApplications,
            ctx
        );
    }

    let has_file_in_use = memchr::memmem::find(
        &contents_lowercase,
        b"the process cannot access the file because it is being used by another process",
    )
    .is_some();
    if has_file_in_use {
        crate::send_telemetry_sync_from_app_ctx!(TelemetryEvent::AutoupdateFileInUse, ctx);
    }

    // Fired when the mutex polling loop timed out and a force-kill was attempted.
    let has_mutex_timeout =
        memchr::memmem::find(&contents_lowercase, b"mnemos mutex still held after timeout").is_some();
    if has_mutex_timeout {
        crate::send_telemetry_sync_from_app_ctx!(TelemetryEvent::AutoupdateMutexTimeout, ctx);
    }

    // Fired when taskkill returned non-zero after the mutex timeout.
    let has_forcekill_failed =
        memchr::memmem::find(&contents_lowercase, b"force-kill failed for").is_some();
    if has_forcekill_failed {
        crate::send_telemetry_sync_from_app_ctx!(TelemetryEvent::AutoupdateForcekillFailed, ctx);
    }

    #[cfg(feature = "crash_reporting")]
    {
        use sentry::protocol::{Attachment, AttachmentType};

        // Patterns for known benign errors that should not trigger Sentry reporting.
        const IGNOREABLE_ERRORS: &[&[u8]] = &[
            // User running out of disk space is not an error we need concern ourselves with.
            // This message occurs after "An error occurred while trying to copy a file:"
            b"there is not enough space on the disk",
            // Recent Inno Setup versions try to enable a security feature which is unavailable on
            // Windows 10 versions prior to 22H2 and this call fails. The failure is benign.
            b"setprocessmitigationpolicy failed with error code 87",
        ];

        let mut error_count = memchr::memmem::find_iter(&contents_lowercase, b"error").count();

        for pattern in IGNOREABLE_ERRORS {
            let ignoreable_count = memchr::memmem::find_iter(&contents_lowercase, pattern).count();
            error_count = error_count.saturating_sub(ignoreable_count);
        }

        if error_count > 0 {
            log::warn!("Autoupdate log file contains errors; reporting to Sentry");

            let attachment = Attachment {
                buffer: contents,
                filename: UPDATE_LOG_FILENAME.to_string(),
                ty: Some(AttachmentType::Attachment),
                ..Default::default()
            };
            sentry::with_scope(
                |scope| {
                    scope.add_attachment(attachment);
                },
                || sentry::capture_message("Windows auto-update error", sentry::Level::Error),
            );
        }
    }

    // Rename the log file to avoid duplicate reports on subsequent launches.
    // We keep the file around so the user can still view it or attach it to a GitHub issue.
    let reported_path = log_path.with_extension("log.reported");
    if let Err(e) = fs::rename(&log_path, &reported_path) {
        log::warn!("Failed to rename autoupdate log file after reporting: {e:#}");
    }
}

/// Runs the downloaded installer, which waits for this process to exit before replacing files.
/// With `relaunch_after` false (a normal quit with an update ready) the installer is told not to
/// start Mnemos again once it is done.
pub(super) fn relaunch(relaunch_after: bool) -> Result<()> {
    let install_dir = install_dir()?;
    let Some(installer) = INSTALLER.lock().take() else {
        bail!("No installer path");
    };
    // Mnemos IDE: check the bytes again right before running them; the file sat in %TEMP% since
    // the download. A mismatch deletes it (TempPath drop) and runs nothing.
    super::verify_downloaded_asset(&installer.path, &installer.name, &installer.digest)?;
    let installer_path = installer.path;

    let log_arg = match autoupdate_log_file() {
        Ok(dir) => format!("/LOG={}", dir.display()),
        Err(e) => {
            log::warn!("Failed to determine location for autoupdate logs: {e:#}");
            "/LOG".to_string()
        }
    };

    // The Inno Setup install wizard will run without user input. It re-launches Mnemos after
    // installing the update files, unless /norelaunch=1 is passed (a quit-time install).
    // https://jrsoftware.org/ishelp/index.php?topic=setupcmdline
    let dir_arg = format!("/DIR={}", install_dir.display());
    let mut args = vec![
        // Skip asking the user to confirm.
        "/SP-",
        // Do not prompt the user for anything. Note that we do not use "VERYSILENT" so that a
        // progress bar is still shown. This is useful since the update process may take a few
        // seconds.
        "/SILENT",
        // Do not provide a cancel button on the progress bar page.
        "/NOCANCEL",
        // Indicate that restarting Windows is not necessary.
        "/NORESTART",
        log_arg.as_str(),
        "/update=1",
        // Do not forcibly kill Mnemos via RestartManager. The installer will wait for
        // Mnemos to exit naturally by polling the single-instance mutex instead.
        "/NOCLOSEAPPLICATIONS",
        dir_arg.as_str(),
    ];
    if !relaunch_after {
        // Read by ShouldLaunchAfterInstall in windows-installer.iss. Installers published
        // before that check existed ignore it and relaunch once.
        args.push("/norelaunch=1");
    }
    Command::new(&installer_path).args(args).spawn()?;

    // DEV ONLY: Sleep after spawning the installer so this process is still alive
    // when Inno Setup tries to overwrite files. This reliably reproduces the
    // auto-update race condition (APP-3702) for testing.
    if matches!(ChannelState::channel(), Channel::Dev) {
        log::info!("DEV: Sleeping 10s after spawning installer to reproduce update race");
        std::thread::sleep(Duration::from_secs(10));
    }

    Ok(())
}

fn installer_file_name() -> Result<String> {
    let app_name_prefix = app_name_prefix(ChannelState::channel());

    // For example, on arm64 this is MnemosSetup-arm64.exe and on x64 this is
    // MnemosSetup.exe.
    if cfg!(target_arch = "aarch64") {
        Ok(format!("{app_name_prefix}Setup-arm64.exe"))
    } else if cfg!(target_arch = "x86_64") {
        Ok(format!("{app_name_prefix}Setup.exe"))
    } else {
        Err(anyhow!(
            "Could not construct setup file name for unsupported architecture"
        ))
    }
}

fn app_name_prefix(channel: Channel) -> &'static str {
    match channel {
        Channel::Stable => "Mnemos",
        Channel::Preview => "MnemosPreview",
        Channel::Local => "mnemos",
        Channel::Integration => "integration",
        Channel::Dev => "MnemosDev",
    }
}

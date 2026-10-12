use std::path::{Path, PathBuf};
use std::{
    env,
    fs::{self, File, TryLockError},
    io::{self, IsTerminal, Write, copy},
};

use anyhow::Result;
use chrono::Local;
use log::LevelFilter;
use std::sync::OnceLock;
use mnemos_core::features::FeatureFlag;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use crate::{LogConfig, LogDestination};
use mnemos_core::channel::ChannelState;

const MAX_FILES_IN_GUI_ROTATION: usize = 5;
const MAX_FILES_IN_CLI_ROTATION: usize = 10;
const CLI_LOG_SUBDIRECTORY: &str = "oz";
const TEMP_LOG_FILE_SUFFIX: &str = "old.temp";

/// Runtime logging state, computed from `LogConfig` during initialization.
#[derive(Debug)]
struct LogState {
    /// Whether or not logs should be written to a file.
    use_logfile: bool,

    /// The directory that logs should be written to. This is set even if `use_logfile` is false,
    /// as we sometimes generate other log files.
    log_directory: PathBuf,

    /// The maximum number of backup log files to keep during rotation.
    max_rotation: usize,

    /// Whether this process owns the main log and may rotate it. False only when another
    /// live process held the main log lock at startup; this process then writes to the
    /// secondary log and leaves the main log and its rotation alone.
    owns_main_log: bool,
}

static LOG_STATE: OnceLock<LogState> = OnceLock::new();

/// Handle holding the exclusive lock on `<logfile_name>.lock`, which marks this process as the
/// owner of the main log. It stays open (and locked) until the process exits, and the OS
/// releases the lock when the process dies, so a crashed owner never blocks the next launch.
static LOG_LOCK: OnceLock<File> = OnceLock::new();

/// Formats a log record to be output to the terminal.
fn format_for_terminal_output(
    buf: &mut env_logger::fmt::Formatter,
    record: &log::Record,
) -> std::io::Result<()> {
    let level = record.level();
    let mut level_style = buf.default_level_style(record.level());
    // Adjust colors to match what we're used to from simplelog.
    match &level {
        log::Level::Info => {
            level_style.set_color(env_logger::fmt::Color::Blue);
        }
        log::Level::Debug => {
            level_style.set_color(env_logger::fmt::Color::Green);
        }
        _ => {}
    }
    let level = level_style.value(format!("[{level}]"));

    let mut target_style = buf.style();
    let target = if cfg!(debug_assertions) {
        target_style.set_dimmed(true);
        target_style.value(format!("[{}] ", record.target()))
    } else {
        target_style.value(String::default())
    };

    let time = chrono::Local::now();
    writeln!(
        buf,
        "{} {level} {target}{}",
        time.format("%H:%M:%S%.3f"),
        record.args()
    )
}

/// Formats a log record to be output to a file.
fn format_for_file_output(
    buf: &mut env_logger::fmt::Formatter,
    record: &log::Record,
) -> std::io::Result<()> {
    let target = if cfg!(debug_assertions) {
        format!("[{}] ", record.target())
    } else {
        String::default()
    };

    writeln!(
        buf,
        "{} [{}] {}{}",
        buf.timestamp(),
        record.level(),
        target,
        record.args()
    )
}

/// Handles the crash recovery process being killed by removing the crash recovery process log file
/// (which is stored in a temp directory and only persisted if the crash recovery process actually
/// handled a crash in the parent process).
pub fn on_crash_recovery_process_killed() {
    let config = LOG_STATE.get().expect("Logging not initialized");
    if !config.use_logfile {
        return;
    }

    let _ = fs::remove_file(crash_recovery_process_log_file_path(&config.log_directory));
}

/// Handles the crash recovery process "recovering" from a parent crash by:
/// 1) Renaming the log file from the main process (which just panicked) to `mnemos.log.old.temp`.
/// 2) Moving the crash recovery process log (which is located at `mnemos.log.recovery`) to the usual
///    path mnemos logs are located (log_directory/mnemos.log).
///    The temp log file (`mnemos.log.old.temp`) will ultimately be rotated to `mnemos.log.old.0` the next
///    time [`rotate_log_files`] is called (which will get called when the event loop starts and we
///    have access to the `AppContext`)
pub fn on_parent_process_crash() {
    let config = LOG_STATE.get().expect("Logging not initialized");
    if !config.use_logfile {
        return;
    }

    let main_log_path = main_process_log_file_path(&config.log_directory);
    let temp_path = temp_log_file_path(&config.log_directory);

    let _ = fs::rename(&main_log_path, temp_path);

    let _ = fs::rename(
        crash_recovery_process_log_file_path(&config.log_directory),
        main_log_path,
    );

    // The crashed parent's lock was released when it died. Take it over, best effort, so a
    // later launch sees this process as the owner of the main log it now writes to.
    if let MainLogLock::Acquired(lock) =
        try_lock_main_log(&config.log_directory, &ChannelState::logfile_name())
    {
        let _ = LOG_LOCK.set(lock);
    }
}

/// Rotates the log and telemetry files, such that:
/// - Each file stores the logs of a single execution.
/// - The .old files store the previous executions, with larger suffixes indicating older executions.
pub async fn rotate_log_files() {
    let config = LOG_STATE.get().expect("Logging not initialized");
    // A process that does not own the main log must not rotate it: the owner is still
    // writing to it.
    if !config.use_logfile || !config.owns_main_log {
        return;
    }

    let max_rotation = config.max_rotation;

    if let Err(err) = rotate_files(&ChannelState::logfile_name(), max_rotation).await {
        log::error!("Failed to rotate log files: {err:?}");
    }

    if FeatureFlag::SendTelemetryToFile.is_enabled()
        && let Err(err) = rotate_files(&ChannelState::telemetry_file_name(), max_rotation).await
    {
        log::error!("Failed to rotate telemetry files: {err:?}");
    }
}

pub async fn rotate_files(channel_file_name: &str, max_rotation: usize) -> Result<()> {
    let log_directory = match log_directory() {
        Ok(log_directory) => log_directory,
        Err(err) => {
            return Err(anyhow::anyhow!("Could not get log directory {err:?}"));
        }
    };

    // Delete the oldest log file.
    let largest_log_file_suffix = max_rotation.saturating_sub(1);
    let _ = fs::remove_file(
        log_directory.join(format!("{channel_file_name}.old.{largest_log_file_suffix}")),
    );

    // Rotate the log files.
    for file_no in (0..largest_log_file_suffix).rev() {
        let old_file_path = log_directory.join(format!("{channel_file_name}.old.{file_no}"));
        let new_file_path = log_directory.join(format!("{channel_file_name}.old.{}", file_no + 1));
        let _ = fs::rename(old_file_path, new_file_path);
    }

    // Rename `mnemos.log.old.temp` (the temporary file) to `mnemos.log.old.0`.
    let temp_file_path = temp_log_file_path(&log_directory);

    let _ = fs::rename(
        temp_file_path,
        log_directory.join(format!("{channel_file_name}.old.0")),
    );

    Ok(())
}

/// Initializes the logger for the crash recovery process.
pub fn init_for_crash_recovery_process() -> Result<()> {
    init_internal(
        true,  /* is_from_crash_recovery_process */
        false, /* is_cli */
        None,  /* log_destination */
    )
}

/// Initializes the global logger for the application.
/// If `config.log_destination` is `Some`, always use the specified destination regardless of
/// environment. If `config.is_cli` is true, logs are written to a separate "oz" subdirectory with
/// a higher rotation limit so that CLI invocations don't evict GUI application logs.
pub fn init(config: LogConfig) -> Result<()> {
    init_internal(
        false, /* is_from_crash_recovery_process */
        config.is_cli,
        config.log_destination,
    )
}

/// Return the path to the log file that is used within the crash recovery process.
/// We use a separate log file for the crash recovery process. If the crash
/// recovery process handles a crash, we'll move the crash recovery process log file to its usual
/// location at `log_directory/mnemos.log`.
fn crash_recovery_process_log_file_path(log_directory: impl AsRef<Path>) -> PathBuf {
    log_directory
        .as_ref()
        .join(recovery_log_file_name(&ChannelState::logfile_name()))
}

/// File name of the crash recovery process's log for the main log `logfile_name`.
fn recovery_log_file_name(logfile_name: &str) -> String {
    format!("{logfile_name}.recovery")
}

/// File name the previous execution's main log is renamed to until it is rotated.
fn temp_log_file_name(logfile_name: &str) -> String {
    format!("{logfile_name}.{TEMP_LOG_FILE_SUFFIX}")
}

/// File name of the lock that marks the owner of the main log `logfile_name`. The lock file
/// is never written, truncated, renamed or deleted, so every process locks the same file.
fn lock_file_name(logfile_name: &str) -> String {
    format!("{logfile_name}.lock")
}

/// File name used by a process that found the main log owned by another live process:
/// `mnemos.log` gives `mnemos.secondary.log`.
fn secondary_log_file_name(logfile_name: &str) -> String {
    let stem = logfile_name.strip_suffix(".log").unwrap_or(logfile_name);
    format!("{stem}.secondary.log")
}

/// Returns the path to the main process's log file.
fn main_process_log_file_path(log_directory: impl AsRef<Path>) -> PathBuf {
    log_directory.as_ref().join(&*ChannelState::logfile_name())
}

/// Returns the path to the current execution's main log file, or to the secondary log when
/// another process owns the main log.
///
/// Note: logging must be initialized before calling this function, otherwise this will
/// return an error.
pub fn log_file_path() -> Result<PathBuf> {
    let config = LOG_STATE
        .get()
        .ok_or_else(|| anyhow::anyhow!("Logging not initialized"))?;
    if !config.owns_main_log {
        return Ok(config
            .log_directory
            .join(secondary_log_file_name(&ChannelState::logfile_name())));
    }
    Ok(main_process_log_file_path(&config.log_directory))
}

/// Collects a list of the paths to both the current mnemos instance's log file,
/// and any older log files (we keep up to 6 log files around at any time,
/// all of which are potentially useful for debugging).
fn current_and_rotated_log_paths() -> Result<Vec<PathBuf>> {
    let log_directory = log_directory()?;
    let current_log_path = main_process_log_file_path(&log_directory);

    let mut rotated_logs: Vec<(usize, PathBuf)> = fs::read_dir(&log_directory)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter_map(|path| {
            let file_name = path.file_name()?.to_str()?;
            let suffix =
                file_name.strip_prefix(&format!("{}.old.", ChannelState::logfile_name()))?;
            let index = suffix.parse::<usize>().ok()?;
            Some((index, path))
        })
        .collect();
    rotated_logs.sort_by_key(|(index, _)| *index);

    let mut files = Vec::new();
    if current_log_path.is_file() {
        files.push(current_log_path);
    }

    files.extend(
        rotated_logs
            .into_iter()
            .map(|(_, path)| path)
            .filter(|path| path.is_file()),
    );

    if files.is_empty() {
        return Err(anyhow::anyhow!(
            "No mnemos logs were found for {}",
            ChannelState::logfile_name()
        ));
    }

    Ok(files)
}

/// Creates a timestamped zip archive containing the current log file
/// and any older logs for the active instance.
pub fn create_log_bundle_zip() -> Result<PathBuf> {
    let log_files = current_and_rotated_log_paths()?;
    let log_directory = log_directory()?;
    let logfile_name = ChannelState::logfile_name();
    let logfile_stem = logfile_name.strip_suffix(".log").unwrap_or(&logfile_name);

    let zip_path = log_directory.join(format!(
        "{logfile_stem}-{}.zip",
        Local::now().format("%Y%m%d-%H%M%S")
    ));
    if zip_path.exists() {
        let error_message = format!(
            "New log zip path conflicts with an existing zip: {}",
            zip_path.display()
        );
        return Err(anyhow::anyhow!("{error_message}"));
    }

    let zip_file = File::create(&zip_path)?;
    let mut zip_writer = ZipWriter::new(zip_file);
    let zip_options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    for log_file in log_files {
        let entry_name = log_file
            .file_name()
            .and_then(|file_name| file_name.to_str())
            .ok_or_else(|| anyhow::anyhow!("Invalid log file name: {}", log_file.display()))?;
        zip_writer.start_file(entry_name, zip_options)?;

        let mut source = File::open(&log_file)?;
        copy(&mut source, &mut zip_writer)?;
    }

    zip_writer.finish()?;
    Ok(zip_path)
}

fn temp_log_file_path(log_directory: impl AsRef<Path>) -> PathBuf {
    log_directory
        .as_ref()
        .join(temp_log_file_name(&ChannelState::logfile_name()))
}

/// Outcome of trying to take the main log lock.
#[derive(Debug)]
enum MainLogLock {
    /// This process holds the lock. The handle must stay open for the life of the process.
    Acquired(File),
    /// Another live process holds the lock and therefore owns the main log.
    HeldElsewhere,
    /// The lock could not be checked (no log name, or the lock file could not be opened or
    /// locked). Callers fall back to the behaviour from before the lock existed.
    Unavailable(Option<io::Error>),
}

/// Tries to take the exclusive lock on `<log_directory>/<logfile_name>.lock` without blocking.
///
/// Only the separate `.lock` file is locked, never the log itself: Windows locks are
/// mandatory and would block readers of the log such as the log bundle. Errors never
/// propagate, since failing here must not stop the app from starting.
fn try_lock_main_log(log_directory: &Path, logfile_name: &str) -> MainLogLock {
    if logfile_name.is_empty() {
        return MainLogLock::Unavailable(None);
    }

    let lock_file = match fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(log_directory.join(lock_file_name(logfile_name)))
    {
        Ok(file) => file,
        Err(err) => return MainLogLock::Unavailable(Some(err)),
    };

    match lock_file.try_lock() {
        Ok(()) => MainLogLock::Acquired(lock_file),
        Err(TryLockError::WouldBlock) => MainLogLock::HeldElsewhere,
        Err(TryLockError::Error(err)) => MainLogLock::Unavailable(Some(err)),
    }
}

/// The log file for the current execution, and whether this process owns the main log.
#[derive(Debug)]
struct ExecutionLogFiles {
    /// The file this execution writes its log to.
    log_file: File,

    /// The main log lock, when this process took it. The caller keeps it for the life of the
    /// process.
    lock: Option<File>,

    /// False only when another live process holds the main log lock.
    owns_main_log: bool,

    /// Why the main log lock could not be checked, if it failed with an error. The log is
    /// still set up as it was before the lock existed.
    lock_error: Option<io::Error>,
}

/// Opens the file for the current execution's log in `log_directory`, for the main log named
/// `logfile_name`.
///
/// - Crash recovery process: truncates and returns `<name>.recovery`. It takes no lock and
///   does not touch the main log, which belongs to the parent process.
/// - The main log lock is free (or cannot be checked): takes the lock, renames the previous
///   execution's `<name>` to `<name>.old.temp` (rotated later in the background to make fewer
///   blocking calls), and returns a new, empty `<name>`.
/// - Another live process holds the lock: leaves `<name>` and its rotation alone and returns
///   a truncated `<stem>.secondary.log` instead.
fn setup_log_files_for_current_execution(
    log_directory: &Path,
    logfile_name: &str,
    is_from_crash_recovery_process: bool,
) -> Result<ExecutionLogFiles> {
    fn open_truncated(path: PathBuf) -> io::Result<File> {
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
    }

    fs::create_dir_all(log_directory)?;

    if is_from_crash_recovery_process {
        // We intentionally do not rename the old main log file like we do below, because that
        // would move the log file of the parent process.
        return Ok(ExecutionLogFiles {
            log_file: open_truncated(log_directory.join(recovery_log_file_name(logfile_name)))?,
            lock: None,
            owns_main_log: true,
            lock_error: None,
        });
    }

    let (lock, lock_error) = match try_lock_main_log(log_directory, logfile_name) {
        MainLogLock::Acquired(lock) => (Some(lock), None),
        MainLogLock::HeldElsewhere => {
            return Ok(ExecutionLogFiles {
                log_file: open_truncated(
                    log_directory.join(secondary_log_file_name(logfile_name)),
                )?,
                lock: None,
                owns_main_log: false,
                lock_error: None,
            });
        }
        MainLogLock::Unavailable(err) => (None, err),
    };

    let main_log_path = log_directory.join(logfile_name);
    let _ = fs::rename(
        &main_log_path,
        log_directory.join(temp_log_file_name(logfile_name)),
    );

    Ok(ExecutionLogFiles {
        log_file: open_truncated(main_log_path)?,
        lock,
        owns_main_log: true,
        lock_error,
    })
}

#[cfg(feature = "crash_reporting")]
fn sentry_log_filter(md: &log::Metadata) -> sentry_log::LogFilter {
    if mnemos_core::errors::should_ignore_log_for_sentry(md) {
        return sentry_log::LogFilter::Ignore;
    }

    match md.target() {
        // Ignore any log lines that come from the `log_panics` crate.
        "panic" => sentry_log::LogFilter::Ignore,

        // Filter out spammy INFO-level log lines from wgpu.
        t if t.starts_with("wgpu_core") || t.starts_with("wgpu_hal") => {
            sentry_log::LogFilter::Ignore
        }

        // Filter out the "redraw_frame" logging from breadcrumbs.
        "mnemos_ui::core::redraw_frame" => sentry_log::LogFilter::Ignore,

        // Filter out logs from the crash-reporting implementation, in case it logs
        // anything in the process of forwarding logs to Sentry.
        t if t.starts_with("mnemos::crash_reporting::") => sentry_log::LogFilter::Ignore,

        _ => sentry_log::default_filter(md),
    }
}

fn init_internal(
    is_from_crash_recovery_process: bool,
    is_cli: bool,
    log_destination: Option<LogDestination>,
) -> Result<()> {
    let mut base_logger = env_logger::builder();

    base_logger.filter_level(LevelFilter::Info);

    // Only include `WARN` or higher logs for wgpu. By default, wgpu outputs logs at the `INFO`
    // level multiple times _per_ frame. See https://github.com/gfx-rs/wgpu/issues/3206.
    // Naga is overly noisy at `DEBUG`, so increase to `INFO`.
    base_logger
        .filter(Some("naga"), LevelFilter::Info)
        .filter(Some("wgpu_core"), LevelFilter::Warn)
        // Since we always pair an insertion with a deletion to avoid duplicate,
        // tantivy will log a lot of warnings for deleting a non-existing doc.
        .filter(Some("tantivy"), LevelFilter::Error)
        .filter(
            Some("wgpu_hal"),
            // On Windows with the DX12 backend, wgpu_hal outputs a ton of WARN-level logs.
            if cfg!(windows) {
                LevelFilter::Error
            } else {
                LevelFilter::Warn
            },
        );
    base_logger.parse_default_env();

    let stdout_is_a_tty = std::io::stdout().is_terminal();
    let in_ci = env::var("CI").is_ok();
    let integration_test = env::var("MNEMOS_INTEGRATION").is_ok();
    let use_logfile = match log_destination {
        Some(LogDestination::File) => true,
        Some(LogDestination::Stderr) => false,
        None => !stdout_is_a_tty && !in_ci && !integration_test,
    };

    let max_rotation = if is_cli {
        MAX_FILES_IN_CLI_ROTATION
    } else {
        MAX_FILES_IN_GUI_ROTATION
    };

    let mut log_directory = init_log_directory()?;
    if is_cli {
        log_directory = log_directory.join(CLI_LOG_SUBDIRECTORY);
    }
    let mut owns_main_log = true;
    let mut lock_error = None;
    if use_logfile {
        let files = setup_log_files_for_current_execution(
            &log_directory,
            &ChannelState::logfile_name(),
            is_from_crash_recovery_process,
        )?;
        owns_main_log = files.owns_main_log;
        lock_error = files.lock_error;
        if let Some(lock) = files.lock {
            // Held until the process exits. `set` can only fail if logging was already
            // initialized, which panics below.
            let _ = LOG_LOCK.set(lock);
        }
        base_logger.target(env_logger::Target::Pipe(Box::new(files.log_file)));
        base_logger.format(format_for_file_output);
    } else {
        // Agent mode eval outputs are written to stdout but redirected to a file, so we don't want terminal styling.
        if cfg!(feature = "agent_mode_evals") {
            base_logger.write_style(env_logger::WriteStyle::Never);
        } else {
            base_logger.write_style(env_logger::WriteStyle::Always);
        }
        base_logger.format(format_for_terminal_output);
    }

    #[cfg(feature = "crash_reporting")]
    {
        let base_logger = base_logger.build();
        log::set_max_level(base_logger.filter());
        let logger = sentry_log::SentryLogger::with_dest(base_logger).filter(sentry_log_filter);
        log::set_boxed_logger(Box::new(logger))
            .expect("Should not have already initialized a logger");
    }

    #[cfg(not(feature = "crash_reporting"))]
    base_logger.init();

    // If we're logging to a file, initialize the `log_panics` crate, which
    // will install a panic hook that writes out panics using `log::error`.
    if use_logfile {
        log_panics::init();
    }

    if !owns_main_log {
        log::warn!(
            "Another process owns {} in {}; this process logs to {} and skips log rotation",
            ChannelState::logfile_name(),
            log_directory.display(),
            secondary_log_file_name(&ChannelState::logfile_name()),
        );
    }
    if let Some(err) = lock_error {
        log::warn!("Could not lock the main log, continuing without the lock: {err}");
    }

    LOG_STATE
        .set(LogState {
            use_logfile,
            log_directory,
            max_rotation,
            owns_main_log,
        })
        .expect("Logging already initialized");
    // We can .expect here because .init would have already panicked if we initialized logging twice.

    Ok(())
}

pub fn log_directory() -> Result<std::path::PathBuf> {
    LOG_STATE
        .get()
        .map(|config| config.log_directory.clone())
        .ok_or_else(|| anyhow::anyhow!("Logging not initialized"))
}

fn init_log_directory() -> Result<std::path::PathBuf> {
    cfg_if::cfg_if! {
        if #[cfg(target_os = "macos")] {
            Ok(dirs::home_dir()
                .ok_or_else(|| {
                    anyhow::anyhow!("could not locate home directory in order to create a log file")
                })?
                .join("Library/Logs/"))
        } else if #[cfg(target_os = "linux")] {
            Ok(mnemos_core::paths::state_dir())
        } else if #[cfg(windows)] {
            Ok(mnemos_core::paths::state_dir().join(mnemos_core::paths::MNEMOS_LOGS_DIR))
        } else {
            Err(anyhow::anyhow!("Have not configured file-based logging for the current platform!"))
        }
    }
}

/// Initializes the logger before running tests.
///
/// Additionally, we must not write anything to stdout in this function, as it
/// can interfere with test harnesses collecting the set of tests to run.  (This
/// is why we're not simply calling the init() function above.)
pub fn init_logging_for_unit_tests() {
    env_logger::builder()
        .is_test(true)
        .filter_level(LevelFilter::Info)
        .write_style(env_logger::WriteStyle::Always)
        .parse_default_env()
        .format(format_for_terminal_output)
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, empty directory under the system temp dir, removed on drop.
    struct TempLogDir(PathBuf);

    impl TempLogDir {
        fn new(test: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            let dir = env::temp_dir().join(format!(
                "mnemos_logging_{test}_{}_{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).expect("create temp log dir");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempLogDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn open_lock_file(dir: &Path, name: &str) -> File {
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(lock_file_name(name)))
            .expect("open lock file")
    }

    #[test]
    fn setup_with_a_lock_held_elsewhere_leaves_the_main_log_alone() {
        let dir = TempLogDir::new("held_elsewhere");
        let name = "mnemos.log";
        fs::write(dir.path().join(name), "live session\n").expect("seed main log");

        let holder = open_lock_file(dir.path(), name);
        holder
            .try_lock()
            .expect("take the lock as the other process");

        let files = setup_log_files_for_current_execution(dir.path(), name, false)
            .expect("setup should succeed");

        assert!(!files.owns_main_log);
        assert!(files.lock.is_none());
        assert!(files.lock_error.is_none());
        assert_eq!(
            fs::read_to_string(dir.path().join(name)).expect("read main log"),
            "live session\n"
        );
        assert!(!dir.path().join(temp_log_file_name(name)).exists());
        assert!(dir.path().join("mnemos.secondary.log").is_file());

        // The secondary log is what the logger writes to.
        let mut log_file = files.log_file;
        log_file.write_all(b"secondary\n").expect("write log");
        assert_eq!(
            fs::read_to_string(dir.path().join("mnemos.secondary.log")).expect("read"),
            "secondary\n"
        );
        drop(holder);
    }

    #[test]
    fn setup_without_a_holder_renames_the_previous_log_and_takes_the_lock() {
        let dir = TempLogDir::new("no_holder");
        let name = "mnemos.log";
        fs::write(dir.path().join(name), "previous session\n").expect("seed main log");

        let files = setup_log_files_for_current_execution(dir.path(), name, false)
            .expect("setup should succeed");

        assert!(files.owns_main_log);
        assert!(files.lock_error.is_none());
        assert_eq!(
            fs::read_to_string(dir.path().join(temp_log_file_name(name))).expect("read temp"),
            "previous session\n"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join(name)).expect("read main log"),
            ""
        );
        assert!(!dir.path().join("mnemos.secondary.log").exists());

        let lock = files
            .lock
            .expect("the lock should be returned to the caller");
        let other = open_lock_file(dir.path(), name);
        assert!(matches!(other.try_lock(), Err(TryLockError::WouldBlock)));

        // A second setup while the lock is held behaves like a second process.
        let second = setup_log_files_for_current_execution(dir.path(), name, false)
            .expect("second setup should succeed");
        assert!(!second.owns_main_log);
        assert_eq!(
            fs::read_to_string(dir.path().join(temp_log_file_name(name))).expect("read temp"),
            "previous session\n"
        );

        // Once the owner is gone the lock is free again.
        drop(lock);
        other
            .try_lock()
            .expect("lock should be free after the owner drops it");
    }

    #[test]
    fn setup_for_crash_recovery_uses_the_recovery_log_without_a_lock() {
        let dir = TempLogDir::new("recovery");
        let name = "mnemos.log";
        fs::write(dir.path().join(name), "parent session\n").expect("seed main log");

        let files = setup_log_files_for_current_execution(dir.path(), name, true)
            .expect("setup should succeed");

        assert!(files.owns_main_log);
        assert!(files.lock.is_none());
        assert!(dir.path().join("mnemos.log.recovery").is_file());
        assert_eq!(
            fs::read_to_string(dir.path().join(name)).expect("read main log"),
            "parent session\n"
        );
        assert!(!dir.path().join(temp_log_file_name(name)).exists());
        assert!(!dir.path().join(lock_file_name(name)).exists());
    }

    #[test]
    fn empty_log_name_skips_the_lock() {
        let dir = TempLogDir::new("empty_name");
        assert!(matches!(
            try_lock_main_log(dir.path(), ""),
            MainLogLock::Unavailable(None)
        ));
        assert!(!dir.path().join(".lock").exists());
    }

    #[test]
    fn secondary_log_name_replaces_the_log_extension() {
        assert_eq!(
            secondary_log_file_name("mnemos.log"),
            "mnemos.secondary.log"
        );
        assert_eq!(
            secondary_log_file_name("mnemos-debug.log"),
            "mnemos-debug.secondary.log"
        );
        assert_eq!(secondary_log_file_name("mnemos"), "mnemos.secondary.log");
    }
}

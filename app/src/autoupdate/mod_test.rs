use chrono::{Local, TimeZone};
use mnemos_ui::{App, ModelHandle, ReadModel, UpdateModel};

use crate::{
    auth::{AuthManager, AuthStateProvider},
    server::{
        server_api::ServerApiProvider, telemetry::context_provider::AppTelemetryContextProvider,
    },
};

use mnemos_core::execution_mode::{AppExecutionMode, ExecutionMode};

use super::*;

fn register_test_dependencies(app: &mut App) -> Arc<ServerApi> {
    let server_api_provider = app.add_singleton_model(|_| ServerApiProvider::new_for_test());
    app.add_singleton_model(|_| AuthStateProvider::new_for_test());
    app.add_singleton_model(AppTelemetryContextProvider::new_context_provider);
    app.add_singleton_model(AuthManager::new_for_test);

    app.read_model(&server_api_provider, |server_api_provider, _| {
        server_api_provider.get()
    })
}

fn initialize_app(app: &mut App) -> ModelHandle<AutoupdateState> {
    let server_api = register_test_dependencies(app);
    app.add_model(|_| AutoupdateState::new(server_api))
}

/// Registers AutoupdateState and RelaunchModel as singletons, for the free functions
/// (apply_pending_update, cancel_relaunch) that look them up with `handle`.
fn initialize_singletons(app: &mut App) {
    let server_api = register_test_dependencies(app);
    app.add_singleton_model(|_| AutoupdateState::new(server_api));
    app.add_singleton_model(|_| RelaunchModel::new());
}

/// `ChannelState::set_app_version` writes process-global state, so the tests that set it
/// take this lock and run one at a time. Without it `test_should_update` can swap the
/// version under another test between its setup and its assertions.
static APP_VERSION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_app_version() -> std::sync::MutexGuard<'static, ()> {
    APP_VERSION_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[test]
fn test_queueing_behavior() {
    App::test((), |mut app| async move {
        app.add_singleton_model(|ctx| AppExecutionMode::new(ExecutionMode::App, false, ctx));
        let autoupdate_state = initialize_app(&mut app);

        app.update_model(&autoupdate_state, |autoupdate, ctx| {
            assert_eq!(autoupdate.get_next_request(ctx), None);
            autoupdate.request_queue.push_back(RequestType::DailyCheck);
            assert_eq!(autoupdate.request_queue.len(), 1);

            autoupdate.stage = AutoupdateStage::DownloadingUpdate;
            assert_eq!(autoupdate.get_next_request(ctx), None);
            assert_eq!(autoupdate.request_queue.len(), 1);

            autoupdate.stage = AutoupdateStage::NoUpdateAvailable;
            assert_eq!(
                autoupdate.get_next_request(ctx),
                Some(RequestType::DailyCheck)
            );
            assert_eq!(autoupdate.request_queue.len(), 0);

            autoupdate.request_queue.push_back(RequestType::Poll);
            autoupdate.request_queue.push_back(RequestType::DailyCheck);
            autoupdate.request_queue.push_back(RequestType::ManualCheck);
            assert_eq!(autoupdate.request_queue.len(), 3);
            assert_eq!(autoupdate.get_next_request(ctx), Some(RequestType::Poll));
            autoupdate.stage = AutoupdateStage::CheckingForUpdate;
            assert_eq!(autoupdate.get_next_request(ctx), None);
            assert_eq!(autoupdate.request_queue.len(), 2);
            autoupdate.stage = AutoupdateStage::NoUpdateAvailable;
            assert_eq!(
                autoupdate.get_next_request(ctx),
                Some(RequestType::DailyCheck)
            );
            assert_eq!(
                autoupdate.get_next_request(ctx),
                Some(RequestType::ManualCheck)
            );
            assert_eq!(autoupdate.request_queue.len(), 0);
        });
    });
}

#[test]
fn test_queue_behavior_sdk_mode() {
    App::test((), |mut app| async move {
        app.add_singleton_model(|ctx| AppExecutionMode::new(ExecutionMode::Sdk, false, ctx));
        let autoupdate_state = initialize_app(&mut app);

        app.update_model(&autoupdate_state, |autoupdate, ctx| {
            assert_eq!(autoupdate.get_next_request(ctx), None);
            autoupdate.request_queue.push_back(RequestType::DailyCheck);
            assert_eq!(autoupdate.request_queue.len(), 1);

            // The daily check should be ignored in SDK mode.
            assert_eq!(autoupdate.get_next_request(ctx), None);

            // Polling should be ignored in SDK mode.
            autoupdate.request_queue.push_back(RequestType::Poll);
            assert_eq!(autoupdate.request_queue.len(), 1);
            assert_eq!(autoupdate.get_next_request(ctx), None);

            // Manual checks should not be ignored in SDK mode.
            autoupdate.request_queue.push_back(RequestType::ManualCheck);
            assert_eq!(autoupdate.request_queue.len(), 1);
            assert_eq!(
                autoupdate.get_next_request(ctx),
                Some(RequestType::ManualCheck)
            );

            // If there are ignored requests, the queue skips to the next non-ignored request.
            autoupdate.request_queue.push_back(RequestType::Poll);
            autoupdate.request_queue.push_back(RequestType::Poll);
            autoupdate.request_queue.push_back(RequestType::DailyCheck);
            autoupdate.request_queue.push_back(RequestType::ManualCheck);
            assert_eq!(autoupdate.request_queue.len(), 4);
            assert_eq!(
                autoupdate.get_next_request(ctx),
                Some(RequestType::ManualCheck)
            );
            assert_eq!(autoupdate.request_queue.len(), 0);
        });
    });
}

/// In SDK (CLI) mode, `poll_for_update` must not cause any actual update check to run.
/// Poll and DailyCheck requests are discarded by `get_next_request` so that the autoupdate
/// state machine stays at `NoUpdateAvailable`. This ensures the CLI never kicks off a
/// background update-check loop.
#[test]
fn test_cli_sdk_mode_prevents_autoupdate_polling() {
    App::test((), |mut app| async move {
        app.add_singleton_model(|ctx| AppExecutionMode::new(ExecutionMode::Sdk, false, ctx));
        let autoupdate_state = initialize_app(&mut app);

        app.update_model(&autoupdate_state, |autoupdate, ctx| {
            // Simulate what the autoupdate poll loop would do: call poll_for_update.
            // In SDK mode, the Poll request enqueued by poll_for_update must be discarded
            // immediately without initiating a check (stage stays NoUpdateAvailable).
            autoupdate.poll_for_update(ctx);
            assert!(
                matches!(autoupdate.stage, AutoupdateStage::NoUpdateAvailable),
                "Stage must not advance to CheckingForUpdate in SDK mode"
            );
            assert_eq!(
                autoupdate.request_queue.len(),
                0,
                "Poll request must be discarded, not left in the queue"
            );

            // DailyCheck requests must also be discarded.
            autoupdate.request_queue.push_back(RequestType::DailyCheck);
            autoupdate.try_execute_request(ctx);
            assert!(
                matches!(autoupdate.stage, AutoupdateStage::NoUpdateAvailable),
                "DailyCheck must not trigger a check in SDK mode"
            );
            assert_eq!(autoupdate.request_queue.len(), 0);
        });
    });
}

/// Some user interactions like focusing/activating the app may trigger an update check
/// if the daily check hasn't been performed today. The daily check runs regardless of
/// login state so the server can track retention for anonymous users.
#[test]
fn test_user_usage_triggered_daily_check() {
    App::test((), |mut app| async move {
        app.add_singleton_model(|ctx| AppExecutionMode::new(ExecutionMode::App, false, ctx));
        let autoupdate_state = initialize_app(&mut app);
        let some_date = NaiveDate::from_ymd_opt(1991, 8, 22).unwrap();

        app.update_model(&autoupdate_state, |autoupdate, _| {
            assert!(
                autoupdate.should_make_daily_request(RequestType::DailyCheck, &some_date, true),
                "do daily check regardless of login state"
            );

            // same date with arbitrary time
            set_last_successful_daily_update_check(autoupdate, 1991, 8, 22, 4, 24, 19);
            assert!(
                !autoupdate.should_make_daily_request(RequestType::DailyCheck, &some_date, true),
                "don't do daily check again on same day"
            );
            assert!(
                autoupdate.should_make_daily_request(
                    RequestType::DailyCheck,
                    &NaiveDate::from_ymd_opt(1991, 8, 23).unwrap(),
                    false
                ),
                "do daily check on next day"
            );
        });
    });
}

#[test]
fn test_polling_triggered_daily_check() {
    App::test((), |mut app| async move {
        app.add_singleton_model(|ctx| AppExecutionMode::new(ExecutionMode::App, false, ctx));
        let autoupdate_state = initialize_app(&mut app);
        let some_date = NaiveDate::from_ymd_opt(1991, 8, 22).unwrap();

        app.update_model(&autoupdate_state, |autoupdate, _| {
            assert!(
                !autoupdate.should_make_daily_request(RequestType::Poll, &some_date, false),
                "don't do daily check on poll without focus"
            );

            assert!(
                autoupdate.should_make_daily_request(RequestType::Poll, &some_date, true),
                "do daily check on poll with focus"
            );

            // same date with arbitrary time
            set_last_successful_daily_update_check(autoupdate, 1991, 8, 22, 23, 57, 22);
            assert!(
                !autoupdate.should_make_daily_request(RequestType::Poll, &some_date, true),
                "don't do daily check on same day"
            );
            assert!(
                autoupdate.should_make_daily_request(
                    RequestType::Poll,
                    &NaiveDate::from_ymd_opt(1991, 8, 23).unwrap(),
                    true
                ),
                "do daily check on next day"
            );
        });
    });
}

#[test]
fn test_manually_triggered_daily_check() {
    App::test((), |mut app| async move {
        app.add_singleton_model(|ctx| AppExecutionMode::new(ExecutionMode::App, false, ctx));
        let autoupdate_state = initialize_app(&mut app);
        let some_date = NaiveDate::from_ymd_opt(1991, 8, 22).unwrap();

        app.update_model(&autoupdate_state, |autoupdate, _| {
            assert!(
                autoupdate.should_make_daily_request(RequestType::ManualCheck, &some_date, true),
                "do daily check regardless of login state"
            );

            // same date with arbitrary time
            set_last_successful_daily_update_check(autoupdate, 1991, 8, 22, 1, 31, 53);
            assert!(
                !autoupdate.should_make_daily_request(RequestType::ManualCheck, &some_date, true),
                "don't do daily check on same day"
            );
            assert!(
                autoupdate.should_make_daily_request(
                    RequestType::ManualCheck,
                    &NaiveDate::from_ymd_opt(1991, 8, 23).unwrap(),
                    true
                ),
                "do daily check on next day"
            );
        });
    });
}

/// Helper function to assign a DateTime in EST
fn set_last_successful_daily_update_check(
    autoupdate_state: &mut AutoupdateState,
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    min: u32,
    sec: u32,
) {
    let local = Local
        .from_local_datetime(
            &NaiveDate::from_ymd_opt(year, month, day)
                .unwrap()
                .and_hms_opt(hour, min, sec)
                .unwrap(),
        )
        .unwrap();
    autoupdate_state.last_successful_daily_update_check = Some(local.with_timezone(local.offset()));
}

fn make_version_info(version_string: impl Into<String>, is_rollback: bool) -> VersionInfo {
    VersionInfo {
        version: version_string.into(),
        update_by: None,
        soft_cutoff: None,
        last_prominent_update: None,
        is_rollback: Some(is_rollback),
        version_for_new_users: None,
        cli_version: None,
        verified_assets: None,
    }
}

/// When a download fails, `downloaded_update` must stay None so the next poll retries.
/// This is the state-machine behavior underlying a disk-space issue where,
/// without cleanup, every failed download retry would leave lots of failed artifacts behind,
/// eventually filling the user's cache directory.
#[test]
fn test_download_failure_allows_retry() {
    let _app_version = lock_app_version();
    App::test((), |mut app| async move {
        app.add_singleton_model(|ctx| AppExecutionMode::new(ExecutionMode::App, false, ctx));
        let autoupdate_state = initialize_app(&mut app);

        app.update_model(&autoupdate_state, |autoupdate, ctx| {
            ChannelState::set_app_version(Some("v0.2023.05.15.08.04.stable_01"));
            let target_version = "v0.2023.05.15.08.04.stable_02";

            autoupdate.on_download_update_complete(
                RequestType::Poll,
                make_version_info(target_version, false),
                "failed_id".to_string(),
                Err(anyhow!("simulated download failure")),
                ctx,
            );

            // After failure: downloaded_update must remain None.
            assert!(
                autoupdate.downloaded_update.is_none(),
                "downloaded_update should not be set after a failed download"
            );
            assert_eq!(
                autoupdate.stage,
                AutoupdateStage::NoUpdateAvailable,
                "Stage should reset to NoUpdateAvailable after download failure"
            );

            // The next should_update call must return CanDownload (allowing retry).
            let version = make_version_info(target_version, false);
            let result = autoupdate.should_update(version, "retry_id".to_string());
            assert!(
                matches!(result, UpdateReady::CanDownload { .. }),
                "Should allow retry download after failure"
            );
        });
    });
}

/// After a successful download, `downloaded_update` is set and subsequent `should_update`
/// calls return `UpdateReady::Yes` — preventing re-downloads on the next poll.
#[test]
fn test_successful_download_prevents_redownload() {
    let _app_version = lock_app_version();
    App::test((), |mut app| async move {
        app.add_singleton_model(|ctx| AppExecutionMode::new(ExecutionMode::App, false, ctx));
        let autoupdate_state = initialize_app(&mut app);

        app.update_model(&autoupdate_state, |autoupdate, ctx| {
            ChannelState::set_app_version(Some("v0.2023.05.15.08.04.stable_01"));
            let target_version = "v0.2023.05.15.08.04.stable_02";

            autoupdate.on_download_update_complete(
                RequestType::Poll,
                make_version_info(target_version, false),
                "success_id".to_string(),
                Ok(DownloadReady::Yes),
                ctx,
            );

            // After success: downloaded_update must be set.
            let download = autoupdate
                .downloaded_update
                .as_ref()
                .expect("downloaded_update should be set after successful download");
            assert_eq!(download.version.version, target_version);
            assert_eq!(download.update_id, "success_id");
            assert!(
                matches!(autoupdate.stage, AutoupdateStage::UpdateReady { .. }),
                "Stage should be UpdateReady after successful download"
            );

            // The next should_update call must return Yes, NOT CanDownload.
            let version = make_version_info(target_version, false);
            let result = autoupdate.should_update(version, "another_id".to_string());
            match result {
                UpdateReady::Yes { update_id, .. } => {
                    assert_eq!(
                        update_id, "success_id",
                        "Should reuse the existing update_id, not trigger a new download"
                    );
                }
                other => panic!(
                    "Expected UpdateReady::Yes but got {other:?} — this would cause re-downloading!"
                ),
            }
        });
    });
}

/// After a successful download of v2, if a download of v3 fails, `downloaded_update`
/// must still point to v2. This is the state-machine invariant that enables the filesystem
/// fix: `download_new_update` captures `last_successful_update_id` from `downloaded_update`,
/// so failure cleanup preserves the old download's directory on disk.
#[test]
fn test_failed_download_preserves_previous_successful_download() {
    let _app_version = lock_app_version();
    App::test((), |mut app| async move {
        app.add_singleton_model(|ctx| AppExecutionMode::new(ExecutionMode::App, false, ctx));
        let autoupdate_state = initialize_app(&mut app);

        app.update_model(&autoupdate_state, |autoupdate, ctx| {
            ChannelState::set_app_version(Some("v0.2023.05.15.08.04.stable_01"));
            let v2 = "v0.2023.05.15.08.04.stable_02";
            let v3 = "v0.2023.05.15.08.04.stable_03";

            // Successful download of v2.
            autoupdate.on_download_update_complete(
                RequestType::Poll,
                make_version_info(v2, false),
                "id_v2".to_string(),
                Ok(DownloadReady::Yes),
                ctx,
            );
            assert_eq!(
                autoupdate.downloaded_update.as_ref().unwrap().update_id,
                "id_v2"
            );
            assert!(matches!(
                autoupdate.stage,
                AutoupdateStage::UpdateReady { .. }
            ));

            // Failed download of v3.
            autoupdate.on_download_update_complete(
                RequestType::Poll,
                make_version_info(v3, false),
                "id_v3_fail".to_string(),
                Err(anyhow!("simulated network failure")),
                ctx,
            );

            // v2 download must be preserved.
            let download = autoupdate
                .downloaded_update
                .as_ref()
                .expect("downloaded_update should still reference the v2 download");
            assert_eq!(download.version.version, v2);
            assert_eq!(download.update_id, "id_v2");
            assert_eq!(autoupdate.stage, AutoupdateStage::NoUpdateAvailable);

            // Re-checking for v2 should return Yes (already downloaded).
            let result =
                autoupdate.should_update(make_version_info(v2, false), "check_v2".to_string());
            assert!(
                matches!(result, UpdateReady::Yes { ref update_id, .. } if update_id == "id_v2"),
                "should_update for the preserved v2 should return Yes with the original update_id"
            );

            // Checking for v3 should return CanDownload (retry).
            let result =
                autoupdate.should_update(make_version_info(v3, false), "check_v3".to_string());
            assert!(
                matches!(result, UpdateReady::CanDownload { .. }),
                "should_update for v3 should allow a retry download"
            );
        });
    });
}

/// Full cycle: success → failure (preserves) → retry success (replaces).
/// Verifies that after a failed download preserves an earlier success, a subsequent
/// successful download correctly replaces it.
#[test]
fn test_successful_download_after_failure_replaces_preserved_download() {
    let _app_version = lock_app_version();
    App::test((), |mut app| async move {
        app.add_singleton_model(|ctx| AppExecutionMode::new(ExecutionMode::App, false, ctx));
        let autoupdate_state = initialize_app(&mut app);

        app.update_model(&autoupdate_state, |autoupdate, ctx| {
            ChannelState::set_app_version(Some("v0.2023.05.15.08.04.stable_01"));
            let v2 = "v0.2023.05.15.08.04.stable_02";
            let v3 = "v0.2023.05.15.08.04.stable_03";

            // Step 1: Successful download of v2.
            autoupdate.on_download_update_complete(
                RequestType::Poll,
                make_version_info(v2, false),
                "id_v2".to_string(),
                Ok(DownloadReady::Yes),
                ctx,
            );
            assert_eq!(autoupdate.downloaded_update.as_ref().unwrap().update_id, "id_v2");

            // Step 2: Failed download of v3 — v2 preserved.
            autoupdate.on_download_update_complete(
                RequestType::Poll,
                make_version_info(v3, false),
                "id_v3_fail".to_string(),
                Err(anyhow!("network error")),
                ctx,
            );
            assert_eq!(autoupdate.downloaded_update.as_ref().unwrap().update_id, "id_v2");

            // Step 3: Retry v3 succeeds — replaces v2.
            autoupdate.on_download_update_complete(
                RequestType::Poll,
                make_version_info(v3, false),
                "id_v3_success".to_string(),
                Ok(DownloadReady::Yes),
                ctx,
            );
            let download = autoupdate.downloaded_update.as_ref().unwrap();
            assert_eq!(download.version.version, v3);
            assert_eq!(download.update_id, "id_v3_success");
            assert!(matches!(autoupdate.stage, AutoupdateStage::UpdateReady { .. }));

            // v3 is now the downloaded version.
            let result = autoupdate.should_update(make_version_info(v3, false), "check".to_string());
            assert!(
                matches!(result, UpdateReady::Yes { ref update_id, .. } if update_id == "id_v3_success"),
                "should_update for v3 should return Yes with the new update_id"
            );
        });
    });
}

#[test]
fn test_should_update() {
    let _app_version = lock_app_version();
    App::test((), |mut app| async move {
        app.add_singleton_model(|ctx| AppExecutionMode::new(ExecutionMode::App, false, ctx));
        let autoupdate_state = initialize_app(&mut app);

        app.update_model(&autoupdate_state, |autoupdate, _| {
            // Test 1: No version tag set
            ChannelState::set_app_version(None);
            let version = make_version_info(
                "v0.2023.05.15.08.04.stable_01",
                false, /* is_rollback */
            );

            let result = autoupdate.should_update(version, "update1".to_string());
            assert!(
                matches!(result, UpdateReady::No),
                "Should not update when no version tag is set"
            );

            // Test 2: Already up to date
            ChannelState::set_app_version(Some("v0.2023.05.15.08.04.stable_01"));
            let version = make_version_info(
                "v0.2023.05.15.08.04.stable_01",
                false, /* is_rollback */
            );
            let result = autoupdate.should_update(version, "update2".to_string());
            assert!(
                matches!(result, UpdateReady::No),
                "Should not update when already on the latest version"
            );

            // Test 3: Current version ahead of server version (no rollback)
            ChannelState::set_app_version(Some("v0.2023.05.15.08.04.stable_02"));
            let version = make_version_info(
                "v0.2023.05.15.08.04.stable_01",
                false, /* is_rollback */
            );
            let result = autoupdate.should_update(version, "update3".to_string());
            assert!(
                matches!(result, UpdateReady::No),
                "Should not update when current version is ahead and no rollback"
            );

            // Test 4: Current version ahead of server version (with rollback)
            ChannelState::set_app_version(Some("v0.2023.05.15.08.04.stable_02"));
            let version =
                make_version_info("v0.2023.05.15.08.04.stable_01", true /* is_rollback */);
            let result = autoupdate.should_update(version, "update4".to_string());
            assert!(
                matches!(result, UpdateReady::CanDownload { .. }),
                "Should update when current version is ahead but rollback is true"
            );

            // Test 5: New update available for download
            ChannelState::set_app_version(Some("v0.2023.05.15.08.04.stable_01"));
            let version = make_version_info(
                "v0.2023.05.15.08.04.stable_02",
                false, /* is_rollback */
            );
            let result = autoupdate.should_update(version.clone(), "updateid".to_string());
            match result {
                UpdateReady::CanDownload {
                    new_version,
                    update_id,
                } => {
                    assert_eq!(
                        new_version.version, "v0.2023.05.15.08.04.stable_02",
                        "New version should match server version"
                    );
                    assert_eq!(
                        update_id, "updateid",
                        "Update ID should match provided update ID"
                    );
                }
                _ => panic!("Expected UpdateReady::CanDownload for new update"),
            }
        });
    });
}

/// Once a bundle is swapped in without a relaunch, queued checks must still run. Before, the
/// UpdatedPendingRestart stage blocked every check until the app restarted.
#[test]
fn test_pending_restart_does_not_block_polling() {
    App::test((), |mut app| async move {
        app.add_singleton_model(|ctx| AppExecutionMode::new(ExecutionMode::App, false, ctx));
        let autoupdate_state = initialize_app(&mut app);

        app.update_model(&autoupdate_state, |autoupdate, ctx| {
            let installed = make_version_info("v0.2023.05.15.08.04.stable_03", false);
            autoupdate.installed_pending_restart = Some(installed.clone());
            autoupdate.stage = AutoupdateStage::UpdatedPendingRestart {
                new_version: installed,
            };

            // get_next_request only pops the queue; it never starts a check.
            autoupdate.request_queue.push_back(RequestType::Poll);
            assert_eq!(autoupdate.get_next_request(ctx), Some(RequestType::Poll));
            assert!(autoupdate.request_queue.is_empty());
        });
    });
}

/// A check that finds the version already swapped in (or an older one) keeps the pending
/// restart and does not download it again. A failed check settles back on it too.
#[test]
fn test_check_for_pending_version_keeps_pending_restart() {
    let _app_version = lock_app_version();
    App::test((), |mut app| async move {
        app.add_singleton_model(|ctx| AppExecutionMode::new(ExecutionMode::App, false, ctx));
        let autoupdate_state = initialize_app(&mut app);

        app.update_model(&autoupdate_state, |autoupdate, ctx| {
            let installed = make_version_info("v0.2023.05.15.08.04.stable_03", false);
            let pending_stage = AutoupdateStage::UpdatedPendingRestart {
                new_version: installed.clone(),
            };
            autoupdate.installed_pending_restart = Some(installed.clone());
            // A real check leaves the stage at CheckingForUpdate until it completes.
            autoupdate.stage = AutoupdateStage::CheckingForUpdate;
            // Tripwire: if the pending-restart gate did not fire, should_update would find this
            // entry and move the stage to UpdateReady, instead of spawning a real download.
            autoupdate.downloaded_update = Some(DownloadedUpdate {
                version: installed.clone(),
                update_id: "tripwire".to_string(),
            });
            assert!(autoupdate.request_queue.is_empty());

            ChannelState::set_app_version(Some("v0.2023.05.15.08.04.stable_01"));
            autoupdate.on_update_check_complete(
                RequestType::Poll,
                "check_id".to_string(),
                Ok(installed.clone()),
                false,
                ctx,
            );
            assert_eq!(autoupdate.stage, pending_stage);

            // An older manifest entry also counts as installed; a newer one does not.
            let older = make_version_info("v0.2023.05.15.08.04.stable_02", false);
            let newer = make_version_info("v0.2023.05.15.08.04.stable_04", false);
            assert!(autoupdate.is_installed_pending_restart(&older));
            assert!(!autoupdate.is_installed_pending_restart(&newer));

            // An unparsable version only matches exactly.
            let unparsable = make_version_info("not-a-version", false);
            autoupdate.installed_pending_restart = Some(unparsable.clone());
            assert!(autoupdate.is_installed_pending_restart(&unparsable));
            assert!(!autoupdate.is_installed_pending_restart(&older));
            autoupdate.installed_pending_restart = Some(installed.clone());

            // A failed check does not report "up to date" over the pending restart.
            autoupdate.stage = AutoupdateStage::CheckingForUpdate;
            autoupdate.on_update_check_complete(
                RequestType::Poll,
                "err_id".to_string(),
                Err(anyhow!("simulated network failure")),
                false,
                ctx,
            );
            assert_eq!(autoupdate.stage, pending_stage);
        });
    });
}

/// A release newer than the pending one still downloads. This is checked without calling
/// on_update_check_complete, which would spawn a real download.
#[test]
fn test_newer_release_downloads_over_pending_restart() {
    let _app_version = lock_app_version();
    App::test((), |mut app| async move {
        app.add_singleton_model(|ctx| AppExecutionMode::new(ExecutionMode::App, false, ctx));
        let autoupdate_state = initialize_app(&mut app);

        app.update_model(&autoupdate_state, |autoupdate, _| {
            let installed = make_version_info("v0.2023.05.15.08.04.stable_03", false);
            let newer = "v0.2023.05.15.08.04.stable_04";
            autoupdate.installed_pending_restart = Some(installed.clone());
            autoupdate.stage = AutoupdateStage::UpdatedPendingRestart {
                new_version: installed,
            };

            assert!(!autoupdate.is_installed_pending_restart(&make_version_info(newer, false)));

            ChannelState::set_app_version(Some("v0.2023.05.15.08.04.stable_01"));
            let result = autoupdate.should_update(make_version_info(newer, false), "newer_id".to_string());
            assert!(
                matches!(result, UpdateReady::CanDownload { ref new_version, .. } if new_version.version == newer),
                "A newer release should download over a pending restart, got {result:?}"
            );
        });
    });
}

/// Poll and DailyCheck are not queued twice; manual checks always are.
#[test]
fn test_duplicate_poll_is_not_queued() {
    App::test((), |mut app| async move {
        app.add_singleton_model(|ctx| AppExecutionMode::new(ExecutionMode::App, false, ctx));
        let autoupdate_state = initialize_app(&mut app);

        app.update_model(&autoupdate_state, |autoupdate, ctx| {
            // A check in flight blocks, so enqueue_request cannot start a real check.
            autoupdate.stage = AutoupdateStage::CheckingForUpdate;

            autoupdate.enqueue_request(RequestType::Poll, ctx);
            autoupdate.enqueue_request(RequestType::Poll, ctx);
            assert_eq!(autoupdate.request_queue.len(), 1);

            autoupdate.enqueue_request(RequestType::DailyCheck, ctx);
            autoupdate.enqueue_request(RequestType::DailyCheck, ctx);
            assert_eq!(autoupdate.request_queue.len(), 2);

            autoupdate.enqueue_request(RequestType::ManualCheck, ctx);
            autoupdate.enqueue_request(RequestType::ManualCheck, ctx);
            assert_eq!(autoupdate.request_queue.len(), 4);
        });
    });
}

/// A rejected download (wrong version inside the bundle, or a refused signature) is not fetched
/// again on every poll. A newer version still downloads, and an ordinary failure still retries.
#[test]
fn test_rejected_update_is_not_retried() {
    let _app_version = lock_app_version();
    App::test((), |mut app| async move {
        app.add_singleton_model(|ctx| AppExecutionMode::new(ExecutionMode::App, false, ctx));
        let autoupdate_state = initialize_app(&mut app);

        app.update_model(&autoupdate_state, |autoupdate, ctx| {
            let rejected = "v0.2023.05.15.08.04.stable_02";
            let newer = "v0.2023.05.15.08.04.stable_03";
            assert!(autoupdate.request_queue.is_empty());

            autoupdate.on_download_update_complete(
                RequestType::Poll,
                make_version_info(rejected, false),
                "rejected_id".to_string(),
                Err(anyhow::Error::new(UpdateRejected(
                    "Bundle version v0.2023.05.15.08.04.stable_01 does not match".to_string(),
                ))),
                ctx,
            );
            assert_eq!(autoupdate.rejected_version.as_deref(), Some(rejected));
            assert_eq!(autoupdate.stage, AutoupdateStage::NoUpdateAvailable);

            ChannelState::set_app_version(Some("v0.2023.05.15.08.04.stable_01"));
            let result =
                autoupdate.should_update(make_version_info(rejected, false), "again".to_string());
            assert_eq!(result, UpdateReady::No);

            // An ordinary failure of another version does not mark it rejected.
            autoupdate.on_download_update_complete(
                RequestType::Poll,
                make_version_info(newer, false),
                "network_id".to_string(),
                Err(anyhow!("simulated network failure")),
                ctx,
            );
            assert_eq!(autoupdate.rejected_version.as_deref(), Some(rejected));

            ChannelState::set_app_version(Some("v0.2023.05.15.08.04.stable_01"));
            let result =
                autoupdate.should_update(make_version_info(newer, false), "newer".to_string());
            assert!(
                matches!(result, UpdateReady::CanDownload { ref new_version, .. } if new_version.version == newer),
                "A version other than the rejected one should download, got {result:?}"
            );
        });
    });
}

/// Cancelling a quit that scheduled an install-on-exit puts the update back to UpdateReady, the
/// same as cancelling an explicit relaunch.
#[test]
fn test_cancel_install_on_exit_restores_update_ready() {
    App::test((), |mut app| async move {
        app.add_singleton_model(|ctx| AppExecutionMode::new(ExecutionMode::App, false, ctx));
        initialize_singletons(&mut app);
        let new_version = make_version_info("v0.2023.05.15.08.04.stable_02", false);

        app.update(|ctx| {
            AutoupdateState::handle(ctx).update(ctx, |autoupdate, ctx| {
                autoupdate.set_autoupdate_stage(
                    AutoupdateStage::Updating {
                        new_version: new_version.clone(),
                        update_id: "install_id".to_string(),
                    },
                    ctx,
                );
            });
            RelaunchModel::handle(ctx).update(ctx, |model, ctx| {
                model.relaunch_status = RelaunchStatus::InstallOnExit;
                ctx.notify();
            });

            cancel_relaunch(ctx);

            assert_eq!(
                get_update_state(ctx),
                AutoupdateStage::UpdateReady {
                    new_version,
                    update_id: "install_id".to_string(),
                }
            );
            assert_eq!(
                RelaunchModel::as_ref(ctx).relaunch_status,
                RelaunchStatus::None
            );
        });
    });
}

/// On Windows, quitting with an update ready schedules the installer for this exit; an explicit
/// "Install update" keeps its relaunch.
#[cfg(windows)]
#[test]
fn test_apply_pending_update_schedules_install_on_exit() {
    App::test((), |mut app| async move {
        app.add_singleton_model(|ctx| AppExecutionMode::new(ExecutionMode::App, false, ctx));
        initialize_singletons(&mut app);
        let new_version = make_version_info("v0.2023.05.15.08.04.stable_02", false);
        let ready = AutoupdateStage::UpdateReady {
            new_version: new_version.clone(),
            update_id: "install_id".to_string(),
        };

        app.update(|ctx| {
            let set_ready = |ctx: &mut AppContext| {
                let ready = ready.clone();
                AutoupdateState::handle(ctx).update(ctx, |autoupdate, ctx| {
                    autoupdate.set_autoupdate_stage(ready, ctx);
                });
            };

            set_ready(ctx);
            assert!(apply_pending_update(ctx, |_| {}));
            assert_eq!(
                get_update_state(ctx),
                AutoupdateStage::Updating {
                    new_version: new_version.clone(),
                    update_id: "install_id".to_string(),
                }
            );
            assert_eq!(
                RelaunchModel::as_ref(ctx).relaunch_status,
                RelaunchStatus::InstallOnExit
            );

            cancel_relaunch(ctx);
            assert_eq!(get_update_state(ctx), ready);
            assert_eq!(
                RelaunchModel::as_ref(ctx).relaunch_status,
                RelaunchStatus::None
            );

            // An explicit relaunch request is not downgraded to install-on-exit.
            set_ready(ctx);
            RelaunchModel::handle(ctx).update(ctx, RelaunchModel::request_relaunch);
            assert!(apply_pending_update(ctx, |_| {}));
            assert_eq!(
                RelaunchModel::as_ref(ctx).relaunch_status,
                RelaunchStatus::Requested
            );
        });
    });
}

/// Mnemos IDE: an unsigned manifest (a publish from a release script older than signed
/// manifests) is an expected state, not reported, also behind the check's context; any
/// other failure is still reported.
#[test]
fn unsigned_manifest_is_an_expected_check_failure() {
    use anyhow::Context as _;
    let unsigned: anyhow::Result<()> = Err(UnsignedManifest.into());
    let unsigned = unsigned
        .context("Error checking for new version")
        .unwrap_err();
    assert!(is_expected_check_failure(&unsigned));
    assert!(unsigned.downcast_ref::<UnsignedManifest>().is_some());

    let no_release: anyhow::Error = NoReleaseForPlatform("windows-arm64".to_owned()).into();
    assert!(is_expected_check_failure(&no_release));

    let other = anyhow::anyhow!("The release manifest signature does not verify; not updating");
    assert!(!is_expected_check_failure(&other));
}

/// Mnemos IDE: the update hints come from the signed payload only. An unsigned `soft_cutoff`
/// or `update_by` in the legacy section must not survive verification.
#[test]
fn apply_signed_release_drops_unsigned_hints() {
    let mut version_info = VersionInfo::new("v0.1.0".to_owned());
    version_info.soft_cutoff = Some("v9.9.9".to_owned());
    version_info.last_prominent_update = Some("v9.9.9".to_owned());
    version_info.update_by = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z").ok();
    version_info.cli_version = Some("v9.9.9".to_owned());

    let mut release = signed::PlatformRelease::new("v0.2.0");
    release.soft_cutoff = Some("v0.1.5".to_owned());
    let payload = signed::ReleasePayload {
        format: signed::PAYLOAD_FORMAT,
        channel: "stable".to_owned(),
        issued_at: "2026-10-09T00:00:00Z".to_owned(),
        platforms: [("macos-aarch64".to_owned(), release)].into_iter().collect(),
    };

    apply_signed_release(&mut version_info, &payload, "macos-aarch64").unwrap();
    assert_eq!(version_info.version, "v0.2.0");
    assert_eq!(version_info.soft_cutoff.as_deref(), Some("v0.1.5"));
    assert_eq!(version_info.update_by, None);
    assert_eq!(version_info.last_prominent_update, None);
    assert_eq!(version_info.cli_version, None);
    assert!(version_info.verified_assets.is_some());

    let err = apply_signed_release(&mut version_info, &payload, "windows-x86_64").unwrap_err();
    assert!(is_expected_check_failure(&err));
}

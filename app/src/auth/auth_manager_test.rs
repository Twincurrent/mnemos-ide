use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::{AuthManager, AuthManagerEvent};
use crate::auth::{
    auth_view_modal::AuthRedirectPayload,
    credentials::{Credentials, RefreshToken},
    user::{FirebaseAuthTokens, TEST_USER_UID},
    AuthStateProvider, UserUid,
};
use crate::server::server_api::auth::UserAuthenticationError;
use crate::terminal::general_settings::GeneralSettings;
use crate::ServerApiProvider;
use mnemos_ui::{App, SingletonEntity};
use settings::Setting as _;
use mnemos_ide_sync::SyncAuthState;

/// An admin access token whose claims decode (`sub` is the default test
/// user's uid). The signature is not checked by the app, so any will do.
fn test_admin_jwt() -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let claims = URL_SAFE_NO_PAD.encode(
        serde_json::json!({"sub": TEST_USER_UID, "email": "admin@example.test"}).to_string(),
    );
    format!("{header}.{claims}.signature")
}

fn initialize_app(app: &mut App) {
    // GeneralSettings holds the `admin_login_seen` marker the sign-in and
    // sign-out paths write. This also registers a no-op `SecureStorage`, which
    // the log-out paths need.
    crate::test_util::settings::initialize_settings_for_tests(app);
    app.add_singleton_model(|_ctx| ServerApiProvider::new_for_test());
    app.add_singleton_model(|_| AuthStateProvider::new_for_test());
    app.add_singleton_model(AuthManager::new_for_test);
}

/// Subscribes to `AuthManager` events and returns a flag that becomes `true`
/// if an `AuthFailed(InvalidStateParameter)` event is observed.
fn track_invalid_state_failures(app: &mut App) -> Arc<AtomicBool> {
    let saw_invalid_state = Arc::new(AtomicBool::new(false));
    let saw_invalid_state_for_closure = saw_invalid_state.clone();
    app.update(|ctx| {
        ctx.subscribe_to_model(&AuthManager::handle(ctx), move |_, event, _| {
            if matches!(
                event,
                AuthManagerEvent::AuthFailed(UserAuthenticationError::InvalidStateParameter)
            ) {
                saw_invalid_state_for_closure.store(true, Ordering::Relaxed);
            }
        });
    });
    saw_invalid_state
}

/// After a logged-in user successfully completes auth, pressing the browser's
/// "Take me to Mnemos" button a second time should silently drop the stale
/// redirect rather than surface an `InvalidStateParameter` error.
#[test]
fn test_duplicate_redirect_for_logged_in_user_is_silently_ignored() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        // Fail immediately if we see InvalidStateParameter at any point.
        app.update(|ctx| {
            ctx.subscribe_to_model(&AuthManager::handle(ctx), move |_, event, _| {
                if matches!(
                    event,
                    AuthManagerEvent::AuthFailed(UserAuthenticationError::InvalidStateParameter)
                ) {
                    panic!("Test failed: Received InvalidStateParameter error");
                }
            });
        });

        // Generate a state token and create the auth payload that we'll use for both calls.
        // The incoming user_uid matches the default test user so the stale second redirect
        // qualifies for the silent-ignore branch.
        let auth_payload = AuthManager::handle(&app).update(&mut app, |auth_manager, _ctx| {
            let state = auth_manager.generate_auth_state();

            AuthRedirectPayload {
                refresh_token: RefreshToken::new("test_refresh_token"),
                access_token: None,
                user_uid: Some(UserUid::new(TEST_USER_UID)),
                deleted_anonymous_user: Some(false),
                state: Some(state),
                code: None,
            }
        });

        // First call: state validates and is consumed.
        AuthManager::handle(&app).update(&mut app, |auth_manager, ctx| {
            auth_manager.initialize_user_from_auth_payload(auth_payload.clone(), true, ctx);
        });

        // The CSRF token must be single-use: successful validation clears it.
        AuthManager::handle(&app).update(&mut app, |auth_manager, _ctx| {
            assert!(
                auth_manager.pending_auth_state.is_none(),
                "pending_auth_state should be cleared after successful validation"
            );
        });

        // Second call with the same (now-consumed) state: the user is already
        // logged in as the test user and the incoming user_uid matches, so we
        // must silently drop the redirect without emitting any AuthFailed event.
        AuthManager::handle(&app).update(&mut app, |auth_manager, ctx| {
            auth_manager.initialize_user_from_auth_payload(auth_payload, true, ctx);
        });
    });
}

/// When the user is fully logged out, a redirect carrying a state that does
/// not match the pending token must surface an `InvalidStateParameter` error.
#[test]
fn test_stale_state_when_logged_out_emits_invalid_state_parameter() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        // Clear the default test user so we're fully logged out.
        app.update(|ctx| {
            let auth_state = AuthStateProvider::as_ref(ctx).get();
            auth_state.set_user(None);
            auth_state.set_credentials(None);
        });

        let saw_invalid_state = track_invalid_state_failures(&mut app);

        // Generate a real pending state, then deliver a redirect whose state
        // doesn't match it.
        AuthManager::handle(&app).update(&mut app, |auth_manager, _ctx| {
            let _known_state = auth_manager.generate_auth_state();
        });

        let bogus_payload = AuthRedirectPayload {
            refresh_token: RefreshToken::new("test_refresh_token"),
            access_token: None,
            user_uid: Some(UserUid::new("some_user_uid")),
            deleted_anonymous_user: Some(false),
            state: Some("not_the_real_state".to_owned()),
            code: None,
        };

        AuthManager::handle(&app).update(&mut app, |auth_manager, ctx| {
            auth_manager.initialize_user_from_auth_payload(bogus_payload, true, ctx);
        });

        assert!(
            saw_invalid_state.load(Ordering::Relaxed),
            "expected AuthFailed(InvalidStateParameter) when logged out and state does not match"
        );
    });
}

/// Even when a user is logged in, a redirect with a bad state and a `user_uid`
/// that does NOT match the current user must surface an `InvalidStateParameter`
/// error: the silent-ignore branch is reserved for redirects that target the
/// same user who is already authenticated.
#[test]
fn test_mismatched_state_with_different_user_uid_emits_invalid_state_parameter() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let saw_invalid_state = track_invalid_state_failures(&mut app);

        // Test user is logged in by default; generate a pending state so the
        // validation below fails because the incoming state is different.
        AuthManager::handle(&app).update(&mut app, |auth_manager, _ctx| {
            let _known_state = auth_manager.generate_auth_state();
        });

        // Attacker-style payload: bogus state, plus a user_uid that differs
        // from the currently logged-in user's uid.
        let attacker_payload = AuthRedirectPayload {
            refresh_token: RefreshToken::new("attacker_refresh_token"),
            access_token: None,
            user_uid: Some(UserUid::new("not_the_current_user")),
            deleted_anonymous_user: Some(false),
            state: Some("not_the_real_state".to_owned()),
            code: None,
        };

        AuthManager::handle(&app).update(&mut app, |auth_manager, ctx| {
            auth_manager.initialize_user_from_auth_payload(attacker_payload, true, ctx);
        });

        assert!(
            saw_invalid_state.load(Ordering::Relaxed),
            "expected AuthFailed(InvalidStateParameter) when incoming user_uid differs from current user"
        );
    });
}

/// `log_out` must clear any pending CSRF state from an auth flow that was
/// started but never completed, so the token cannot be replayed against the
/// next session in the same process.
#[test]
fn test_log_out_clears_pending_auth_state() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        // `log_out` clears user+credentials and then calls `persist`, which
        // routes to `PersistedUser::remove_from_secure_storage`. That requires
        // a `SecureStorage` singleton; `initialize_app` registers a no-op one.

        AuthManager::handle(&app).update(&mut app, |auth_manager, ctx| {
            let _pending = auth_manager.generate_auth_state();
            assert!(
                auth_manager.pending_auth_state.is_some(),
                "precondition: generate_auth_state should populate pending_auth_state"
            );

            auth_manager.log_out(ctx);

            assert!(
                auth_manager.pending_auth_state.is_none(),
                "log_out should clear pending_auth_state"
            );
        });
    });
}

// These two tests verify that `persist` skips writing to secure storage under certain conditions.
// They rely on the fact that no secure storage singleton is registered in the test app: if
// `write_to_secure_storage` were ever called, it would panic trying to look up the unregistered
// singleton, causing the test to fail.

#[test]
fn test_persist_skips_when_refresh_token_is_empty() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        // Override default test credentials with Firebase tokens that have an empty refresh token.
        app.update(|ctx| {
            let tokens = FirebaseAuthTokens {
                id_token: String::new(),
                refresh_token: String::new(),
                expiration_time: chrono::Utc::now().fixed_offset() + chrono::Duration::days(365),
            };
            AuthStateProvider::as_ref(ctx)
                .get()
                .set_credentials(Some(Credentials::Firebase(tokens)));
        });

        AuthManager::handle(&app).update(&mut app, |auth_manager, ctx| {
            auth_manager.persist(ctx);
        });
    });
}

#[test]
fn test_persist_skips_when_api_key_authenticated() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        app.update(|ctx| {
            AuthStateProvider::as_ref(ctx)
                .get()
                .set_credentials(Some(Credentials::ApiKey {
                    key: "wk-test-key".to_owned(),
                    owner_type: None,
                }));
        });

        AuthManager::handle(&app).update(&mut app, |auth_manager, ctx| {
            auth_manager.persist(ctx);
        });
    });
}

/// Counts `IdeSyncAuthChanged` events, which is what re-renders the header
/// chip and the Settings > Sync row.
fn track_ide_sync_auth_changes(app: &mut App) -> Arc<std::sync::atomic::AtomicUsize> {
    let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen_for_closure = seen.clone();
    app.update(|ctx| {
        ctx.subscribe_to_model(&AuthManager::handle(ctx), move |_, event, _| {
            if matches!(event, AuthManagerEvent::IdeSyncAuthChanged) {
                seen_for_closure.fetch_add(1, Ordering::Relaxed);
            }
        });
    });
    seen
}

/// The fix for "Sync signed out" is signing in again, without restarting the
/// app: every admin redirect starts a fresh sync run in place of the old one
/// (which may have halted on a refused session and never resumes by itself).
#[test]
fn test_every_admin_redirect_restarts_ide_sync() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let changes = track_ide_sync_auth_changes(&mut app);

        AuthManager::handle(&app).update(&mut app, |auth_manager, _ctx| {
            let unreadable = "Could not read the saved admin login: denied";
            auth_manager.ide_sync_not_started = Some(unreadable.to_owned());
            assert_eq!(
                auth_manager.ide_sync_auth(),
                SyncAuthState::SignedOut {
                    reason: unreadable.to_owned()
                }
            );
        });

        for round in 1..=2 {
            let payload = AuthManager::handle(&app).update(&mut app, |auth_manager, _ctx| {
                AuthRedirectPayload {
                    refresh_token: RefreshToken::new("admin_refresh_token"),
                    access_token: Some(test_admin_jwt()),
                    user_uid: None,
                    deleted_anonymous_user: None,
                    state: Some(auth_manager.generate_auth_state()),
                    code: None,
                }
            });
            AuthManager::handle(&app).update(&mut app, |auth_manager, ctx| {
                auth_manager.initialize_user_from_auth_payload(payload, true, ctx);
                assert_eq!(
                    auth_manager.ide_sync_test_launches.len(),
                    round,
                    "each sign-in starts sync again, even with a run in place"
                );
                assert_eq!(auth_manager.ide_sync_test_launches[round - 1], None);
                assert!(auth_manager.ide_sync_not_started.is_none());
            });
            assert_eq!(changes.load(Ordering::Relaxed), round);
        }
    });
}

/// Startup with nothing in the keyring is a first-time or offline user: sync
/// was never set up, which is not a warning (no chip, no error).
#[test]
fn test_startup_without_an_admin_login_reads_as_never_configured() {
    // Every keyring read in this process goes to an in-memory store, never
    // the OS one. There is no way back, which is the point.
    mnemos_ide_sync::auth::use_in_memory_keyring();
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        AuthManager::handle(&app).update(&mut app, |auth_manager, ctx| {
            auth_manager.start_ide_sync(ctx);
            assert!(auth_manager.ide_sync_test_launches.is_empty());
            assert!(auth_manager.ide_sync_manager.is_none());
            assert!(auth_manager.ide_sync_not_started.is_none());
            assert_eq!(auth_manager.ide_sync_auth(), SyncAuthState::NeverConfigured);
            assert_eq!(
                crate::auth::ide_sync_auth::notice(&auth_manager.ide_sync_auth()),
                None,
                "no chip for a user who never signed in"
            );
        });
    });
}

/// A machine that had an admin login whose keyring entry is gone (the save
/// failed, or the keyring was reset) must say so on the next start with the
/// "Sync signed out" chip, not look like a machine that never signed in.
#[test]
fn test_startup_with_a_lost_admin_login_reads_as_signed_out() {
    mnemos_ide_sync::auth::use_in_memory_keyring();
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        GeneralSettings::handle(&app).update(&mut app, |settings, ctx| {
            settings
                .admin_login_seen
                .set_value(true, ctx)
                .expect("set admin_login_seen");
        });
        AuthManager::handle(&app).update(&mut app, |auth_manager, ctx| {
            auth_manager.start_ide_sync(ctx);
            assert!(auth_manager.ide_sync_manager.is_none());
            assert!(matches!(
                auth_manager.ide_sync_auth(),
                SyncAuthState::SignedOut { .. }
            ));
            assert!(
                crate::auth::ide_sync_auth::notice(&auth_manager.ide_sync_auth()).is_some(),
                "the chip shows for a lost login"
            );
        });
    });
}

/// Clicking "Sign in" again before the browser comes back (the chip, then
/// Settings > Sync) must not void the first tab: both open the same state,
/// so finishing the sign-in in either one works.
#[test]
fn test_repeated_admin_sign_in_reuses_the_pending_state() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let saw_invalid_state = track_invalid_state_failures(&mut app);

        let (first, second) = AuthManager::handle(&app).update(&mut app, |auth_manager, _ctx| {
            (
                auth_manager.admin_sign_in_url(),
                auth_manager.admin_sign_in_url(),
            )
        });
        assert_eq!(first, second, "the second click reuses the first state");
        assert!(first.contains("/signup/remote?"), "{first}");
        let state = first
            .split("state=")
            .nth(1)
            .and_then(|rest| rest.split('&').next())
            .expect("state param")
            .to_owned();

        // The user finishes in the first tab.
        AuthManager::handle(&app).update(&mut app, |auth_manager, ctx| {
            let payload = AuthRedirectPayload {
                refresh_token: RefreshToken::new("admin_refresh_token"),
                access_token: Some(test_admin_jwt()),
                user_uid: None,
                deleted_anonymous_user: None,
                state: Some(state),
                code: None,
            };
            auth_manager.initialize_user_from_auth_payload(payload, true, ctx);
            assert_eq!(auth_manager.ide_sync_test_launches.len(), 1, "signed in");
            assert!(
                auth_manager.pending_auth_state.is_none(),
                "still single-use"
            );
        });
        assert!(!saw_invalid_state.load(Ordering::Relaxed));

        // With nothing in flight, the next click starts a new sign-in.
        AuthManager::handle(&app).update(&mut app, |auth_manager, _ctx| {
            assert_ne!(auth_manager.admin_sign_in_url(), first);
        });
    });
}

/// The OS keyring holds the developer's real admin login. Every test in this
/// process must get the in-memory store without asking for it, which is what
/// the `ctor` in `lib.rs` is for; this fails if it stops running.
#[test]
fn test_unit_tests_never_reach_the_os_keyring() {
    assert!(mnemos_ide_sync::auth::keyring_is_in_memory());
}

/// Signs in through the admin redirect (the /signup/remote handoff), which
/// sets `Credentials::SessionCookie`. Unit tests never write the keyring on
/// this path (see `persist_admin_tokens`) and never build a real manager
/// (see `run_ide_sync`).
fn sign_in_as_admin(app: &mut App) {
    let payload = AuthManager::handle(app).update(app, |auth_manager, _ctx| AuthRedirectPayload {
        refresh_token: RefreshToken::new("admin_refresh_token"),
        access_token: Some(test_admin_jwt()),
        user_uid: None,
        deleted_anonymous_user: None,
        state: Some(auth_manager.generate_auth_state()),
        code: None,
    });
    AuthManager::handle(app).update(app, |auth_manager, ctx| {
        auth_manager.initialize_user_from_auth_payload(payload, true, ctx);
    });
    app.update(|ctx| {
        assert!(matches!(
            AuthStateProvider::as_ref(ctx).get().credentials(),
            Some(Credentials::SessionCookie)
        ));
        assert!(crate::auth::is_admin_sync_login(ctx));
        assert!(*GeneralSettings::as_ref(ctx).admin_login_seen);
    });
}

/// What every way out of the admin login must leave behind: no login, no
/// sync, and the same state as a machine that never signed in (no chip;
/// the "Sync is off" row on Settings > Sync offers the sign-in).
fn assert_signed_out_of_admin(app: &mut App) {
    app.update(|ctx| {
        let auth_state = AuthStateProvider::as_ref(ctx).get();
        assert!(auth_state.credentials().is_none());
        assert!(auth_state.user_id().is_none());
        assert!(!auth_state.is_logged_in());
        assert!(auth_state.is_anonymous_or_logged_out(), "menu entry hidden");
        assert!(
            !*GeneralSettings::as_ref(ctx).admin_login_seen,
            "a deliberate sign-out is not a lost login"
        );
    });
    AuthManager::handle(app).update(app, |auth_manager, _ctx| {
        assert!(auth_manager.pending_auth_state.is_none());
        assert!(auth_manager.ide_sync_manager.is_none());
        assert_eq!(auth_manager.ide_sync_auth(), SyncAuthState::NeverConfigured);
    });
}

/// "Sign out of sync" stops mnemos-ide-sync and forgets the admin login, and says
/// so even when no manager was running (which is always the case in unit
/// tests, so this also covers the unconditional event).
#[test]
fn test_sign_out_of_admin_stops_sync_and_clears_the_login() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        sign_in_as_admin(&mut app);
        let changes = track_ide_sync_auth_changes(&mut app);

        let generation = AuthManager::handle(&app).update(&mut app, |auth_manager, ctx| {
            // A sign-in left open in the browser, and a stale reason from
            // startup: neither may outlive the sign-out.
            let _pending = auth_manager.generate_auth_state();
            auth_manager.ide_sync_not_started =
                Some("Could not read the saved admin login: test".to_owned());
            let generation = auth_manager.ide_sync_generation;
            auth_manager.sign_out_of_admin(ctx);
            generation
        });

        assert_signed_out_of_admin(&mut app);
        AuthManager::handle(&app).update(&mut app, |auth_manager, _ctx| {
            assert_eq!(
                auth_manager.ide_sync_generation,
                generation + 1,
                "a status watch armed for the old run must not fire"
            );
            assert_eq!(auth_manager.ide_sync_test_launches.len(), 1, "no restart");
        });
        assert_eq!(changes.load(Ordering::Relaxed), 1);
    });
}

/// `auth::log_out` under the admin login only signs out of sync. The full
/// log out deletes mnemos.sqlite through `persistence::remove` and then
/// dispatches `root_view:log_out` to every window; on the way it looks up
/// singletons this test app never registers (`CodebaseIndexManager`,
/// `GlobalResourceHandlesProvider` for the `persistence::remove` sender,
/// `CloudModel`, ...), and an unregistered singleton panics. Getting through
/// proves the admin branch returns before `persistence::remove` and the
/// dispatch.
#[test]
fn test_log_out_under_the_admin_login_keeps_the_workspace() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        app.update(|ctx| {
            // Other credentials keep the full log out.
            assert!(!crate::auth::is_admin_sync_login(ctx));
        });
        sign_in_as_admin(&mut app);
        let changes = track_ide_sync_auth_changes(&mut app);

        app.update(crate::auth::log_out);

        assert_signed_out_of_admin(&mut app);
        assert_eq!(changes.load(Ordering::Relaxed), 1);
    });
}

/// The menu and Settings dispatch `maybe_log_out`, whose "you will lose ..."
/// warning does not apply to signing out of sync. Deciding on that warning
/// reads `CloudModel` and `GeneralSettings`, which this test app never
/// registers, so getting through proves it went straight to the sign-out.
#[test]
fn test_maybe_log_out_under_the_admin_login_skips_the_warning() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        sign_in_as_admin(&mut app);

        app.update(crate::auth::maybe_log_out);

        assert_signed_out_of_admin(&mut app);
    });
}

/// "Log in" and "Copy URL" on the auth view go to the admin platform's
/// desktop sign-in, not /login/remote (which 404s), and share one state so
/// finishing in either tab works.
#[test]
fn test_sign_in_url_is_the_admin_sign_up_handoff() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        AuthManager::handle(&app).update(&mut app, |auth_manager, _ctx| {
            let url = auth_manager.sign_in_url();
            assert!(url.contains("/signup/remote?"), "{url}");
            assert!(!url.contains("/login/remote"), "{url}");
            let state = auth_manager
                .pending_auth_state
                .clone()
                .expect("pending state");
            assert!(url.contains(&format!("state={state}")), "{url}");
            assert_eq!(auth_manager.sign_in_url(), url, "a second click reuses it");
            assert_eq!(auth_manager.admin_sign_in_url(), url);
        });
    });
}

/// Regression: the login from the sign-in redirect used to be stamped with a
/// hard-coded TwinCurrent URL whatever server issued it. It must be bound to
/// the server the app sent the user to sign in at.
#[test]
fn admin_login_is_bound_to_the_issuing_server() {
    let (auth, not_saved) = super::persist_admin_tokens(mnemos_ide_sync::auth::AdminTokens {
        access_token: "AT".to_owned(),
        refresh_token: "RT".to_owned(),
        expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
    });
    assert_eq!(not_saved, None);
    let issuer = mnemos_core::channel::ChannelState::server_root_url();
    assert_eq!(auth.base_url.as_str(), &*issuer);
    assert!(auth.is_for_server(&issuer));
    assert!(!auth.is_for_server("https://other-admin.example.test"));
}

/// Counts `AuthFailed` events of any kind.
fn track_auth_failures(app: &mut App) -> Arc<std::sync::atomic::AtomicUsize> {
    let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen_for_closure = seen.clone();
    app.update(|ctx| {
        ctx.subscribe_to_model(&AuthManager::handle(ctx), move |_, event, _| {
            if matches!(event, AuthManagerEvent::AuthFailed(_)) {
                seen_for_closure.fetch_add(1, Ordering::Relaxed);
            }
        });
    });
    seen
}

/// A redirect whose access token is not a readable JWT used to sign the app
/// in as `User::test()`. It must fail the sign-in and leave nothing behind.
#[test]
fn test_unreadable_admin_jwt_fails_the_sign_in() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let failures = track_auth_failures(&mut app);
        let changes = track_ide_sync_auth_changes(&mut app);

        let payload =
            AuthManager::handle(&app).update(&mut app, |auth_manager, _ctx| AuthRedirectPayload {
                refresh_token: RefreshToken::new("admin_refresh_token"),
                access_token: Some("admin.access.token".to_owned()),
                user_uid: None,
                deleted_anonymous_user: None,
                state: Some(auth_manager.generate_auth_state()),
                code: None,
            });
        AuthManager::handle(&app).update(&mut app, |auth_manager, ctx| {
            auth_manager.initialize_user_from_auth_payload(payload, true, ctx);
            assert!(
                auth_manager.ide_sync_test_launches.is_empty(),
                "no sync run for a refused sign-in"
            );
        });
        assert_eq!(failures.load(Ordering::Relaxed), 1);
        assert_eq!(changes.load(Ordering::Relaxed), 0);
        app.update(|ctx| {
            assert!(!matches!(
                AuthStateProvider::as_ref(ctx).get().credentials(),
                Some(Credentials::SessionCookie)
            ));
            assert!(!*GeneralSettings::as_ref(ctx).admin_login_seen);
        });
    });
}

/// The sign-in URL carries the S256 challenge of the verifier kept for the
/// pending state, and a second click sends the same challenge.
#[test]
fn test_admin_sign_in_url_carries_the_pkce_challenge() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        AuthManager::handle(&app).update(&mut app, |auth_manager, _ctx| {
            let url = auth_manager.admin_sign_in_url();
            let verifier = auth_manager
                .pending_code_verifier
                .clone()
                .expect("a verifier is kept with the pending state");
            assert!(
                url.contains(&format!("code_challenge={}", verifier.challenge())),
                "{url}"
            );
            assert!(url.contains("code_challenge_method=S256"), "{url}");
            assert!(
                !url.contains(verifier.as_str()),
                "the verifier never leaves the app"
            );
            assert_eq!(auth_manager.admin_sign_in_url(), url);
        });
    });
}

/// A PKCE redirect is accepted only for the sign-in in progress: a wrong
/// state is refused before any exchange, and the verifier survives for the
/// real redirect.
#[test]
fn test_code_redirect_with_a_wrong_state_is_refused() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let saw_invalid_state = track_invalid_state_failures(&mut app);
        AuthManager::handle(&app).update(&mut app, |auth_manager, ctx| {
            let _ = auth_manager.admin_sign_in_url();
            let payload = AuthRedirectPayload {
                refresh_token: RefreshToken::new(""),
                access_token: None,
                user_uid: None,
                deleted_anonymous_user: None,
                state: Some("not_the_real_state".to_owned()),
                code: Some("c0deC0deC0deC0deC0de".to_owned()),
            };
            auth_manager.initialize_user_from_auth_payload(payload, true, ctx);
            assert!(auth_manager.pending_auth_state.is_some());
            assert!(auth_manager.pending_code_verifier.is_some());
            assert!(auth_manager.ide_sync_test_launches.is_empty());
        });
        assert!(saw_invalid_state.load(Ordering::Relaxed));
    });
}

/// A PKCE redirect for the pending sign-in consumes its state and verifier
/// at once, so a replayed redirect cannot start a second exchange. The code
/// here is malformed, so the exchange fails before any request is made.
#[test]
fn test_code_redirect_consumes_the_state_and_verifier() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let saw_invalid_state = track_invalid_state_failures(&mut app);
        AuthManager::handle(&app).update(&mut app, |auth_manager, ctx| {
            let _ = auth_manager.admin_sign_in_url();
            let state = auth_manager.pending_auth_state.clone().unwrap();
            let payload = AuthRedirectPayload {
                refresh_token: RefreshToken::new(""),
                access_token: None,
                user_uid: None,
                deleted_anonymous_user: None,
                state: Some(state),
                code: Some("not a code".to_owned()),
            };
            auth_manager.initialize_user_from_auth_payload(payload, true, ctx);
            assert!(auth_manager.pending_auth_state.is_none());
            assert!(auth_manager.pending_code_verifier.is_none());
            assert!(
                auth_manager.ide_sync_test_launches.is_empty(),
                "nothing signs in before the exchange succeeds"
            );
        });
        assert!(!saw_invalid_state.load(Ordering::Relaxed));
    });
}

/// Signing out drops an unfinished sign-in's verifier with its state.
#[test]
fn test_sign_out_clears_the_pending_verifier() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        AuthManager::handle(&app).update(&mut app, |auth_manager, ctx| {
            let _ = auth_manager.admin_sign_in_url();
            assert!(auth_manager.pending_code_verifier.is_some());
            auth_manager.sign_out_of_admin(ctx);
            assert!(auth_manager.pending_code_verifier.is_none());
        });
    });
}

/// The PKCE redirect parses with its code and no token; the legacy token
/// redirect still parses; a redirect with neither is refused, and the error
/// never repeats the URL's query (which holds tokens on the legacy shape).
#[test]
fn test_auth_redirect_parsing() {
    let pkce = AuthRedirectPayload::from_raw_url(
        "mnemos://auth/desktop_redirect?code=abc123abc123abc123&state=s1".to_owned(),
    )
    .unwrap();
    assert_eq!(pkce.code.as_deref(), Some("abc123abc123abc123"));
    assert_eq!(pkce.state.as_deref(), Some("s1"));
    assert!(pkce.access_token.is_none());
    assert_eq!(pkce.refresh_token.get(), "");

    let legacy = AuthRedirectPayload::from_raw_url(
        "mnemos://auth/desktop_redirect?access_token=AT&refresh_token=RT&state=s2".to_owned(),
    )
    .unwrap();
    assert!(legacy.code.is_none());
    assert_eq!(legacy.access_token.as_deref(), Some("AT"));
    assert_eq!(legacy.refresh_token.get(), "RT");

    let empty_code = AuthRedirectPayload::from_raw_url(
        "mnemos://auth/desktop_redirect?code=&state=s3".to_owned(),
    );
    assert!(empty_code.is_err());

    let err = AuthRedirectPayload::from_raw_url(
        "mnemos://elsewhere/x?access_token=SECRET_AT&refresh_token=SECRET_RT".to_owned(),
    )
    .unwrap_err()
    .to_string();
    assert!(!err.contains("SECRET"), "{err}");
}

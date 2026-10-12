pub(super) mod user_persistence;

use std::result::Result as StdResult;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use settings::Setting as _;
use uuid::Uuid;
use mnemos_core::channel::ChannelState;
use mnemos_core::features::FeatureFlag;
use mnemos_ui::{clipboard::ClipboardContent, Entity, ModelContext, SingletonEntity, UpdateModel};

use super::auth_state::{AuthState, PersistAction};
use super::auth_view_modal::{AuthRedirectPayload, AuthViewVariant};
use super::credentials::Credentials;
use super::user::User;
use super::AuthStateProvider;
use super::UserUid;
use crate::ai::llms::LLMPreferences;
use crate::ai::persisted_workspace::PersistedWorkspace;
use crate::ai::AIRequestUsageModel;
use crate::autoupdate::AutoupdateState;
use crate::persistence::ModelEvent;
use crate::server::cloud_objects::update_manager::UpdateManager;
use crate::server::server_api::auth::FetchUserResult;
use crate::server::server_api::ServerApiProvider;
use crate::server::{
    server_api::{
        auth::{AuthClient, MintCustomTokenError, UserAuthenticationError},
        ServerApi,
    },
    telemetry::AnonymousUserSignupEntrypoint,
};
use crate::settings::cloud_preferences_syncer::CloudPreferencesSyncer;
use crate::settings::initializer::SettingsInitializer;
use crate::settings::PrivacySettings;
use crate::terminal::general_settings::GeneralSettings;
use crate::terminal::shared_session::manager::Manager as SharedSessionManager;
#[cfg(target_family = "wasm")]
use crate::uri::browser_url_handler::{parse_current_url, update_browser_url};
use crate::workspaces::team_tester::TeamTesterStatus;
use crate::{
    persistence, report_if_error, send_telemetry_from_ctx,
    send_telemetry_sync_from_ctx, GlobalResourceHandlesProvider, TelemetryEvent,
};
#[cfg(target_family = "wasm")]
use url::Url;
use user_persistence::PersistedUser;

#[derive(Debug)]
pub enum AuthManagerEvent {
    /// Successfully authenticated a user with no errors.
    AuthComplete,
    /// Failed to authenticate a user, due to a particular `UserAuthenticationError`.
    AuthFailed(UserAuthenticationError),
    /// The user chose to skip login entirely (no Firebase user created).
    SkippedLogin,
    /// The user now needs to reauthenticate. If the user needs to reauth, an `AuthFailed`
    /// event might be triggered instead, but there are some code paths where we don't
    /// refresh the entire user, only their token, which is when this event might be emitted.
    NeedsReauth,
    /// The user is anonymous and has attempted to access a login-gated feature or link.
    AttemptedLoginGatedFeature {
        auth_view_variant: AuthViewVariant,
    },
    // The current user is anonymous and the client has received a browser intent to sign in with a different Mnemos account.
    // Holds an auth payload from the received browser intent.
    LoginOverrideDetected(AuthRedirectPayload),
    /// Failed to mint a new custom token for an anonymous user.
    MintCustomTokenFailed(MintCustomTokenError),
    /// Mnemos IDE: mnemos-ide-sync's admin login changed state (see
    /// [`AuthManager::ide_sync_auth`]): signed out, token not saved, or back
    /// to signed in after a new sign-in.
    IdeSyncAuthChanged,
}

pub type LoginGatedFeature = &'static str;

/// A queued start or stop of a mnemos-ide-sync run (see
/// `AuthManager::ide_sync_lifecycle`).
type IdeSyncLifecycle = futures::future::Shared<futures::future::BoxFuture<'static, ()>>;

/// AuthManager is a singleton model which manages the currently logged-in user's state.
/// If you need to access the state, use `AuthStateProvider`.
pub struct AuthManager {
    auth_state: Arc<AuthState>,
    server_api: Arc<ServerApi>,
    auth_client: Arc<dyn AuthClient>,
    /// A generated state token that the web app must provide back to the client.
    pending_auth_state: Option<String>,
    /// Mnemos IDE: the PKCE verifier of the sign-in `pending_auth_state` belongs
    /// to. Set and cleared together with it; never persisted or logged. The
    /// redirect's one-time code is worthless without it.
    pending_code_verifier: Option<mnemos_ide_sync::desktop_auth::CodeVerifier>,
    /// Live mnemos-ide-sync manager handle. `None` until admin auth completes (B3
    /// desktop redirect path) and reset on log_out. Drop = stop sync.
    ide_sync_manager: Option<Arc<mnemos_ide_sync::SyncManager>>,
    /// Why no manager runs, when startup found a saved admin login it could
    /// not use. Shown as "Sync signed out" rather than leaving sync off without
    /// a word. `None` with no manager means nobody signed in here.
    ide_sync_not_started: Option<String>,
    /// Mnemos IDE: the last "delete synced data" / "delete account" request
    /// from Settings > Sync, for that page to show.
    sync_deletion: Option<SyncDeletionStatus>,
    /// Mnemos IDE: the sync client of a deletion that has not succeeded yet,
    /// kept in memory only so Settings > Sync can retry it. Sync is already
    /// stopped and the login already gone from the keyring by then, so a
    /// failed deletion can never be undone by this machine uploading its
    /// copy again. Cleared by a success, a new sign-in or a sign-out.
    sync_deletion_client: Option<Arc<mnemos_ide_sync::SyncClient>>,
    /// Mnemos IDE: whether the sync server deletes this account's data on
    /// request (`GET /api/tcb/me`, `canDeleteData`). `Some(false)` for a
    /// staff account, which the server always refuses: Settings > Sync then
    /// explains that instead of offering a deletion that would sign this
    /// machine out for nothing. `None` until known, or from a server without
    /// the route (the deletion is offered, and a refusal is explained).
    sync_account_deletable: Option<bool>,
    /// Mnemos IDE: the login of the run being launched was read from the
    /// keyring at startup (saved by an earlier run, possibly an older build),
    /// not made by a sign-in in this process. Tells the sync manager this
    /// machine synced before, so it keeps the team it had
    /// (`SyncManager::with_login_saved_earlier`).
    ide_sync_login_saved_earlier: bool,
    /// Bumped whenever the manager is replaced or dropped, so a status watch
    /// armed for an earlier manager does nothing when it fires.
    ide_sync_generation: u64,
    /// The last queued start or stop of a sync run. Each new one waits for
    /// it, so a stop can never overtake the start it undoes: an app launched
    /// by a sign-in redirect replaces the keyring's run in the same turn that
    /// run was spawned.
    ide_sync_lifecycle: Option<IdeSyncLifecycle>,
    /// Unit tests only: the "login not saved" reason each (re)start of sync
    /// was given, recorded instead of building a real manager (see
    /// `run_ide_sync`).
    #[cfg(test)]
    ide_sync_test_launches: Vec<Option<String>>,
}

impl AuthManager {
    /// Creates a new instance of the AuthManager. The auth state must already be initialized through
    /// [`AuthStateProvider`].
    pub fn new(
        server_api: Arc<ServerApi>,
        auth_client: Arc<dyn AuthClient>,
        ctx: &mut ModelContext<Self>,
    ) -> Self {
        let auth_state = AuthStateProvider::as_ref(ctx).get().clone();

        let mut me = Self {
            auth_state,
            server_api,
            auth_client,
            pending_auth_state: None,
            pending_code_verifier: None,
            ide_sync_manager: None,
            ide_sync_not_started: None,
            sync_deletion: None,
            sync_deletion_client: None,
            sync_account_deletable: None,
            ide_sync_login_saved_earlier: false,
            ide_sync_generation: 0,
            ide_sync_lifecycle: None,
            #[cfg(test)]
            ide_sync_test_launches: Vec::new(),
        };
        // Restart path: the login redirect starts sync, but a rehydrated
        // session (admin tokens already in the keyring) previously never
        // did — the user looked logged in while mnemos-ide-sync stayed off.
        // A saved login that cannot be used reads as "Sync signed out"; no
        // saved login at all is NeverConfigured, which shows nothing.
        me.start_ide_sync(ctx);
        me
    }

    #[cfg(test)]
    pub fn new_for_test(ctx: &mut ModelContext<Self>) -> Self {
        use crate::server::server_api::ServerApiProvider;

        let server_api = ServerApiProvider::as_ref(ctx).get();
        let auth_state = AuthStateProvider::as_ref(ctx).get().clone();

        Self {
            auth_state,
            server_api: server_api.clone(),
            auth_client: server_api,
            pending_auth_state: None,
            pending_code_verifier: None,
            ide_sync_manager: None,
            ide_sync_not_started: None,
            sync_deletion: None,
            sync_deletion_client: None,
            sync_account_deletable: None,
            ide_sync_login_saved_earlier: false,
            ide_sync_generation: 0,
            ide_sync_lifecycle: None,
            ide_sync_test_launches: Vec::new(),
        }
    }

    /// Fetches and ultimately sets the user's auth state from an auth payload.
    /// Typically, this function is triggered when a user clicks the intent link from their browser
    /// back to Mnemos after login (or pastes the URL in the app).
    pub fn initialize_user_from_auth_payload(
        &mut self,
        auth_payload: AuthRedirectPayload,
        enforce_state_validation: bool,
        ctx: &mut ModelContext<Self>,
    ) {
        let AuthRedirectPayload {
            refresh_token,
            access_token,
            user_uid,
            deleted_anonymous_user,
            state,
            code,
        } = auth_payload.clone();

        // Mnemos IDE: the PKCE verifier of the sign-in this redirect claims to
        // finish, taken before the state check below consumes it.
        let code_verifier = self.pending_code_verifier.clone();

        if let Some(received_state) = &state {
            if !self.consume_auth_state(received_state) {
                if self.should_silently_ignore_stale_redirect(&user_uid) {
                    log::info!(
                        "Dropping auth redirect with stale state for already-logged-in user"
                    );
                    return;
                }
                ctx.emit(AuthManagerEvent::AuthFailed(
                    UserAuthenticationError::InvalidStateParameter,
                ));
                return;
            }
        } else if enforce_state_validation {
            if self.should_silently_ignore_stale_redirect(&user_uid) {
                log::info!("Dropping auth redirect without state for already-logged-in user");
                return;
            }
            ctx.emit(AuthManagerEvent::AuthFailed(
                UserAuthenticationError::MissingStateParameter,
            ));
            return;
        }

        // Mnemos IDE: the PKCE desktop sign-in. The redirect carries a
        // one-time code instead of tokens; trade it, with the verifier kept
        // for this sign-in, at the server the app sent the user to (never one
        // the redirect names). A code is accepted only for a sign-in this
        // process started: its state matched above, and only that sign-in's
        // verifier makes the server hand out the tokens.
        if let Some(code) = code {
            if state.is_none() {
                ctx.emit(AuthManagerEvent::AuthFailed(
                    UserAuthenticationError::MissingStateParameter,
                ));
                return;
            }
            let Some(verifier) = code_verifier else {
                ctx.emit(AuthManagerEvent::AuthFailed(
                    UserAuthenticationError::Unexpected(anyhow!(
                        "Received a sign-in code with no sign-in in progress"
                    )),
                ));
                return;
            };
            let issuer = ChannelState::server_root_url().to_string();
            let _ = ctx.spawn(
                mnemos_ide_sync::desktop_auth::exchange_desktop_code(issuer, code, verifier),
                |me, result, ctx| match result {
                    Ok(tokens) => me.complete_admin_login(tokens, ctx),
                    Err(err) => {
                        log::warn!("Admin sign-in failed: {err}");
                        ctx.emit(AuthManagerEvent::AuthFailed(
                            UserAuthenticationError::Unexpected(anyhow!(
                                "Could not finish the sign-in: {err}"
                            )),
                        ));
                    }
                },
            );
            return;
        }

        // Mnemos IDE: the legacy admin redirect (B3: desktop OAuth handoff via
        // /signup/remote), still sent by a server from before PKCE. It
        // carries the admin-platform tokens themselves. Remove once the server
        // stops sending it (Admin repo: LEGACY_TOKEN_REDIRECT_REMOVAL).
        if let Some(access) = access_token {
            log::info!("Admin sign-in used the legacy token redirect");
            self.complete_admin_login(
                mnemos_ide_sync::auth::AdminTokens {
                    access_token: access,
                    refresh_token: refresh_token.get().to_string(),
                    // The admin backend's access-token lifetime is longer;
                    // the next refresh replaces this with the server's window.
                    expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
                },
                ctx,
            );
            return;
        }

        if self.auth_state.is_user_anonymous().unwrap_or_default() {
            let incoming_user_matches_current_user = match user_uid {
                None => false,
                Some(incoming_user_uid) => self
                    .auth_state
                    .user_id()
                    .map(|current_user_uid| current_user_uid == incoming_user_uid)
                    .unwrap_or_default(),
            };
            if !incoming_user_matches_current_user && !deleted_anonymous_user.unwrap_or_default() {
                ctx.emit(AuthManagerEvent::LoginOverrideDetected(auth_payload));
                return;
            }
            send_telemetry_from_ctx!(TelemetryEvent::AnonymousUserLinkedFromBrowser, ctx);
        }

        // Mnemos IDE cut: upstream exchanged the redirect's bare refresh token
        // at Firebase. Mnemos IDE has no Firebase sign-in, so such a redirect
        // is refused here instead of failing later in the token exchange.
        Self::refuse_firebase_sign_in(ctx);
    }

    /// Mnemos IDE: refuse a sign-in that only the upstream Firebase token
    /// exchange could finish (a redirect without a code or admin tokens, or
    /// the device-code login). Mnemos IDE signs in through the admin
    /// platform's PKCE redirect only.
    fn refuse_firebase_sign_in(ctx: &mut ModelContext<Self>) {
        ctx.emit(AuthManagerEvent::AuthFailed(
            UserAuthenticationError::Unexpected(anyhow!(
                "This sign-in method is not supported by Mnemos IDE. Start the sign-in again from the Mnemos app."
            )),
        ));
    }

    /// Mnemos IDE: finish a sign-in to the admin platform with `tokens`,
    /// from the PKCE code exchange or a legacy token redirect. Persists them
    /// to the OS keyring (for mnemos-ide-sync) and marks the user as logged in
    /// via SessionCookie creds. This keeps `is_logged_in()` true without
    /// sending the admin JWT to upstream mnemos-server (which would 401 it).
    /// The Firebase exchange path is skipped entirely; Mnemos IDE callers like
    /// /remote-control read tokens straight from the keyring.
    ///
    /// This does NOT disable upstream Mnemos-cloud calls: anything gated
    /// only on `is_logged_in()` (cloud object polling, refresh_user) still
    /// runs, because SessionCookie counts as logged in, and its requests
    /// go out with `AuthToken::NoAuth`. Such calls must be gated
    /// separately.
    ///
    /// An access token whose claims cannot be read fails the sign-in with
    /// `AuthFailed` and saves nothing. It used to fall back to `User::test()`,
    /// which signed the app in as a placeholder user.
    fn complete_admin_login(
        &mut self,
        tokens: mnemos_ide_sync::auth::AdminTokens,
        ctx: &mut ModelContext<Self>,
    ) {
        let user = match build_user_from_admin_jwt(&tokens.access_token) {
            Ok(user) => user,
            Err(err) => {
                log::warn!("Admin sign-in refused: unreadable access token: {err:#}");
                ctx.emit(AuthManagerEvent::AuthFailed(
                    UserAuthenticationError::Unexpected(
                        err.context("The sign-in returned an unreadable access token"),
                    ),
                ));
                return;
            }
        };
        let (admin_auth, not_saved) = persist_admin_tokens(tokens);
        // Remember that this machine has an admin login, so a keyring that
        // loses it reads as "Sync signed out" on the next start, not as a
        // machine that never signed in.
        Self::set_admin_login_seen(true, ctx);
        self.auth_state.set_user(Some(user));
        self.auth_state
            .set_credentials(Some(Credentials::SessionCookie));
        // A new login always replaces the running manager: it may have
        // halted on a refused session (it never resumes on its own), or
        // belong to whoever was signed in before. This is what makes
        // "Sync signed out" -> sign in work without an app restart.
        let previous = self.ide_sync_manager.take();
        // Mnemos IDE: signing in again after a failed deletion starts over;
        // the old retry no longer applies.
        self.sync_deletion_client = None;
        if matches!(self.sync_deletion, Some(SyncDeletionStatus::Failed(_))) {
            self.sync_deletion = None;
        }
        self.ide_sync_login_saved_earlier = false;
        self.launch_ide_sync(admin_auth, previous, not_saved, ctx);
        ctx.emit(AuthManagerEvent::IdeSyncAuthChanged);
        ctx.emit(AuthManagerEvent::AuthComplete);
    }

    pub fn resume_interrupted_auth_payload(
        &mut self,
        auth_payload: AuthRedirectPayload,
        ctx: &mut ModelContext<Self>,
    ) {
        // Mnemos IDE cut: an interrupted payload only ever reaches here from
        // the Firebase redirect path above, whose token exchange is gone.
        let _ = auth_payload;
        Self::refuse_firebase_sign_in(ctx);
    }

    #[cfg(target_family = "wasm")]
    pub fn initialize_user_from_session_cookie(&self, ctx: &mut ModelContext<Self>) {
        let auth_client = self.auth_client.clone();
        let _ = ctx.spawn(
            async move {
                auth_client
                    .fetch_user(super::credentials::LoginToken::SessionCookie, false)
                    .await
            },
            Self::on_user_fetched,
        );
    }

    /// Refreshes the user's auth state using their existing credentials.
    pub fn refresh_user(&self, ctx: &mut ModelContext<Self>) {
        let Some(credentials) = self.auth_state.credentials() else {
            log::warn!("Attempted to refresh user without credentials");
            return;
        };

        let Some(token) = credentials.login_token() else {
            log::info!("Attempted to refresh a user with no login token, skipping");
            return;
        };

        let auth_client = self.auth_client.clone();
        let _ = ctx.spawn(
            async move { auth_client.fetch_user(token, true).await },
            Self::on_user_fetched,
        );
    }

    /// Authenticate using the OAuth2 device authorization flow.
    ///
    /// Mnemos IDE cut: upstream asked mnemos-server for a device code and
    /// exchanged the approved code for a Firebase custom token. The Mnemos
    /// sync server has no device-code endpoint and Mnemos IDE has no Firebase
    /// sign-in, so this always fails with `AuthFailed`.
    #[cfg_attr(target_family = "wasm", allow(dead_code))]
    pub fn authorize_device(&self, ctx: &mut ModelContext<Self>) {
        // Clear any stale user state so old credentials don't linger after
        // the failed sign-in.
        self.auth_state.set_credentials(None);
        Self::refuse_firebase_sign_in(ctx);
    }

    /// Callback for handling a successful fetch of a user from mnemos-server and Firebase.
    /// This does the heavy-lifting of setting up all components of the application that depend
    /// on a user's authenticated state, and emits events to subscribers that let them know
    /// an auth event has occurred.
    fn on_user_fetched(
        &mut self,
        fetch_user_result: StdResult<FetchUserResult, UserAuthenticationError>,
        ctx: &mut ModelContext<Self>,
    ) {
        match fetch_user_result {
            Ok(fetch_user_result) => {
                let FetchUserResult {
                    user,
                    credentials,
                    from_refresh,
                    llms,
                } = fetch_user_result;

                self.set_and_persist(Some(user.clone()), Some(credentials), ctx);

                self.set_needs_reauth(false, ctx);

                // Must be called on the main thread.
                #[cfg(feature = "crash_reporting")]
                crate::crash_reporting::set_user_id(
                    user.local_id,
                    Some(user.metadata.email.clone()),
                    ctx,
                );

                SettingsInitializer::handle(ctx).update(ctx, |initializer, ctx| {
                    initializer.handle_user_fetched(self.auth_state.clone(), ctx);
                });

                // Reset the initial-load condition so that any cloud preference
                // sync waits for the *new* user's cloud objects rather than
                // resolving immediately against stale data from a prior session.
                // Only do this for non-refresh fetches (login/signup), not for
                // token refreshes where the user identity hasn't changed.
                if !from_refresh {
                    UpdateManager::handle(ctx).update(ctx, |manager, _| {
                        manager.reset_initial_load();
                    });
                }

                // Now that we have a user, start polling for team and cloud object information.
                // The polling loop's first tick fires immediately, so there is no need for a
                // separate out-of-band refresh here.
                TeamTesterStatus::handle(ctx).update(ctx, |model, ctx| {
                    model.initiate_data_pollers(false, ctx);
                });

                CloudPreferencesSyncer::handle(ctx).update(ctx, |model, ctx| {
                    model.handle_user_fetched(self.auth_state.clone(), ctx)
                });

                AIRequestUsageModel::handle(ctx).update(ctx, |usage_model, ctx| {
                    usage_model.refresh_request_usage_async(ctx);
                });

                LLMPreferences::handle(ctx).update(ctx, |prefs, ctx| {
                    prefs.update_feature_model_choices(Ok(llms), ctx);
                });

                PersistedWorkspace::handle(ctx).update(ctx, |index_manager_updater, ctx| {
                    index_manager_updater.on_user_changed(ctx);
                });

                if !user.is_user_anonymous() {
                    GeneralSettings::handle(ctx).update(ctx, |settings, ctx| {
                        report_if_error!(settings
                            .did_non_anonymous_user_log_in
                            .set_value(true, ctx));
                    });
                }

                // Force refresh for shared sessions if user may have changed.
                if !from_refresh {
                    SharedSessionManager::handle(ctx).update(ctx, |manager, ctx| {
                        manager.stop_all_shared_sessions(ctx);
                        manager.rejoin_all_shared_sessions(ctx);
                    });
                }

                let global_resource_handles =
                    GlobalResourceHandlesProvider::as_ref(ctx).get().clone();

                // As part of Logout v0:
                // Reconstruct the database if it was removed.
                // Do nothing if the database was not removed.
                persistence::reconstruct(&global_resource_handles.model_event_sender);
                if let Some(model_event_sender) = &global_resource_handles.model_event_sender {
                    if let Err(e) =
                        model_event_sender.send(ModelEvent::UpsertCurrentUserInformation {
                            user_information: PersistedCurrentUserInformation {
                                email: self.auth_state.user_email().unwrap_or_default(),
                            },
                        })
                    {
                        log::error!("Error persisting user information to database: {e:?}");
                    };
                }

                // Fetch the user's privacy settings from the server if any or update the server settings.
                let privacy_settings_handle = PrivacySettings::handle(ctx);
                let privacy_settings_snapshot =
                    privacy_settings_handle.as_ref(ctx).get_snapshot(ctx);
                ctx.update_model(&privacy_settings_handle, |privacy_settings, ctx| {
                    privacy_settings.fetch_or_update_settings(ctx);
                });

                // Now that the user is logged in, do the daily version check.
                if FeatureFlag::Autoupdate.is_enabled() {
                    AutoupdateState::handle(ctx).update(ctx, |autoupdate_state, ctx| {
                        autoupdate_state.maybe_daily_check_for_update(ctx);
                    });
                }

                let server_api = self.server_api.clone();
                let user_id = self.auth_state.user_id().unwrap_or_default();
                let anonymous_id = self.auth_state.anonymous_id();
                let _ = ctx.spawn(
                    // Synchronously add the identify and login event to the telemetry event queue and
                    // then flush the queue to ensure the events get to Rudderstack. We need to do this
                    // one-off because the login event happens only once for the user and we don't want
                    // to drop the event if the user quits the app before the next flush of the queue.
                    // TODO(alokedesai): Investigate a more robust way of handling events
                    // that don't get flushed to Rudderstack outside of this event specifically.
                    async move {
                        mnemos_ui::telemetry::record_identify_user_event(
                            user_id.as_string(),
                            anonymous_id.clone(),
                            mnemos_ui::time::get_current_time(),
                        );
                        mnemos_ui::telemetry::record_event(
                            Some(user_id.as_string()),
                            anonymous_id,
                            TelemetryEvent::Login.name().into(),
                            TelemetryEvent::Login.payload(),
                            TelemetryEvent::Login.contains_ugc(),
                            mnemos_ui::time::get_current_time(),
                        );

                        // Note that this snapshot might get overwritten to disabled after the server fetch.
                        // However, it is still fine to flush to Rudderstack here as the login event is low-risk
                        // and it is better to err on the side of over-reporting than under-reporting.
                        if let Err(e) = server_api
                            .flush_telemetry_events(privacy_settings_snapshot)
                            .await
                        {
                            log::info!("Failed to flush events from Telemetry queue: {e}");
                        }
                        server_api.notify_login().await;
                    },
                    |_, _, _| {},
                );

                // Once the user is authenticated, attempt to report the sandbox that Mnemos is running in, if any.
                ctx.spawn(
                    async { mnemos_isolation_platform::detect() },
                    |_, platform, ctx| {
                        if let Some(platform) = platform {
                            send_telemetry_from_ctx!(
                                TelemetryEvent::DetectedIsolationPlatform { platform },
                                ctx
                            );
                        }
                    },
                );

                ctx.emit(AuthManagerEvent::AuthComplete);
            }
            Err(error) => {
                match error {
                    UserAuthenticationError::DeniedAccessToken(_) => {
                        self.set_needs_reauth(true, ctx);
                    }
                    UserAuthenticationError::UserAccountDisabled(_) => {}
                    UserAuthenticationError::Unexpected(_) => {}
                    UserAuthenticationError::InvalidStateParameter => {}
                    UserAuthenticationError::MissingStateParameter => {}
                }

                ctx.emit(AuthManagerEvent::AuthFailed(error));
            }
        }
    }

    /// Sets the user and credentials in auth state and persists to secure storage.
    /// Persistence depends on the credential type - currently, we only persist
    /// state if authenticated via a Firebase token.
    fn set_and_persist(
        &self,
        user: Option<User>,
        credentials: Option<Credentials>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.auth_state.set_user(user);
        self.auth_state.set_credentials(credentials);
        self.persist(ctx);
    }

    /// Persists (or removes) the current user and credentials to/from secure storage,
    /// based on the current auth state.
    fn persist(&self, ctx: &mut ModelContext<Self>) {
        match self.auth_state.persist_action() {
            PersistAction::Persist(persisted_user) => {
                if persisted_user.auth_tokens.refresh_token.is_empty() {
                    log::warn!("Skipping user persistence due to empty refresh token");
                    return;
                }
                let _ = persisted_user.write_to_secure_storage(ctx).map_err(|err| {
                    log::warn!("Unable to persist user to secure storage: {err:?}");
                });
            }
            PersistAction::Remove => {
                let _ = PersistedUser::remove_from_secure_storage(ctx).map_err(|err| {
                    log::warn!("Unable to clear user from secure storage: {err:?}");
                });
            }
            PersistAction::DoNothing => {}
        }
    }

    /// Helper function for logging out the user.
    /// NOTE: You probably want to call auth::log_out instead; this only manages the auth state,
    /// it doesn't shut down any other user-dependent parts of the app.
    /// TODO(jeff): Can we move those pieces in here?
    pub(super) fn log_out(&mut self, ctx: &mut ModelContext<Self>) {
        self.sign_out_of_admin(ctx);
    }

    /// Mnemos IDE: sign out of the admin platform only. Stops mnemos-ide-sync, forgets the
    /// admin login (keyring and auth state) and leaves everything else alone:
    /// the SQLite workspace, the open windows and tabs. `auth::log_out` calls
    /// only this under the admin login; the full log out is also built on it.
    ///
    /// Afterwards the app is in the same state as a first start on a machine
    /// with no admin login (`SyncAuthState::NeverConfigured`): no chip, and
    /// the "Sync is off" row on Settings > Sync offers the sign-in again.
    pub(super) fn sign_out_of_admin(&mut self, ctx: &mut ModelContext<Self>) {
        // Clear any dangling CSRF token from an auth flow that was started but never
        // completed before this logout, so it can't be replayed against the next session
        // in the same process.
        self.pending_auth_state = None;
        // Take this machine off the live agents feed before the login goes:
        // queued ahead of the stop, with the manager's in-memory tokens.
        #[cfg(not(test))]
        if let Some(manager) = self.ide_sync_manager.clone() {
            let machine = crate::workspace::live_agents::this_machine_key();
            self.queue_ide_sync_lifecycle(
                async move {
                    if let Err(e) = manager.client.delete_live_agents(&machine).await {
                        log::info!("mnemos-ide-sync: removing the live agents key failed: {e}");
                    }
                },
                ctx,
            );
        }
        self.pending_code_verifier = None;
        // Mnemos IDE: a deletion left to retry belongs to the login going away.
        self.sync_deletion_client = None;
        self.stop_ide_sync(ctx);
        // The keyring is the real OS credential store, not a test double: under
        // `cargo test` this wiped the developer's admin login (2026-09-24).
        #[cfg(not(test))]
        let _ = mnemos_ide_sync::auth::AdminAuth::clear_keyring();
        // A reason recorded at startup (say, the keyring could not be read)
        // no longer applies: nothing is signed in now, which is what `None`
        // reads as.
        self.ide_sync_not_started = None;
        // Signing out on purpose is not a lost login: the next start is
        // NeverConfigured, with no chip.
        Self::set_admin_login_seen(false, ctx);
        self.set_and_persist(None, None, ctx);
        // `stop_ide_sync` only emits when a manager was running. The profile
        // menu and Settings re-render on this event, so without it they would
        // keep showing the signed-in state.
        ctx.emit(AuthManagerEvent::IdeSyncAuthChanged);
    }

    /// Mnemos IDE: records whether this machine has an admin login the user has
    /// not signed out of (see `GeneralSettings::admin_login_seen`).
    fn set_admin_login_seen(seen: bool, ctx: &mut ModelContext<Self>) {
        GeneralSettings::handle(ctx).update(ctx, |settings, ctx| {
            if *settings.admin_login_seen != seen {
                report_if_error!(settings.admin_login_seen.set_value(seen, ctx));
            }
        });
    }

    /// Live mnemos-ide-sync manager, if admin auth has completed. The Drive panel
    /// uses this for the sync-status watch channel and team operations.
    pub fn ide_sync_manager(&self) -> Option<Arc<mnemos_ide_sync::SyncManager>> {
        self.ide_sync_manager.clone()
    }

    /// What the UI shows about mnemos-ide-sync's admin login: the header chip and
    /// the Settings > Sync row. [`AuthManagerEvent::IdeSyncAuthChanged`]
    /// fires when it changes.
    pub fn ide_sync_auth(&self) -> mnemos_ide_sync::SyncAuthState {
        super::ide_sync_auth::resolve(
            self.ide_sync_not_started.as_deref(),
            self.ide_sync_manager.as_ref().map(|m| m.auth_state()),
        )
    }

    /// Open the admin platform's desktop sign-in. Its redirect lands in
    /// `initialize_user_from_auth_payload`, which restarts mnemos-ide-sync with the
    /// new login.
    pub fn open_admin_sign_in(&mut self, ctx: &mut ModelContext<Self>) {
        let url = self.admin_sign_in_url();
        ctx.open_url(&url);
    }

    /// The URL [`Self::open_admin_sign_in`] opens.
    ///
    /// Reuses a sign-in already in flight instead of starting a new one. A
    /// second click (the browser was slow, or the chip and then Settings)
    /// would otherwise replace the pending state, and finishing the sign-in
    /// in the first tab would fail with `InvalidStateParameter`, which
    /// nothing on this path shows. The state is still consumed once, by
    /// whichever tab finishes first.
    fn admin_sign_in_url(&mut self) -> String {
        let state = match self.pending_auth_state.clone() {
            Some(state) => state,
            None => self.generate_auth_state(),
        };
        // The same verifier as the first click, so the challenge matches too.
        let challenge = self.pending_code_challenge();
        // /signup/remote is the only remote-handoff route the admin frontend
        // serves (it handles sign-in too); /login/remote 404s.
        Self::remote_sign_up_url(&state, &challenge)
    }

    /// Startup: sync with the login saved in the keyring, if there is one.
    /// Runs while the model is being built, so it records why sync is off
    /// instead of emitting an event nobody can hear yet.
    fn start_ide_sync(&mut self, ctx: &mut ModelContext<Self>) {
        if self.ide_sync_manager.is_some() {
            return;
        }
        // A login saved for another server (a build pointed elsewhere) is
        // refused rather than replayed against this one.
        let loaded =
            mnemos_ide_sync::auth::AdminAuth::load_for_server(&ChannelState::server_root_url());
        // Only a definite answer settles the live-feed default: a keyring
        // that could not be read this time is asked again on the next start.
        let has_login = match &loaded {
            Ok(auth) => Some(auth.tokens.is_some()),
            Err(mnemos_ide_sync::IdeError::NotAuthenticated) => Some(false),
            Err(_) => None,
        };
        if let Some(has_login) = has_login {
            Self::pin_live_feed_for_existing_login(has_login, ctx);
        }
        let reason = match loaded {
            // A legacy entry with neither shape of token: every request
            // would fail with "not authenticated" and never recover.
            Ok(auth) if auth.tokens.is_none() => "The saved admin login has no tokens".to_owned(),
            Ok(auth) => {
                self.ide_sync_login_saved_earlier = true;
                self.launch_ide_sync(auth, None, None, ctx);
                return;
            }
            // No login in the keyring. If this machine had one (and the user
            // did not sign out), the keyring lost it or never saved it: warn.
            // Otherwise nobody signed in here: sync is optional, so there is
            // nothing to warn about (`SyncAuthState::NeverConfigured`).
            Err(mnemos_ide_sync::IdeError::NotAuthenticated) => {
                let seen = *GeneralSettings::as_ref(ctx).admin_login_seen;
                match super::ide_sync_auth::missing_login_reason(seen) {
                    Some(reason) => reason,
                    None => {
                        log::info!("mnemos-ide-sync: not started: no admin login on this machine");
                        self.ide_sync_not_started = None;
                        return;
                    }
                }
            }
            Err(e) => format!("Could not read the saved admin login: {e}"),
        };
        log::warn!("mnemos-ide-sync: not started: {reason}");
        self.ide_sync_not_started = Some(reason);
    }

    /// Mnemos IDE: the state of the last deletion request, for Settings > Sync.
    pub(crate) fn sync_deletion(&self) -> Option<&SyncDeletionStatus> {
        self.sync_deletion.as_ref()
    }

    /// Mnemos IDE: whether Settings > Sync can offer a deletion: sync runs
    /// here, or an earlier deletion failed and can be retried, and the server
    /// has not said it refuses this account.
    pub(crate) fn can_delete_synced_data(&self) -> bool {
        self.sync_deletion_offer() == super::ide_sync_auth::DeletionOffer::Offered
    }

    /// Mnemos IDE: what Settings > Sync offers for deleting synced data.
    pub(crate) fn sync_deletion_offer(&self) -> super::ide_sync_auth::DeletionOffer {
        super::ide_sync_auth::deletion_offer(
            self.ide_sync_manager.is_some(),
            self.sync_deletion_client.is_some(),
            self.sync_account_deletable,
        )
    }

    /// Mnemos IDE: ask the server whether it deletes this account's data,
    /// for the current run only.
    #[cfg_attr(test, allow(dead_code))]
    fn fetch_sync_account_info(
        &mut self,
        client: Arc<mnemos_ide_sync::SyncClient>,
        ctx: &mut ModelContext<Self>,
    ) {
        let generation = self.ide_sync_generation;
        ctx.spawn(
            async move { client.account_info().await },
            move |me, result, ctx| {
                if me.ide_sync_generation != generation {
                    return;
                }
                match result {
                    Ok(info) => {
                        me.sync_account_deletable = info.map(|i| i.can_delete_data);
                        ctx.emit(AuthManagerEvent::IdeSyncAuthChanged);
                    }
                    Err(e) => log::info!("mnemos-ide-sync: account info not read: {e}"),
                }
            },
        );
    }

    /// Mnemos IDE: delete everything this account synced (`account` false), or
    /// the account itself (`account` true), on the sync server.
    ///
    /// This machine signs out of sync first: the run is stopped and the login
    /// removed from the keyring before the request is sent, and the request
    /// waits for the stop to finish, so no push or poll from here can land
    /// after (or during) the deletion, whatever its outcome. A failed deletion
    /// keeps the client in memory for a retry from Settings > Sync and never
    /// restarts sync. Only a 403 or 404 (the server refuses this account or
    /// has no deletion endpoint, so nothing was deleted) signs back in with
    /// the same login. Other machines
    /// still signed in to the account upload theirs on their next sync; the
    /// confirmation in Settings > Sync says to sign them out first.
    pub(crate) fn delete_synced_data(&mut self, account: bool, ctx: &mut ModelContext<Self>) {
        if matches!(self.sync_deletion, Some(SyncDeletionStatus::InProgress { .. })) {
            return;
        }
        // The server would refuse it (403) after this machine signed out for
        // it: say so and leave sync running.
        if self.sync_deletion_offer() == super::ide_sync_auth::DeletionOffer::RefusedForAccount {
            self.sync_deletion = Some(SyncDeletionStatus::Failed(
                super::ide_sync_auth::STAFF_ACCOUNT_DELETION_REFUSED.to_owned(),
            ));
            ctx.emit(AuthManagerEvent::IdeSyncAuthChanged);
            return;
        }
        let client = match (self.sync_deletion_client.clone(), self.ide_sync_manager.clone()) {
            (Some(client), _) => client,
            (None, Some(manager)) => {
                let client = manager.client.clone();
                // Stops the run (queued), removes the live agents key and
                // clears the keyring and the auth state.
                self.sign_out_of_admin(ctx);
                client
            }
            (None, None) => {
                self.sync_deletion = Some(SyncDeletionStatus::Failed(
                    "Not signed in to sync on this machine".to_owned(),
                ));
                ctx.emit(AuthManagerEvent::IdeSyncAuthChanged);
                return;
            }
        };
        self.sync_deletion_client = Some(client.clone());
        self.sync_deletion = Some(SyncDeletionStatus::InProgress { account });
        ctx.emit(AuthManagerEvent::IdeSyncAuthChanged);
        // Every start and stop queued so far, the stop above included.
        let stopped = self.ide_sync_lifecycle.clone();
        ctx.spawn(
            async move {
                if let Some(stopped) = stopped {
                    stopped.await;
                }
                let result = if account {
                    client.delete_my_account().await
                } else {
                    client.delete_my_synced_data().await
                };
                // A refresh during the request saves the rotated login to the
                // keyring; take it back out so the next start stays signed
                // out. Keep the tokens for the one case that signs back in.
                let tokens = client.auth.lock().await.tokens.clone();
                let profile_id = client.profile_id().await;
                #[cfg(not(test))]
                let _ = mnemos_ide_sync::auth::AdminAuth::clear_keyring();
                (result, tokens, profile_id)
            },
            move |me, (result, tokens, profile_id), ctx| {
                match result {
                    Ok(()) => {
                        log::info!(
                            "mnemos-ide-sync: deleted {} on the sync server",
                            if account { "the account" } else { "the synced data" }
                        );
                        // The encrypted data and its marker are gone with it,
                        // so the recovery key saved here protects nothing. Kept,
                        // it would read as "marker missing" at the next sign-in
                        // and block starting over unencrypted.
                        if let Some(profile_id) = profile_id {
                            if let Err(e) = super::ide_sync_e2e::remove(ctx, &profile_id) {
                                log::warn!(
                                    "mnemos-ide-sync: the recovery key of the deleted data was not forgotten: {e}"
                                );
                            }
                        }
                        me.sync_deletion_client = None;
                        me.sync_deletion = Some(SyncDeletionStatus::Done { account });
                    }
                    Err(e) => {
                        log::warn!("mnemos-ide-sync: deletion failed: {e}");
                        // 403: the server refuses this account. 404: a
                        // server without the deletion endpoints. Neither
                        // deleted anything.
                        let refused = matches!(
                            &e,
                            mnemos_ide_sync::IdeError::Http { status, .. }
                                if matches!(status.as_u16(), 403 | 404)
                        );
                        me.sync_deletion = Some(SyncDeletionStatus::Failed(e.to_string()));
                        // Nothing was deleted, so there is nothing to
                        // protect: sync resumes with this login.
                        if let (true, Some(tokens)) = (refused, tokens) {
                            me.sync_deletion_client = None;
                            me.complete_admin_login(tokens, ctx);
                            // complete_admin_login clears a failure; this one
                            // is still worth showing.
                            me.sync_deletion = Some(SyncDeletionStatus::Failed(format!(
                                "{e}. Nothing was deleted and sync continues."
                            )));
                        }
                    }
                }
                ctx.emit(AuthManagerEvent::IdeSyncAuthChanged);
            },
        );
    }

    /// Mnemos IDE: the live agents feed used to be always on and is now off
    /// by default. The first time a build with the setting starts, an install
    /// that already has an admin login keeps the feed on (the private
    /// `live_feed_legacy_default` pin, never the synced settings.toml key).
    /// Runs once per install, whatever it finds, so a later first sign-in on a
    /// new install still gets the new default.
    fn pin_live_feed_for_existing_login(has_login: bool, ctx: &mut ModelContext<Self>) {
        GeneralSettings::handle(ctx).update(ctx, |settings, ctx| {
            if *settings.live_feed_default_checked {
                return;
            }
            if super::ide_sync_auth::live_feed_pin_needed(
                has_login,
                settings.live_feed_enabled.is_value_explicitly_set(),
            ) {
                report_if_error!(settings.live_feed_legacy_default.set_value(true, ctx));
            }
            report_if_error!(settings.live_feed_default_checked.set_value(true, ctx));
        });
    }

    /// Start sync with `auth` once `previous` (if any) has stopped.
    /// `token_not_saved` is why the keyring refused this login, if it did.
    fn launch_ide_sync(
        &mut self,
        auth: mnemos_ide_sync::auth::AdminAuth,
        previous: Option<Arc<mnemos_ide_sync::SyncManager>>,
        token_not_saved: Option<String>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.ide_sync_not_started = None;
        self.ide_sync_generation += 1;
        self.sync_account_deletable = None;
        self.run_ide_sync(auth, previous, token_not_saved, ctx);
    }

    #[cfg(not(test))]
    fn run_ide_sync(
        &mut self,
        auth: mnemos_ide_sync::auth::AdminAuth,
        previous: Option<Arc<mnemos_ide_sync::SyncManager>>,
        token_not_saved: Option<String>,
        ctx: &mut ModelContext<Self>,
    ) {
        let toml_path = crate::settings::user_preferences_toml_file_path();
        // Mnemos IDE: the end-to-end encryption keys saved on this machine.
        let e2e_keys = super::ide_sync_e2e::load(ctx);
        // Mnemos IDE: settings.toml syncs per key, by the registry's scopes.
        let policy = crate::settings::ide_sync_policy::settings_toml_sync_policy();
        let manager = Arc::new(
            mnemos_ide_sync::SyncManager::from_auth(auth)
                .with_mnemos_settings_toml(toml_path)
                .with_e2e_keys(e2e_keys)
                .with_settings_policy(policy)
                .with_login_saved_earlier(self.ide_sync_login_saved_earlier),
        );
        if let Some(reason) = token_not_saved {
            manager.note_token_not_saved(reason);
        }
        self.ide_sync_manager = Some(manager.clone());
        self.watch_ide_sync_auth(&manager, ctx);
        self.fetch_sync_account_info(manager.client.clone(), ctx);
        self.queue_ide_sync_lifecycle(
            async move {
                // The old run must be over before the new one starts: both
                // would watch and push the same files.
                if let Some(previous) = previous {
                    if let Err(e) = previous.stop().await {
                        log::warn!("mnemos-ide-sync: stopping the previous run failed: {e}");
                    }
                }
                if let Err(e) = manager.start().await {
                    log::warn!("mnemos-ide-sync: start failed: {e}");
                }
            },
            ctx,
        );
    }

    /// Mnemos IDE: stop and start the current sync run again, after every
    /// start or stop queued before it. Used once end-to-end encryption is
    /// turned on or unlocked here, so the run starts over in the encrypted
    /// namespace (its first pull encrypts what is still plaintext).
    pub fn restart_ide_sync(&mut self, ctx: &mut ModelContext<Self>) {
        let Some(manager) = self.ide_sync_manager.clone() else {
            return;
        };
        self.queue_ide_sync_lifecycle(
            async move {
                if let Err(e) = manager.clone().stop().await {
                    log::warn!("mnemos-ide-sync: stopping for a restart failed: {e}");
                }
                if let Err(e) = manager.start().await {
                    log::warn!("mnemos-ide-sync: restart failed: {e}");
                }
            },
            ctx,
        );
        ctx.emit(AuthManagerEvent::IdeSyncAuthChanged);
    }

    /// Run `op` after every start or stop queued before it.
    fn queue_ide_sync_lifecycle(
        &mut self,
        op: impl std::future::Future<Output = ()> + Send + 'static,
        ctx: &mut ModelContext<Self>,
    ) {
        use futures::FutureExt as _;
        let prior = self.ide_sync_lifecycle.take();
        let next = async move {
            if let Some(prior) = prior {
                prior.await;
            }
            op.await;
        }
        .boxed()
        .shared();
        self.ide_sync_lifecycle = Some(next.clone());
        ctx.spawn(next, |_, _, _| {});
    }

    /// Unit tests: adopt `manager` as the current run without starting it.
    /// The caller builds it under a test data dir (see `run_ide_sync`).
    #[cfg(test)]
    pub(crate) fn set_ide_sync_manager_for_test(&mut self, manager: Arc<mnemos_ide_sync::SyncManager>) {
        self.ide_sync_not_started = None;
        self.ide_sync_manager = Some(manager);
    }

    /// Unit tests never build a manager: building one resolves the real
    /// Mnemos data dir and `~/.claude`, and a started one talks to the admin
    /// server. They check what would have started instead.
    #[cfg(test)]
    fn run_ide_sync(
        &mut self,
        _auth: mnemos_ide_sync::auth::AdminAuth,
        _previous: Option<Arc<mnemos_ide_sync::SyncManager>>,
        token_not_saved: Option<String>,
        _ctx: &mut ModelContext<Self>,
    ) {
        // Read by the real run (`with_login_saved_earlier`).
        let _ = self.ide_sync_login_saved_earlier;
        self.ide_sync_test_launches.push(token_not_saved);
    }

    /// Emit [`AuthManagerEvent::IdeSyncAuthChanged`] whenever `manager`'s
    /// login state changes, for as long as it is the current manager.
    #[cfg(not(test))]
    fn watch_ide_sync_auth(
        &mut self,
        manager: &Arc<mnemos_ide_sync::SyncManager>,
        ctx: &mut ModelContext<Self>,
    ) {
        let generation = self.ide_sync_generation;
        let mut status = manager.status();
        // Mnemos IDE: the hooks opt-in and the approvals waiting in Settings >
        // Sync re-render the same views as the login state.
        let watched = |s: &mnemos_ide_sync::SyncStatusSnapshot| {
            (s.auth.clone(), s.hooks_sync, s.pending_approvals.clone())
        };
        let seen = watched(&*status.borrow_and_update());
        ctx.spawn(
            async move {
                // Only a change of login state (or of the gates) is worth an
                // event: the snapshot also moves on every push and pull.
                loop {
                    // Err: the manager is gone (replaced or logged out).
                    status.changed().await.ok()?;
                    let changed = watched(&*status.borrow_and_update()) != seen;
                    if changed {
                        return Some(());
                    }
                }
            },
            move |me, changed, ctx| {
                if changed.is_none() || me.ide_sync_generation != generation {
                    return;
                }
                ctx.emit(AuthManagerEvent::IdeSyncAuthChanged);
                if let Some(manager) = me.ide_sync_manager.clone() {
                    me.watch_ide_sync_auth(&manager, ctx);
                }
            },
        );
    }

    fn stop_ide_sync(&mut self, ctx: &mut ModelContext<Self>) {
        self.ide_sync_generation += 1;
        let Some(manager) = self.ide_sync_manager.take() else {
            return;
        };
        self.queue_ide_sync_lifecycle(
            async move {
                if let Err(e) = manager.stop().await {
                    log::warn!("mnemos-ide-sync: stop failed: {e}");
                }
            },
            ctx,
        );
        ctx.emit(AuthManagerEvent::IdeSyncAuthChanged);
    }

    /// Sets whether or not this user's Firebase credentials are invalid and thus needs to reauth.
    pub fn set_needs_reauth(&self, needs_reauth: bool, ctx: &mut ModelContext<Self>) {
        let became_true = self.auth_state.set_needs_reauth(needs_reauth);

        if became_true {
            send_telemetry_from_ctx!(TelemetryEvent::NeedsReauth, ctx);
            ctx.emit(AuthManagerEvent::NeedsReauth);
        }
    }

    pub fn attempt_login_gated_feature(
        &self,
        feature: LoginGatedFeature,
        auth_view_variant: AuthViewVariant,
        ctx: &mut ModelContext<Self>,
    ) {
        if self.auth_state.is_anonymous_or_logged_out() {
            send_telemetry_from_ctx!(
                TelemetryEvent::AnonymousUserAttemptLoginGatedFeature { feature },
                ctx
            );
            ctx.emit(AuthManagerEvent::AttemptedLoginGatedFeature { auth_view_variant });
        };
    }

    pub fn anonymous_user_hit_drive_object_limit(&self, ctx: &mut ModelContext<Self>) {
        if self.auth_state.is_anonymous_or_logged_out() {
            send_telemetry_from_ctx!(TelemetryEvent::AnonymousUserHitCloudObjectLimit, ctx);
            ctx.emit(AuthManagerEvent::AttemptedLoginGatedFeature {
                auth_view_variant: AuthViewVariant::HitDriveObjectLimitCloseable,
            });
        };
    }

    pub fn initiate_anonymous_user_linking(
        &self,
        entrypoint: AnonymousUserSignupEntrypoint,
        ctx: &mut ModelContext<Self>,
    ) {
        let auth_client = self.auth_client.clone();
        let _ = ctx.spawn(
            async move { auth_client.fetch_new_custom_token().await },
            move |me, response, ctx| {
                let custom_token = me.auth_client.on_custom_token_fetched(response);

                match custom_token {
                    Ok(custom_token) => {
                        // Send synchronously since this is an important event in the sign up funnel and we
                        // don't want to lose events if the user quits before the event queue is flushed.
                        send_telemetry_sync_from_ctx!(
                            TelemetryEvent::InitiateAnonymousUserSignup { entrypoint },
                            ctx
                        );
                        let login_options_url = me.login_options_url(&custom_token);
                        if cfg!(target_family = "wasm") {
                            #[cfg(target_family = "wasm")]
                            if let Some(current_url) = parse_current_url() {
                                update_browser_url(
                                    Url::parse(&format!(
                                        "{}?redirect_to={}",
                                        login_options_url,
                                        current_url.path()
                                    ))
                                    .ok(),
                                    true,
                                );
                            } else {
                                update_browser_url(Url::parse(&login_options_url).ok(), true);
                            }
                        } else {
                            ctx.open_url(&login_options_url);
                        }
                    }
                    Err(e) => {
                        ctx.emit(AuthManagerEvent::MintCustomTokenFailed(e));
                    }
                }
            },
        );
    }

    pub fn copy_anonymous_user_linking_url_to_clipboard(&self, ctx: &mut ModelContext<Self>) {
        if !self.auth_state.is_user_anonymous().unwrap_or_default() {
            return;
        }
        let auth_client = self.auth_client.clone();
        let _ = ctx.spawn(
            async move { auth_client.fetch_new_custom_token().await },
            move |me, response, ctx| {
                let custom_token = me.auth_client.on_custom_token_fetched(response);

                match custom_token {
                    Ok(custom_token) => {
                        let login_options_url = me.login_options_url(&custom_token);
                        ctx.clipboard().write(ClipboardContent {
                            plain_text: login_options_url,
                            paths: None,
                            ..Default::default()
                        });
                    }
                    Err(e) => {
                        ctx.emit(AuthManagerEvent::MintCustomTokenFailed(e));
                    }
                };
            },
        );
    }

    /// Generates a unique state parameter for the authentication flow.
    ///
    /// Mnemos IDE: also starts a fresh PKCE verifier, so a state and its
    /// verifier always belong to the same sign-in.
    fn generate_auth_state(&mut self) -> String {
        let state = Uuid::new_v4().to_string();
        self.pending_auth_state = Some(state.clone());
        self.pending_code_verifier = Some(mnemos_ide_sync::desktop_auth::CodeVerifier::generate());
        state
    }

    pub fn sign_up_url(&mut self) -> String {
        let state = self.generate_auth_state();
        let challenge = self.pending_code_challenge();
        Self::remote_sign_up_url(&state, &challenge)
    }

    /// Mnemos IDE: the S256 challenge of the pending sign-in's verifier,
    /// starting a verifier if the pending state has none.
    fn pending_code_challenge(&mut self) -> String {
        self.pending_code_verifier
            .get_or_insert_with(mnemos_ide_sync::desktop_auth::CodeVerifier::generate)
            .challenge()
    }

    /// Mnemos IDE: the admin platform's desktop sign-in. `code_challenge` asks
    /// for the PKCE redirect (a one-time code, no tokens in the URL). `scheme`
    /// is still sent because a server from before PKCE requires it; a PKCE
    /// server ignores it and only ever redirects to its own allowlisted
    /// scheme. A server that predates PKCE ignores the challenge and sends
    /// the legacy token redirect, which `initialize_user_from_auth_payload`
    /// still accepts.
    fn remote_sign_up_url(state: &str, code_challenge: &str) -> String {
        format!(
            // TODO: we should probably be able to remove the public_beta flag
            "{}/signup/remote?scheme={}&state={}&code_challenge={}&code_challenge_method={}&public_beta=true",
            ChannelState::server_root_url(),
            ChannelState::url_scheme(),
            state,
            code_challenge,
            mnemos_ide_sync::desktop_auth::CODE_CHALLENGE_METHOD,
        )
    }

    /// Mnemos IDE: the admin platform's desktop sign-in, same as
    /// [`Self::open_admin_sign_in`]. It serves no /login/remote (that 404s),
    /// and reusing a pending state keeps "Copy URL" followed by "Log in", or
    /// a second click, from voiding the first tab.
    pub fn sign_in_url(&mut self) -> String {
        self.admin_sign_in_url()
    }

    pub fn login_options_url(&mut self, custom_token: &str) -> String {
        let state = self.generate_auth_state();
        format!(
            "{}/login_options/{}?state={}",
            ChannelState::server_root_url(),
            custom_token,
            state,
        )
    }

    pub fn link_sso_url(&mut self, email: &str) -> String {
        let state = self.generate_auth_state();
        format!(
            "{}/link_sso?email={}&state={}",
            ChannelState::server_root_url(),
            email,
            state,
        )
    }

    /// Validates and consumes the pending auth state token. Returns `true` if the
    /// provided state matches; in that case the pending state is cleared so the
    /// CSRF token is single-use. A subsequent call with the same value will fail.
    ///
    /// Mnemos IDE: the PKCE verifier of that sign-in is dropped with it, so a
    /// second redirect cannot reuse it.
    fn consume_auth_state(&mut self, received_state: &str) -> bool {
        if self.pending_auth_state.as_deref() == Some(received_state) {
            self.pending_auth_state = None;
            self.pending_code_verifier = None;
            true
        } else {
            false
        }
    }

    /// Returns whether an auth redirect that failed state validation should be
    /// silently dropped rather than surfaced as an error. This covers the
    /// "user clicks the browser's 'Take me to Mnemos' button twice" case: once
    /// they're fully logged in, a second redirect targeting the same user is
    /// redundant and should not produce a user-visible error.
    fn should_silently_ignore_stale_redirect(&self, incoming_user_uid: &Option<UserUid>) -> bool {
        if self.auth_state.is_anonymous_or_logged_out() {
            return false;
        }
        match (self.auth_state.user_id(), incoming_user_uid) {
            (Some(current_uid), Some(incoming_uid)) => current_uid == *incoming_uid,
            _ => false,
        }
    }

    /// Sets the user as onboarded both on the server and locally.
    /// This method:
    /// 1. Updates the server by calling set_user_is_onboarded
    /// 2. Updates the local auth state and persists the user data
    pub fn set_user_onboarded(&self, ctx: &mut ModelContext<Self>) {
        // Update server
        let auth_client = self.auth_client.clone();
        let _ = ctx.spawn(
            async move { auth_client.set_user_is_onboarded().await },
            |_, _, _| {},
        );

        // Update local auth state and persist
        self.auth_state.set_is_onboarded(true);

        self.persist(ctx);
    }
}

#[derive(Clone, Debug)]
pub struct PersistedCurrentUserInformation {
    pub email: String,
}

impl Entity for AuthManager {
    type Event = AuthManagerEvent;
}

impl SingletonEntity for AuthManager {}

/// Decode the unverified payload of an admin JWT (`sub`, `email`) into a
/// minimally-populated `User`. Signature is not verified — the token has
/// already round-tripped through the admin backend's redirect, so this is
/// purely a parse step to surface email/uid in Mnemos's auth state.
fn build_user_from_admin_jwt(jwt: &str) -> Result<User> {
    use super::user::{PrincipalType, UserMetadata};
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};

    let payload_b64 = jwt
        .split('.')
        .nth(1)
        .ok_or_else(|| anyhow!("JWT missing payload segment"))?;
    let payload_bytes = URL_SAFE_NO_PAD
        .decode(payload_b64)
        .map_err(|e| anyhow!("JWT payload base64 decode failed: {e}"))?;
    let claims: serde_json::Value = serde_json::from_slice(&payload_bytes)
        .map_err(|e| anyhow!("JWT payload JSON decode failed: {e}"))?;

    let sub = claims
        .get("sub")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("JWT missing `sub` claim"))?;
    let email = claims
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    Ok(User {
        local_id: UserUid::new(sub),
        metadata: UserMetadata {
            email,
            display_name: None,
            photo_url: None,
        },
        is_onboarded: true,
        needs_sso_link: false,
        anonymous_user_type: None,
        is_on_work_domain: false,
        linked_at: None,
        personal_object_limits: None,
        principal_type: PrincipalType::User,
    })
}

/// Persist the admin-platform JWT pair from the sign-in to mnemos-ide-sync's
/// keyring entry, and return it for the sync run this login starts, with why
/// the keyring refused it if it did. The login is bound to the server the app
/// sent the user to sign in at (`ChannelState::server_root_url`, the same root
/// `remote_sign_up_url` uses), never to anything the redirect carries, so
/// refreshes and sync requests only ever go back to the issuer. The expiry
/// comes with `tokens`: the server's window for a PKCE code exchange, one hour
/// from now for a legacy redirect (the next refresh replaces it).
///
/// A failed save never blocks login: sync runs on the returned pair, and the
/// UI says the login was not saved (the next start would be signed out).
fn persist_admin_tokens(
    tokens: mnemos_ide_sync::auth::AdminTokens,
) -> (mnemos_ide_sync::auth::AdminAuth, Option<String>) {
    use mnemos_ide_sync::auth::AdminAuth;

    let auth = AdminAuth::from_tokens(ChannelState::server_root_url(), tokens);
    // Same reason as `log_out`: tests must never overwrite the real login.
    if cfg!(test) {
        return (auth, None);
    }
    match auth.save_to_keyring() {
        Ok(()) => {
            log::info!("Persisted admin tokens to keyring");
            (auth, None)
        }
        Err(err) => {
            log::warn!("Failed to persist admin tokens to keyring: {err}");
            (auth, Some(err.to_string()))
        }
    }
}

#[cfg(test)]
#[path = "auth_manager_test.rs"]
mod auth_manager_test;

/// Mnemos IDE: where a deletion requested from Settings > Sync stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SyncDeletionStatus {
    InProgress { account: bool },
    /// Deleted on the server; this machine is signed out of sync.
    Done { account: bool },
    Failed(String),
}

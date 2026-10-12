//! Mnemos IDE: what the UI says about mnemos-ide-sync's admin login.
//!
//! Sync used to stop silently: once the server expired the refresh token,
//! every push failed in the log while the app looked healthy. The workspace
//! header chip and the Settings > Sync row now read
//! [`AuthManager::ide_sync_auth`](super::AuthManager::ide_sync_auth), which is
//! built here from two facts: whether a manager was started at all, and what
//! the running one reports. Kept as pure functions so the mapping is tested
//! without an app, a manager or a keyring.

use mnemos_ide_sync::SyncAuthState;

/// The login state the UI shows. `not_started` is why startup could not use a
/// saved login (an unreadable keyring, a login without tokens); `running` is
/// what the current manager reports, if there is one.
///
/// No manager and no reason means nobody signed in here, or the user signed
/// out: [`SyncAuthState::NeverConfigured`]. Sync is optional and the app works
/// without an account, so a user who never signed in sees no warning.
pub(crate) fn resolve(not_started: Option<&str>, running: Option<SyncAuthState>) -> SyncAuthState {
    match (running, not_started) {
        // A manager knows best, including a refusal that came after it
        // started.
        (Some(state), _) => state,
        (None, Some(reason)) => SyncAuthState::SignedOut {
            reason: reason.to_owned(),
        },
        (None, None) => SyncAuthState::NeverConfigured,
    }
}

/// Why startup found no admin login in the keyring, if that is worth a warning.
/// `admin_login_seen` is the non-secret marker a successful admin sign-in sets
/// and a deliberate sign-out clears: with it set, a login existed here and the
/// keyring lost it (or never saved it), so the user gets a "Sync signed out"
/// chip. Without it nobody signed in on this machine, which is
/// [`SyncAuthState::NeverConfigured`] and shows nothing.
pub(crate) fn missing_login_reason(admin_login_seen: bool) -> Option<String> {
    admin_login_seen.then(|| "The saved admin login is missing from the keyring".to_owned())
}

/// What the header chip and the settings row say while the login needs
/// attention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SyncAuthNotice {
    /// The chip's text. Short: the tab bar has little room.
    pub label: &'static str,
    /// What happened and why, for the tooltip and the settings row.
    pub detail: String,
    /// What clicking the chip does.
    pub action_hint: &'static str,
    /// The action behind the chip and the settings row's button.
    pub action: SyncAuthNoticeAction,
}

/// What the chip's click and the settings row's button do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncAuthNoticeAction {
    /// Open the admin sign-in.
    SignIn,
    /// Check for a Mnemos update: the sync server needs a newer build.
    CheckForUpdate,
}

/// `None` while signed in, or when sync was never set up: the chip is not
/// shown at all then.
pub(crate) fn notice(state: &SyncAuthState) -> Option<SyncAuthNotice> {
    match state {
        SyncAuthState::SignedIn | SyncAuthState::NeverConfigured => None,
        SyncAuthState::SignedOut { reason } => Some(SyncAuthNotice {
            label: "Sync signed out",
            detail: format!(
                "Claude config and session sync is stopped: {}",
                ascii_only(reason)
            ),
            action_hint: "Click to sign in to the admin platform again",
            action: SyncAuthNoticeAction::SignIn,
        }),
        SyncAuthState::TokenNotSaved { reason } => Some(SyncAuthNotice {
            label: "Sync login not saved",
            detail: format!(
                "Sync runs, but the login could not be saved to the keyring, so the next start \
                 will be signed out: {}",
                ascii_only(reason)
            ),
            action_hint: "Click to sign in again",
            action: SyncAuthNoticeAction::SignIn,
        }),
        SyncAuthState::UpdateRequired { reason } => Some(SyncAuthNotice {
            label: "Update Mnemos to sync",
            detail: format!(
                "Claude config and session sync is stopped: {}",
                ascii_only(reason)
            ),
            action_hint: "Click to check for a Mnemos update",
            action: SyncAuthNoticeAction::CheckForUpdate,
        }),
    }
}

/// The reason comes from the server or an OS keyring error and may carry
/// anything; the UI font has no glyphs for many symbols on Windows.
pub(crate) fn ascii_only(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii() && !c.is_ascii_control() {
                c
            } else {
                '?'
            }
        })
        .collect()
}

/// Whether the first run of a build with the live-feed setting pins the feed
/// on: only for an install that already has an admin login, and only while
/// the user has not chosen either way.
pub(crate) fn live_feed_pin_needed(has_login: bool, explicitly_set: bool) -> bool {
    has_login && !explicitly_set
}

/// Why Settings > Sync offers no deletion for a staff account.
pub(crate) const STAFF_ACCOUNT_DELETION_REFUSED: &str =
    "This is a staff account of the sync server's admin platform. The server never deletes \
     its data on request (it can hold company data); an operator deletes it instead. Sync \
     continues.";

/// What Settings > Sync offers for deleting synced data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeletionOffer {
    /// Nothing to delete from here: not signed in to sync.
    None,
    /// The delete buttons (sync runs, or a failed deletion can be retried).
    Offered,
    /// The server refuses this account: explain, offer nothing.
    RefusedForAccount,
}

/// See [`DeletionOffer`]. `deletable` is the server's `canDeleteData`, `None`
/// while unknown (offered, and a refusal is explained when it comes). A
/// retry of a deletion already under way is always offered: this machine
/// signed out for it.
pub(crate) fn deletion_offer(
    running: bool,
    retry_pending: bool,
    deletable: Option<bool>,
) -> DeletionOffer {
    if retry_pending {
        return DeletionOffer::Offered;
    }
    match (running, deletable) {
        (false, _) => DeletionOffer::None,
        (true, Some(false)) => DeletionOffer::RefusedForAccount,
        (true, _) => DeletionOffer::Offered,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staff_accounts_are_not_offered_a_deletion() {
        assert_eq!(deletion_offer(true, false, Some(false)), DeletionOffer::RefusedForAccount);
        assert_eq!(deletion_offer(true, false, Some(true)), DeletionOffer::Offered);
        // Unknown (an older server): offered, the 403 is explained.
        assert_eq!(deletion_offer(true, false, None), DeletionOffer::Offered);
        assert_eq!(deletion_offer(false, false, None), DeletionOffer::None);
        // A retry stays offered: this machine already signed out for it.
        assert_eq!(deletion_offer(false, true, Some(false)), DeletionOffer::Offered);
        assert!(STAFF_ACCOUNT_DELETION_REFUSED.is_ascii());
    }

    #[test]
    fn live_feed_is_pinned_only_for_existing_logins() {
        assert!(live_feed_pin_needed(true, false));
        assert!(!live_feed_pin_needed(false, false), "a new install stays off");
        assert!(!live_feed_pin_needed(true, true), "an explicit choice is kept");
    }

    #[test]
    fn a_running_manager_decides() {
        let refused = SyncAuthState::SignedOut {
            reason: "Session not found or expired".into(),
        };
        assert_eq!(
            resolve(None, Some(SyncAuthState::SignedIn)),
            SyncAuthState::SignedIn
        );
        assert_eq!(resolve(None, Some(refused.clone())), refused);
        let unsaved = SyncAuthState::TokenNotSaved {
            reason: "Access is denied.".into(),
        };
        assert_eq!(resolve(None, Some(unsaved.clone())), unsaved);
    }

    #[test]
    fn no_manager_is_signed_out_with_the_start_reason() {
        assert_eq!(
            resolve(Some("Could not read the saved admin login: denied"), None),
            SyncAuthState::SignedOut {
                reason: "Could not read the saved admin login: denied".into()
            }
        );
    }

    #[test]
    fn a_missing_login_warns_only_when_one_was_seen() {
        assert_eq!(missing_login_reason(false), None);
        let reason = missing_login_reason(true).expect("a lost login is a warning");
        assert!(matches!(
            resolve(Some(&reason), None),
            SyncAuthState::SignedOut { .. }
        ));
    }

    #[test]
    fn no_manager_and_no_reason_is_never_configured() {
        // Never signed in, logged out, or a test app that never started sync.
        assert_eq!(resolve(None, None), SyncAuthState::NeverConfigured);
    }

    #[test]
    fn the_chip_shows_only_when_the_login_needs_attention() {
        assert_eq!(notice(&SyncAuthState::SignedIn), None);
        assert_eq!(notice(&SyncAuthState::NeverConfigured), None);

        let out = notice(&SyncAuthState::SignedOut {
            reason: "Session not found or expired".into(),
        })
        .unwrap();
        assert_eq!(out.label, "Sync signed out");
        assert!(
            out.detail.ends_with("Session not found or expired"),
            "{}",
            out.detail
        );

        let unsaved = notice(&SyncAuthState::TokenNotSaved {
            reason: "Access is denied.".into(),
        })
        .unwrap();
        assert_eq!(unsaved.label, "Sync login not saved");
        assert!(
            unsaved.detail.contains("Access is denied."),
            "{}",
            unsaved.detail
        );
        assert_eq!(unsaved.action, SyncAuthNoticeAction::SignIn);

        let update = notice(&SyncAuthState::UpdateRequired {
            reason: "Update Mnemos to keep syncing.".into(),
        })
        .unwrap();
        assert_eq!(update.label, "Update Mnemos to sync");
        assert_eq!(update.action, SyncAuthNoticeAction::CheckForUpdate);
        assert!(update.detail.contains("Update Mnemos"), "{}", update.detail);
    }

    #[test]
    fn every_string_the_chip_renders_is_ascii() {
        let states = [
            SyncAuthState::SignedOut {
                reason: "Sitzung abgelaufen \u{2014} \u{25cf}\n".into(),
            },
            SyncAuthState::TokenNotSaved {
                reason: "\u{fffd} denied".into(),
            },
            SyncAuthState::UpdateRequired {
                reason: "Bitte aktualisieren \u{2014}".into(),
            },
        ];
        for state in &states {
            let n = notice(state).unwrap();
            for text in [n.label, n.detail.as_str(), n.action_hint] {
                assert!(
                    text.chars().all(|c| c.is_ascii() && !c.is_ascii_control()),
                    "{text:?}"
                );
            }
        }
    }
}

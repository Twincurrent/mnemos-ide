//! Mnemos IDE: where this machine keeps the end-to-end encryption recovery
//! keys of mnemos-ide-sync, by admin profile id.
//!
//! Stored through upstream secure storage (the login keychain on macOS,
//! DPAPI-encrypted files in the state dir on Windows), as AGENTS.md asks for
//! a new secret; never in the admin-login keyring entry (`tcbv2*`), which
//! older builds rewrite without fields they do not know. Never synced and
//! never sent anywhere: the server only sees what the key seals.

use std::collections::HashMap;
use std::sync::Mutex;

use mnemos_ui::AppContext;
use mnemos_ui_extras::secure_storage::AppContextExt as _;
use serde::{Deserialize, Serialize};

const STORAGE_KEY: &str = "MnemosSyncRecoveryKeys";

#[derive(Debug, Default, Serialize, Deserialize)]
struct Stored {
    /// Admin profile id -> recovery key text.
    #[serde(default)]
    keys: HashMap<String, String>,
}

/// The keys as last read or written in this process. The macOS keychain
/// reports every failed lookup (a locked keychain, a denied prompt) as
/// "not found", so a "not found" after keys were seen is not trusted to
/// mean "none saved": those keys are kept in what is written back.
static LAST_KNOWN: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

fn remember(stored: &Stored) {
    *LAST_KNOWN.lock().unwrap_or_else(|e| e.into_inner()) = Some(stored.keys.clone());
}

/// What is saved. `Ok(empty)` only when nothing is saved yet; a store that
/// cannot be read or holds something unparseable is an error, so a write
/// never replaces other profiles' keys with an empty map.
fn read(ctx: &AppContext) -> Result<Stored, String> {
    let raw = ctx.secure_storage().read_value(STORAGE_KEY);
    let known = LAST_KNOWN.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let stored = interpret(raw, known.as_ref())?;
    remember(&stored);
    Ok(stored)
}

/// [`read`] without the storage: what a read result means, given the keys
/// last known in this process.
fn interpret(
    raw: Result<String, mnemos_ui_extras::secure_storage::Error>,
    known: Option<&HashMap<String, String>>,
) -> Result<Stored, String> {
    match raw {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| format!("the saved recovery keys could not be read ({e})")),
        Err(mnemos_ui_extras::secure_storage::Error::NotFound) => {
            let keys = known.cloned().unwrap_or_default();
            if !keys.is_empty() {
                log::warn!(
                    "mnemos-ide-sync: saved recovery keys reported missing after they were read; keeping the ones known"
                );
            }
            Ok(Stored { keys })
        }
        Err(e) => Err(format!("secure storage could not be read ({e:?})")),
    }
}

/// Add `recovery_text` for `profile_id` unless a key is saved for it.
fn insert_new(stored: &mut Stored, profile_id: &str, recovery_text: &str) -> Result<(), String> {
    if stored.keys.contains_key(profile_id) {
        return Err("a recovery key for this account is already saved here".to_owned());
    }
    stored
        .keys
        .insert(profile_id.to_owned(), recovery_text.to_owned());
    Ok(())
}

/// Remove the key of `profile_id` if it is still `recovery_text`.
fn remove_matching(stored: &mut Stored, profile_id: &str, recovery_text: &str) -> bool {
    if stored.keys.get(profile_id).map(String::as_str) != Some(recovery_text) {
        return false;
    }
    stored.keys.remove(profile_id);
    true
}

fn write(ctx: &AppContext, stored: &Stored) -> Result<(), String> {
    let text = serde_json::to_string(stored).map_err(|e| e.to_string())?;
    ctx.secure_storage()
        .write_value(STORAGE_KEY, &text)
        .map_err(|e| format!("{e:?}"))?;
    remember(stored);
    Ok(())
}

/// Every recovery key saved on this machine, by admin profile id. Empty
/// (and logged) when they cannot be read: the run then has no key, which
/// with the account's marker present locks personal sync, never uploads
/// plaintext.
pub(crate) fn load(ctx: &AppContext) -> HashMap<String, String> {
    match read(ctx) {
        Ok(stored) => stored.keys,
        Err(e) => {
            log::warn!("mnemos-ide-sync: {e}");
            HashMap::new()
        }
    }
}

/// The recovery key saved for `profile_id`, if any.
pub(crate) fn get(ctx: &AppContext, profile_id: &str) -> Result<Option<String>, String> {
    Ok(read(ctx)?.keys.remove(profile_id))
}

/// Save `recovery_text` as the key of `profile_id`, replacing any other.
/// For a key the server has just confirmed (an unlock). Fails without
/// writing when the saved keys cannot be read.
pub(crate) fn save(ctx: &AppContext, profile_id: &str, recovery_text: &str) -> Result<(), String> {
    let mut stored = read(ctx)?;
    stored
        .keys
        .insert(profile_id.to_owned(), recovery_text.to_owned());
    write(ctx, &stored)
}

/// Save `recovery_text` as the key of `profile_id` only when none is saved
/// for it yet (turning encryption on never replaces a key in use).
pub(crate) fn save_new(
    ctx: &AppContext,
    profile_id: &str,
    recovery_text: &str,
) -> Result<(), String> {
    let mut stored = read(ctx)?;
    insert_new(&mut stored, profile_id, recovery_text)?;
    write(ctx, &stored)
}

/// Forget the key of `profile_id`, but only while it is still
/// `recovery_text` (a turn-on that did not go through removes the key it
/// saved, never one saved since).
pub(crate) fn remove_if(ctx: &AppContext, profile_id: &str, recovery_text: &str) {
    let mut stored = match read(ctx) {
        Ok(stored) => stored,
        Err(e) => {
            log::warn!("mnemos-ide-sync: not removing a recovery key: {e}");
            return;
        }
    };
    if !remove_matching(&mut stored, profile_id, recovery_text) {
        return;
    }
    if let Err(e) = write(ctx, &stored) {
        log::warn!("mnemos-ide-sync: could not update saved recovery keys: {e}");
    }
}

/// Forget the key of `profile_id`, whatever it is: the account's encrypted
/// data is gone (a deletion from here, or "start over" after the server lost
/// its marker). Fails without writing when the saved keys cannot be read, so
/// other profiles' keys are never dropped.
pub(crate) fn remove(ctx: &AppContext, profile_id: &str) -> Result<(), String> {
    let mut stored = read(ctx)?;
    if stored.keys.remove(profile_id).is_none() {
        return Ok(());
    }
    write(ctx, &stored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mnemos_ui_extras::secure_storage::Error;

    fn keys(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn a_failed_read_is_an_error_not_an_empty_store() {
        let raw = Err(Error::Unknown(anyhow::anyhow!("keychain locked")));
        assert!(interpret(raw, None).is_err());
        assert!(interpret(Ok("not json".to_owned()), None).is_err());
    }

    #[test]
    fn not_found_is_empty_unless_keys_were_seen() {
        assert!(interpret(Err(Error::NotFound), None)
            .unwrap()
            .keys
            .is_empty());
        // macOS reports a denied lookup as not found: the keys this process
        // already read are kept, so a save cannot drop other profiles'.
        let known = keys(&[("other", "KEY-B")]);
        let mut stored = interpret(Err(Error::NotFound), Some(&known)).unwrap();
        insert_new(&mut stored, "me", "KEY-A").unwrap();
        assert_eq!(stored.keys, keys(&[("other", "KEY-B"), ("me", "KEY-A")]));
    }

    #[test]
    fn turning_on_never_replaces_a_saved_key() {
        let mut stored = Stored {
            keys: keys(&[("me", "IN-USE")]),
        };
        assert!(insert_new(&mut stored, "me", "NEW").is_err());
        assert_eq!(stored.keys["me"], "IN-USE");
    }

    #[test]
    fn only_the_key_a_turn_on_saved_is_removed() {
        let mut stored = Stored {
            keys: keys(&[("me", "IN-USE"), ("other", "KEY-B")]),
        };
        assert!(!remove_matching(&mut stored, "me", "NEW"));
        assert_eq!(stored.keys["me"], "IN-USE");
        assert!(remove_matching(&mut stored, "me", "IN-USE"));
        assert_eq!(stored.keys, keys(&[("other", "KEY-B")]));
    }
}

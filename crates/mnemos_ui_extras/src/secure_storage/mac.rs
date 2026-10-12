//! Implementations of the [`SecureStorage`] service for the macOS platform.

use anyhow::anyhow;
use security_framework::os::macos::{
    keychain::SecKeychain, keychain_item::SecKeychainItem, passwords::SecKeychainItemPassword,
};

use super::Error;

/// Implementation of the SecureStorage service using macOS Security
/// framework keychains.
pub struct SecureStorage {
    /// The name of the service under which to store the values.
    service_name: String,
}

impl SecureStorage {
    pub fn new(service_name: &str) -> Self {
        Self {
            service_name: service_name.to_owned(),
        }
    }
}

impl super::SecureStorage for SecureStorage {
    fn write_value(&self, key: &str, value: &str) -> Result<(), Error> {
        let keychain = SecKeychain::default()?;
        keychain
            .set_generic_password(self.service_name.as_str(), key, value.as_bytes())
            .map_err(Into::into)
    }

    fn read_value(&self, key: &str) -> Result<String, Error> {
        let (password, _) = self.get_password_item(key)?;
        String::from_utf8(password.as_ref().to_vec())
            .map_err(|err| Error::DecodeError(err.utf8_error()))
    }

    fn remove_value(&self, key: &str) -> Result<(), Error> {
        let (_, item) = self.get_password_item(key)?;
        item.delete();
        Ok(())
    }
}

impl SecureStorage {
    fn get_password_item(
        &self,
        key: &str,
    ) -> Result<(SecKeychainItemPassword, SecKeychainItem), Error> {
        let keychain = SecKeychain::default()?;
        keychain
            .find_generic_password(&self.service_name, key)
            .map_err(|err| lookup_error(err.code()))
    }
}

/// `errSecItemNotFound` from `SecBase.h`.
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

/// Mnemos IDE: only a lookup that found no item is `NotFound`. Every failure
/// used to be, so a locked keychain or a denied access prompt read as "no
/// value saved", and a caller that then wrote back what it thought was the
/// whole value (the sync recovery keys, kept by profile) dropped the rest.
/// Callers that only care whether a value is there treat any error the same.
fn lookup_error(code: i32) -> Error {
    if code == ERR_SEC_ITEM_NOT_FOUND {
        Error::NotFound
    } else {
        Error::Unknown(anyhow!("keychain lookup failed (OSStatus {code})"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_missing_item_is_not_found() {
        assert!(matches!(lookup_error(ERR_SEC_ITEM_NOT_FOUND), Error::NotFound));
        // errSecUserCanceled, errSecAuthFailed, errSecInteractionNotAllowed.
        for code in [-128, -25293, -25308] {
            assert!(matches!(lookup_error(code), Error::Unknown(_)), "{code}");
        }
    }
}

impl From<security_framework::base::Error> for Error {
    fn from(value: security_framework::base::Error) -> Self {
        Error::Unknown(anyhow!(value))
    }
}

//! Dedicated iDoris credential namespace; no plaintext fallback.

use zeroize::Zeroizing;

pub const SERVICE: &str = "ai.auraai.agent24.idoris";

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum KeychainError {
    #[error("unlock the keychain or allow Agent24 access")]
    KeychainLocked,
    #[error("iDoris keychain is unsupported on this platform")]
    Unsupported,
    #[error("iDoris keychain entry was not found")]
    NotFound,
    #[error("iDoris keychain operation failed")]
    Unavailable,
}

/// Implementations must not include credential values in errors or diagnostics.
pub trait Keychain: Send + Sync {
    fn put(&self, key_id: &str, secret: &str) -> Result<(), KeychainError>;
    fn get(&self, key_id: &str) -> Result<Zeroizing<String>, KeychainError>;
    fn delete(&self, key_id: &str) -> Result<(), KeychainError>;
}

pub struct SystemKeychain;

#[cfg(target_os = "macos")]
fn map_error(error: keyring::Error) -> KeychainError {
    match error {
        keyring::Error::NoEntry => KeychainError::NotFound,
        keyring::Error::NoStorageAccess(_) | keyring::Error::PlatformFailure(_) => {
            KeychainError::KeychainLocked
        }
        _ => KeychainError::Unavailable,
    }
}

#[cfg(target_os = "macos")]
impl Keychain for SystemKeychain {
    fn put(&self, key_id: &str, secret: &str) -> Result<(), KeychainError> {
        keyring::Entry::new(SERVICE, key_id)
            .and_then(|entry| entry.set_password(secret))
            .map_err(map_error)
    }

    fn get(&self, key_id: &str) -> Result<Zeroizing<String>, KeychainError> {
        keyring::Entry::new(SERVICE, key_id)
            .and_then(|entry| entry.get_password())
            .map(Zeroizing::new)
            .map_err(map_error)
    }

    fn delete(&self, key_id: &str) -> Result<(), KeychainError> {
        keyring::Entry::new(SERVICE, key_id)
            .and_then(|entry| entry.delete_credential())
            .map_err(map_error)
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn access_errors_are_locked_and_never_expose_backend_details() {
        for error in [
            keyring::Error::NoStorageAccess(Box::new(std::io::Error::other("idk_sensitive"))),
            keyring::Error::PlatformFailure(Box::new(std::io::Error::other("idk_sensitive"))),
        ] {
            let error = map_error(error);
            assert_eq!(error, KeychainError::KeychainLocked);
            assert!(!format!("{error:?} {error}").contains("idk_"));
        }
        assert_eq!(map_error(keyring::Error::NoEntry), KeychainError::NotFound);
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn unsupported_platform_never_uses_plaintext() {
        assert_eq!(
            SystemKeychain.put("key", "secret"),
            Err(KeychainError::Unsupported)
        );
        assert_eq!(SystemKeychain.get("key"), Err(KeychainError::Unsupported));
        assert_eq!(
            SystemKeychain.delete("key"),
            Err(KeychainError::Unsupported)
        );
    }
}

#[cfg(not(target_os = "macos"))]
impl Keychain for SystemKeychain {
    fn put(&self, _key_id: &str, _secret: &str) -> Result<(), KeychainError> {
        Err(KeychainError::Unsupported)
    }

    fn get(&self, _key_id: &str) -> Result<Zeroizing<String>, KeychainError> {
        Err(KeychainError::Unsupported)
    }

    fn delete(&self, _key_id: &str) -> Result<(), KeychainError> {
        Err(KeychainError::Unsupported)
    }
}

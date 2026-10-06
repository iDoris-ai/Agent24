//! The keystore password, held only long enough to be written to Hyphae's
//! stdin.
//!
//! [`Password`] never implements [`std::fmt::Debug`] with its contents
//! visible, and its backing storage is zeroed on drop. It is never logged,
//! never placed in argv, and the only consumer (`runner::HyphaeRunner`)
//! writes its bytes straight to a child's stdin pipe and closes it
//! immediately.

use base64::Engine;
use zeroize::{Zeroize, Zeroizing};

use crate::runner::RunnerError;

/// Matches Hyphae's own `maxPasswordStdinBytes` limit.
pub const PASSWORD_MAX: usize = 4096;

pub struct Password(Zeroizing<Vec<u8>>);

impl Password {
    /// Accepts `1..=PASSWORD_MAX` bytes. Hyphae itself requires a non-empty
    /// password, so the empty case is rejected here too rather than being
    /// passed through only to fail downstream with a less specific error.
    pub fn new(mut bytes: Vec<u8>) -> Result<Self, RunnerError> {
        if bytes.is_empty() || bytes.len() > PASSWORD_MAX {
            let len = bytes.len();
            bytes.zeroize();
            return Err(RunnerError::PasswordLength(len));
        }
        Ok(Self(Zeroizing::new(bytes)))
    }

    /// Makes a short-lived, zero-on-drop copy for a subprocess while the
    /// original remains available for a subsequent verified registration.
    pub(crate) fn duplicate(&self) -> Self {
        Self(Zeroizing::new(self.0.to_vec()))
    }

    /// A freshly generated password: `random32` base64url-(no padding)
    /// encoded, 43 ASCII characters — always well inside [`PASSWORD_MAX`]
    /// (COMM-HYPHAE.md §3: "自动生成的口令本身就是" base64url text). Used
    /// for the very first identity a keystore gets, before anyone has typed
    /// a password of their own (§6.4) — the caller supplies the random
    /// bytes so this stays a pure function, with the entropy source
    /// (`rand::rng().fill_bytes`, COMM-2a's `router.rs`) kept out of this
    /// crate's one place for zero-on-drop password storage.
    pub fn generate(random32: [u8; 32]) -> Self {
        let text = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random32);
        Self(Zeroizing::new(text.into_bytes()))
    }

    /// Only visible within the crate: the runner writes this straight to a
    /// child's stdin and nowhere else.
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for Password {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Password")
            .field("len", &self.0.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn rejects_empty_password() {
        assert!(matches!(
            Password::new(Vec::new()),
            Err(RunnerError::PasswordLength(0))
        ));
    }

    #[test]
    fn accepts_max_length() {
        let bytes = vec![b'x'; PASSWORD_MAX];
        assert!(Password::new(bytes).is_ok());
    }

    #[test]
    fn rejects_one_byte_over_max() {
        let bytes = vec![b'x'; PASSWORD_MAX + 1];
        assert!(matches!(
            Password::new(bytes),
            Err(RunnerError::PasswordLength(n)) if n == PASSWORD_MAX + 1
        ));
    }

    #[test]
    fn generate_is_43_char_base64url_text_with_no_padding() {
        let password = Password::generate([7u8; 32]);
        let text = std::str::from_utf8(password.as_bytes()).unwrap();
        assert_eq!(text.len(), 43);
        assert!(!text.contains('+'));
        assert!(!text.contains('/'));
        assert!(!text.contains('='));
    }

    #[test]
    fn generate_is_deterministic_in_its_input() {
        // Pure function: same bytes in, same password out — the actual
        // randomness lives in the caller's `random32` argument, not here.
        let a = Password::generate([1u8; 32]);
        let b = Password::generate([1u8; 32]);
        assert_eq!(a.as_bytes(), b.as_bytes());
    }

    #[test]
    fn debug_never_contains_the_plaintext() {
        let secret = "super-secret-token-value";
        let password = Password::new(secret.as_bytes().to_vec()).unwrap();
        let debug = format!("{password:?}");
        assert!(!debug.contains(secret));
        assert!(debug.contains("len"));
    }
}

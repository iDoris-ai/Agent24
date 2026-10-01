//! The keystore password, held only long enough to be written to Hyphae's
//! stdin.
//!
//! [`Password`] never implements [`std::fmt::Debug`] with its contents
//! visible, and its backing storage is zeroed on drop. It is never logged,
//! never placed in argv, and the only consumer (`runner::HyphaeRunner`)
//! writes its bytes straight to a child's stdin pipe and closes it
//! immediately.

use zeroize::Zeroizing;

use crate::runner::RunnerError;

/// Matches Hyphae's own `maxPasswordStdinBytes` limit.
pub const PASSWORD_MAX: usize = 4096;

pub struct Password(Zeroizing<Vec<u8>>);

impl Password {
    /// Accepts `1..=PASSWORD_MAX` bytes. Hyphae itself requires a non-empty
    /// password, so the empty case is rejected here too rather than being
    /// passed through only to fail downstream with a less specific error.
    pub fn new(bytes: Vec<u8>) -> Result<Self, RunnerError> {
        if bytes.is_empty() || bytes.len() > PASSWORD_MAX {
            return Err(RunnerError::PasswordLength(bytes.len()));
        }
        Ok(Self(Zeroizing::new(bytes)))
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
    fn debug_never_contains_the_plaintext() {
        let secret = "super-secret-token-value";
        let password = Password::new(secret.as_bytes().to_vec()).unwrap();
        let debug = format!("{password:?}");
        assert!(!debug.contains(secret));
        assert!(debug.contains("len"));
    }
}

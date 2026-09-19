//! Private process ownership for the sidecar host.
//!
//! The host owns one [`GenerationOwner`] for each launched generation. On
//! Windows, the owner is a `processkit::ProcessGroup`: its spawn path creates
//! the child suspended, assigns it to a Job Object, and resumes it. Dropping
//! the owner closes the Job Object handle, whose kill-on-close limit tears down
//! the whole owned tree.

#[cfg(windows)]
mod owner;

#[cfg(windows)]
pub use owner::{GenerationId, GenerationOwner, OwnedProcess};

#[cfg(not(windows))]
mod owner {
    use std::io;

    /// A generation identity is non-zero and never inferred from a PID.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct GenerationId(u64);

    impl GenerationId {
        /// Construct a generation identity, rejecting the sentinel zero.
        pub fn new(value: u64) -> io::Result<Self> {
            (value != 0)
                .then_some(Self(value))
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "generation is zero"))
        }
    }

    /// Windows-only process ownership is intentionally unavailable elsewhere.
    #[derive(Debug)]
    pub struct GenerationOwner;

    impl GenerationOwner {
        /// Report that this foundation has no non-Windows process backend.
        pub fn new(_: GenerationId) -> io::Result<Self> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "sidecar process ownership is only implemented on Windows",
            ))
        }
    }

    #[cfg(test)]
    #[allow(clippy::expect_used)]
    mod tests {
        use super::*;

        #[test]
        fn non_windows_backend_does_not_fallback_to_unowned_spawn() {
            let generation = GenerationId::new(1).expect("non-zero generation");
            assert_eq!(
                GenerationOwner::new(generation)
                    .expect_err("backend must remain Windows-only")
                    .kind(),
                io::ErrorKind::Unsupported
            );
        }
    }
}

#[cfg(not(windows))]
pub use owner::{GenerationId, GenerationOwner};

use std::time::Duration;

use crate::WorkspaceError;

/// Caller-controlled scratch policy only; identity and root authority are service-owned.
pub struct ScratchCreateSpec {
    ttl: Duration,
}

impl ScratchCreateSpec {
    pub fn new(ttl: Duration) -> Result<Self, WorkspaceError> {
        if ttl.is_zero() {
            return Err(WorkspaceError::InvalidSpec { field: "ttl" });
        }
        Ok(Self { ttl })
    }

    pub const fn ttl(&self) -> Duration {
        self.ttl
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn scratch_spec_accepts_policy_without_caller_identity_or_root() {
        let spec = ScratchCreateSpec::new(Duration::from_secs(60)).unwrap();
        assert_eq!(spec.ttl(), Duration::from_secs(60));
        assert_eq!(
            ScratchCreateSpec::new(Duration::ZERO).err(),
            Some(WorkspaceError::InvalidSpec { field: "ttl" })
        );
    }
}

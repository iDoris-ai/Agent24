//! Store-owned value types for workspace allocation journals.

/// Static validation failures for allocation values; rejected input is never retained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllocationValueError {
    InvalidId,
    InvalidFailureReason,
    InvalidRootGeneration,
    InvalidRelativeName,
}

impl std::fmt::Display for AllocationValueError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidId => "invalid allocation id",
            Self::InvalidFailureReason => "invalid allocation failure reason",
            Self::InvalidRootGeneration => "invalid allocation root generation",
            Self::InvalidRelativeName => "invalid allocation relative name",
        })
    }
}

impl std::error::Error for AllocationValueError {}

/// Opaque allocation journal identifier (`wa_` plus a canonical ULID).
#[derive(Clone, PartialEq, Eq)]
pub struct AllocationId(String);

impl AllocationId {
    /// Parse a canonical allocation identifier.
    pub fn parse(value: &str) -> Result<Self, AllocationValueError> {
        let bytes = value.as_bytes();
        if bytes.len() != 29
            || !bytes.starts_with(b"wa_")
            || !(b'0'..=b'7').contains(&bytes[3])
            || !bytes[4..]
                .iter()
                .all(|byte| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(byte))
        {
            return Err(AllocationValueError::InvalidId);
        }
        Ok(Self(value.to_owned()))
    }
}

/// Closed allocation journal lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllocationPhase {
    Reserved,
    Materialized,
    Committed,
    Retained,
}

/// Opaque, bounded allocation failure reason.
#[derive(Clone, PartialEq, Eq)]
pub struct AllocationFailureReason(String);

impl AllocationFailureReason {
    /// Accept only 1–128 ASCII lowercase letters, digits, `_`, and `-`.
    pub fn parse(value: &str) -> Result<Self, AllocationValueError> {
        let bytes = value.as_bytes();
        if bytes.is_empty()
            || bytes.len() > 128
            || !bytes.iter().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_' || *byte == b'-'
            })
        {
            return Err(AllocationValueError::InvalidFailureReason);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn allocation_id_keeps_canonical_ulid_boundaries() {
        for value in [
            format!("wa_0{}", "0".repeat(25)),
            format!("wa_7{}", "Z".repeat(25)),
        ] {
            assert!(AllocationId::parse(&value).is_ok());
        }
        for value in [
            "wx_00000000000000000000000000",
            "wa_0000000000000000000000000",
            "wa_000000000000000000000000000",
            "wa_0000000000000000000000000a",
            "wa_80000000000000000000000000",
            "wa_90000000000000000000000000",
            "wa_I0000000000000000000000000",
            "wa_L0000000000000000000000000",
            "wa_O0000000000000000000000000",
            "wa_U0000000000000000000000000",
        ] {
            assert!(matches!(
                AllocationId::parse(value),
                Err(AllocationValueError::InvalidId)
            ));
        }
        assert!(AllocationId::parse(&format!("wa_0{}\0", "0".repeat(24))).is_err());
    }

    #[test]
    fn allocation_phase_has_only_the_four_supported_states() {
        let phases = [
            AllocationPhase::Reserved,
            AllocationPhase::Materialized,
            AllocationPhase::Committed,
            AllocationPhase::Retained,
        ];
        assert_eq!(phases.len(), 4);
        assert_ne!(phases[0], phases[1]);
        assert_ne!(phases[1], phases[2]);
        assert_ne!(phases[2], phases[3]);
    }

    #[test]
    fn allocation_failure_reason_is_bounded_ascii_and_errors_are_static() {
        let max_len = "x".repeat(128);
        let too_long = "x".repeat(129);
        for value in ["a", max_len.as_str()] {
            assert!(AllocationFailureReason::parse(value).is_ok());
        }
        for value in [
            "",
            too_long.as_str(),
            "Reason",
            "reason\0",
            "reason/path",
            "café",
        ] {
            let error = match AllocationFailureReason::parse(value) {
                Ok(_) => panic!("invalid reason accepted"),
                Err(error) => error,
            };
            assert_eq!(error, AllocationValueError::InvalidFailureReason);
            if !value.is_empty() {
                assert!(!error.to_string().contains(value));
            }
        }
    }
}

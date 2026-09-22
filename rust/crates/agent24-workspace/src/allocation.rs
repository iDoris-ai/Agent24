use crate::WorkspaceError;

const INVALID_ID: WorkspaceError = WorkspaceError::InvalidValue {
    field: "allocation_id",
};
const INVALID_PHASE: WorkspaceError = WorkspaceError::InvalidValue { field: "phase" };

/// Opaque, validated allocation journal identifier (`wa_` plus a canonical ULID).
#[derive(Clone, PartialEq, Eq)]
pub struct AllocationId(String);

impl AllocationId {
    pub fn parse(value: &str) -> Result<Self, WorkspaceError> {
        let bytes = value.as_bytes();
        if bytes.len() != 29
            || !bytes.starts_with(b"wa_")
            || !(b'0'..=b'7').contains(&bytes[3])
            || !bytes[4..]
                .iter()
                .all(|byte| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(byte))
        {
            return Err(INVALID_ID);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
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

impl AllocationPhase {
    #[allow(dead_code)]
    pub(crate) const fn as_db_str(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::Materialized => "materialized",
            Self::Committed => "committed",
            Self::Retained => "retained",
        }
    }

    #[allow(dead_code)]
    pub(crate) fn from_db(value: &str) -> Result<Self, WorkspaceError> {
        match value {
            "reserved" => Ok(Self::Reserved),
            "materialized" => Ok(Self::Materialized),
            "committed" => Ok(Self::Committed),
            "retained" => Ok(Self::Retained),
            _ => Err(INVALID_PHASE),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn allocation_id_validates_canonical_ulid_boundaries() {
        for value in [
            format!("wa_0{}", "0".repeat(25)),
            format!("wa_7{}", "Z".repeat(25)),
        ] {
            assert_eq!(AllocationId::parse(&value).unwrap().as_str(), value);
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
            assert!(
                matches!(AllocationId::parse(value), Err(error) if error == INVALID_ID),
                "{value:?}"
            );
        }
        let nul = format!("wa_0{}\0", "0".repeat(24));
        assert!(matches!(AllocationId::parse(&nul), Err(error) if error == INVALID_ID));
    }

    #[test]
    fn allocation_phase_is_a_closed_database_set() {
        for (value, phase) in [
            ("reserved", AllocationPhase::Reserved),
            ("materialized", AllocationPhase::Materialized),
            ("committed", AllocationPhase::Committed),
            ("retained", AllocationPhase::Retained),
        ] {
            assert_eq!(AllocationPhase::from_db(value).unwrap(), phase);
            assert_eq!(phase.as_db_str(), value);
        }
        assert_eq!(AllocationPhase::from_db("unknown"), Err(INVALID_PHASE));
    }
}

use chrono::{DateTime, SecondsFormat};
use thiserror::Error;

const MAX_WORKSPACE_MS: i64 = 7 * 86_400_000;
const MAX_HOST_MS: i64 = 90_000;

pub type WorkspaceResult<T> = Result<T, WorkspaceStoreError>;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum WorkspaceStoreError {
    #[error("corrupt {table} row: invalid {field}")]
    CorruptRow {
        table: &'static str,
        field: &'static str,
    },
    #[error("invalid workspace value: {field}")]
    InvalidValue { field: &'static str },
    #[error("workspace database error")]
    Database,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct WorkspaceInstant {
    text: String,
    epoch_millis: i64,
}

impl WorkspaceInstant {
    pub fn parse(text: &str) -> WorkspaceResult<Self> {
        let parsed = DateTime::parse_from_rfc3339(text)
            .map_err(|_| WorkspaceStoreError::InvalidValue { field: "timestamp" })?;
        if parsed.to_rfc3339_opts(SecondsFormat::Millis, true) != text {
            return Err(WorkspaceStoreError::InvalidValue { field: "timestamp" });
        }
        Ok(Self {
            text: text.to_owned(),
            epoch_millis: parsed.timestamp_millis(),
        })
    }
    pub fn as_str(&self) -> &str {
        &self.text
    }
    pub fn epoch_millis(&self) -> i64 {
        self.epoch_millis
    }
}

impl std::fmt::Display for WorkspaceInstant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceTtl(i64);
impl WorkspaceTtl {
    pub fn new(milliseconds: i64) -> WorkspaceResult<Self> {
        (1..=MAX_WORKSPACE_MS)
            .contains(&milliseconds)
            .then_some(Self(milliseconds))
            .ok_or(WorkspaceStoreError::InvalidValue {
                field: "workspace_ttl",
            })
    }
    pub fn millis(self) -> i64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostLeaseTtl(i64);
impl HostLeaseTtl {
    pub fn new(milliseconds: i64) -> WorkspaceResult<Self> {
        (1..=MAX_HOST_MS)
            .contains(&milliseconds)
            .then_some(Self(milliseconds))
            .ok_or(WorkspaceStoreError::InvalidValue {
                field: "host_lease_ttl",
            })
    }
    pub fn millis(self) -> i64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceLeaseId(String);
impl WorkspaceLeaseId {
    pub fn parse(value: &str) -> WorkspaceResult<Self> {
        const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
        let b = value.as_bytes();
        if b.len() != 29
            || &b[..3] != b"wl_"
            || !matches!(b[3], b'0'..=b'7')
            || !b[4..].iter().all(|c| ALPHABET.contains(c))
        {
            return Err(WorkspaceStoreError::InvalidValue { field: "lease_id" });
        }
        Ok(Self(value.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootIdentity {
    Unix {
        device: [u8; 8],
        inode: [u8; 8],
    },
    Windows {
        volume_serial: [u8; 8],
        file_id: [u8; 16],
    },
}

impl RootIdentity {
    pub fn unix(device: &[u8], inode: &[u8]) -> WorkspaceResult<Self> {
        Ok(Self::Unix {
            device: device
                .try_into()
                .map_err(|_| WorkspaceStoreError::InvalidValue {
                    field: "unix_device",
                })?,
            inode: inode
                .try_into()
                .map_err(|_| WorkspaceStoreError::InvalidValue {
                    field: "unix_inode",
                })?,
        })
    }
    pub fn windows(volume_serial: &[u8], file_id: &[u8]) -> WorkspaceResult<Self> {
        Ok(Self::Windows {
            volume_serial: volume_serial.try_into().map_err(|_| {
                WorkspaceStoreError::InvalidValue {
                    field: "windows_volume_serial",
                }
            })?,
            file_id: file_id
                .try_into()
                .map_err(|_| WorkspaceStoreError::InvalidValue {
                    field: "windows_file_id",
                })?,
        })
    }
}

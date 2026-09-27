/// Static workspace-domain errors; values and paths are never included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceError {
    InvalidSpec { field: &'static str },
    UnsupportedPlatform,
    RootUnavailable { reason: &'static str },
    RootConflict { reason: &'static str },
}

impl std::fmt::Display for WorkspaceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSpec { field } => write!(formatter, "invalid workspace spec: {field}"),
            Self::UnsupportedPlatform => {
                write!(formatter, "workspace roots unsupported on this platform")
            }
            Self::RootUnavailable { reason } => {
                write!(formatter, "workspace root unavailable: {reason}")
            }
            Self::RootConflict { reason } => write!(formatter, "workspace root conflict: {reason}"),
        }
    }
}

impl std::error::Error for WorkspaceError {}

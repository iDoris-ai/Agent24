/// Static workspace-domain errors; values and paths are never included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceError {
    InvalidSpec { field: &'static str },
}

impl std::fmt::Display for WorkspaceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSpec { field } => write!(formatter, "invalid workspace spec: {field}"),
        }
    }
}

impl std::error::Error for WorkspaceError {}

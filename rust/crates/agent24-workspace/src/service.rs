/// Workspace service boundary. Operational behavior is added in later slices.
pub struct WorkspaceService {
    _private: (),
}

impl WorkspaceService {
    pub const fn new() -> Self {
        Self { _private: () }
    }
}

impl Default for WorkspaceService {
    fn default() -> Self {
        Self::new()
    }
}

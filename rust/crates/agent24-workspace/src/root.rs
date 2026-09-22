/// Opaque proof that a workspace root was pinned by the service boundary.
///
/// This type intentionally has no path constructor or serialization/debug surface.
pub struct PinnedWorkspaceRoot {
    _sealed: (),
}

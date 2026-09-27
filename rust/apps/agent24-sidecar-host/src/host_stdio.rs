//! Safe, independently owned copies of the host process standard streams.
//!
//! Acquisition is deliberately dormant: callers must acquire the complete pair
//! before starting any workers, and `run()` does not use this module.

use std::{fmt, fs::File, io};

#[derive(Debug)]
pub(crate) struct HostStdio {
    pub(crate) stdin: File,
    pub(crate) stdout: File,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct HostStdioError {
    stage: &'static str,
    kind: io::ErrorKind,
}

impl HostStdioError {
    fn from_io(stage: &'static str, error: io::Error) -> Self {
        Self {
            stage,
            kind: error.kind(),
        }
    }
}

impl fmt::Debug for HostStdioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostStdioError")
            .field("stage", &self.stage)
            .field("kind", &self.kind)
            .finish()
    }
}

impl fmt::Display for HostStdioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({:?})", self.stage, self.kind)
    }
}

impl std::error::Error for HostStdioError {}

#[cfg(unix)]
fn clone_streams(
    stdin: &impl std::os::fd::AsFd,
    stdout: &impl std::os::fd::AsFd,
) -> Result<HostStdio, HostStdioError> {
    let stdin = stdin
        .as_fd()
        .try_clone_to_owned()
        .map(File::from)
        .map_err(|error| HostStdioError::from_io("stdin", error))?;
    let stdout = stdout
        .as_fd()
        .try_clone_to_owned()
        .map(File::from)
        .map_err(|error| HostStdioError::from_io("stdout", error))?;
    Ok(HostStdio { stdin, stdout })
}

#[cfg(windows)]
fn clone_streams(
    stdin: &impl std::os::windows::io::AsHandle,
    stdout: &impl std::os::windows::io::AsHandle,
) -> Result<HostStdio, HostStdioError> {
    let stdin = stdin
        .as_handle()
        .try_clone_to_owned()
        .map(File::from)
        .map_err(|error| HostStdioError::from_io("stdin", error))?;
    let stdout = stdout
        .as_handle()
        .try_clone_to_owned()
        .map(File::from)
        .map_err(|error| HostStdioError::from_io("stdout", error))?;
    Ok(HostStdio { stdin, stdout })
}

/// Clone both process streams before returning either owned stream.
pub(crate) fn acquire() -> Result<HostStdio, HostStdioError> {
    #[cfg(unix)]
    {
        let stdin = io::stdin();
        let stdout = io::stdout();
        clone_streams(&stdin, &stdout)
    }

    #[cfg(windows)]
    {
        let stdin = io::stdin();
        let stdout = io::stdout();
        clone_streams(&stdin, &stdout)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn clones_have_independent_lifetimes_and_round_trip() {
        use std::{
            io::{Read, Write},
            os::unix::net::UnixStream,
        };

        let (mut peer, source) = UnixStream::pair().expect("pair");
        let streams = clone_streams(&source, &source).expect("clone streams");
        assert!(
            rustix::io::fcntl_getfd(&streams.stdin)
                .expect("stdin descriptor flags")
                .contains(rustix::io::FdFlags::CLOEXEC)
        );
        assert!(
            rustix::io::fcntl_getfd(&streams.stdout)
                .expect("stdout descriptor flags")
                .contains(rustix::io::FdFlags::CLOEXEC)
        );
        drop(source);

        peer.write_all(b"from peer").expect("write peer");
        let mut input = [0; 9];
        let mut stdin = streams.stdin;
        stdin.read_exact(&mut input).expect("read clone");
        assert_eq!(&input, b"from peer");

        let mut stdout = streams.stdout;
        stdout.write_all(b"from clone").expect("write clone");
        let mut output = [0; 10];
        peer.read_exact(&mut output).expect("read peer");
        assert_eq!(&output, b"from clone");
    }

    #[test]
    fn errors_keep_only_stage_and_error_kind() {
        let error =
            HostStdioError::from_io("stdout", io::Error::other("private path and secret token"));
        assert_eq!(error.kind, io::ErrorKind::Other);
        assert!(!error.to_string().contains("private path"));
        assert!(!format!("{error:?}").contains("secret token"));
    }
}

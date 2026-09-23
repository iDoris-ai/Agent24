use crate::target::OwnedPipes;

#[cfg(unix)]
pub(crate) type NativeStdin = std::process::ChildStdin;
#[cfg(windows)]
pub(crate) type NativeStdin = tokio::process::ChildStdin;
#[cfg(unix)]
pub(crate) type NativeStdout = std::process::ChildStdout;
#[cfg(windows)]
pub(crate) type NativeStdout = tokio::process::ChildStdout;
#[cfg(unix)]
pub(crate) type NativeStderr = std::process::ChildStderr;
#[cfg(windows)]
pub(crate) type NativeStderr = tokio::process::ChildStderr;

pub(crate) struct TargetPipes {
    stdin: Option<NativeStdin>,
    stdout: NativeStdout,
    stderr: NativeStderr,
}

impl From<OwnedPipes> for TargetPipes {
    fn from(pipes: OwnedPipes) -> Self {
        Self {
            stdin: Some(pipes.stdin),
            stdout: pipes.stdout,
            stderr: pipes.stderr,
        }
    }
}

impl TargetPipes {
    pub(crate) fn close_stdin(&mut self) {
        drop(self.stdin.take());
    }

    pub(crate) fn stdin_mut(&mut self) -> Option<&mut NativeStdin> {
        self.stdin.as_mut()
    }

    pub(crate) fn stdout_mut(&mut self) -> &mut NativeStdout {
        &mut self.stdout
    }

    pub(crate) fn stderr_mut(&mut self) -> &mut NativeStderr {
        &mut self.stderr
    }
}

//! Bounded synchronous I/O adapters for Windows ownership tests.

use std::{
    io::{self, PipeReader, PipeWriter, Read},
    path::PathBuf,
    process::Command,
    sync::mpsc,
    thread,
    time::Duration,
};

use crate::owner::OwnedPipes;

pub(crate) struct NativePipes {
    pub(crate) stdin: PipeWriter,
    pub(crate) stdout: PipeReader,
    pub(crate) stderr: PipeReader,
}

impl TryFrom<OwnedPipes> for NativePipes {
    type Error = io::Error;

    fn try_from(pipes: OwnedPipes) -> io::Result<Self> {
        Ok(Self {
            stdin: pipes.stdin,
            stdout: pipes.stdout,
            stderr: pipes.stderr,
        })
    }
}

/// Read at most `limit` bytes from both pipes concurrently, then run cleanup
/// before returning results for assertions. A stalled reader triggers cleanup
/// at the deadline so the pipe closes before its worker is joined.
pub(crate) fn read_pair_then_cleanup<C>(
    stdout: PipeReader,
    stderr: PipeReader,
    stdout_limit: u64,
    stderr_limit: u64,
    timeout: Duration,
    cleanup: C,
) -> io::Result<(Vec<u8>, Vec<u8>)>
where
    C: FnOnce() -> io::Result<()>,
{
    let (tx, rx) = mpsc::channel();
    let stdout_worker = spawn_reader(stdout, stdout_limit, tx.clone(), 0);
    let stderr_worker = spawn_reader(stderr, stderr_limit, tx, 1);
    let deadline = std::time::Instant::now() + timeout;
    let mut values: [Option<io::Result<Vec<u8>>>; 2] = [None, None];
    let mut timed_out = false;

    while values.iter().any(Option::is_none) {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            timed_out = true;
            break;
        }
        match rx.recv_timeout(remaining) {
            Ok((index, value)) => values[index] = Some(value),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                timed_out = true;
                break;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                values
                    .iter_mut()
                    .filter(|value| value.is_none())
                    .for_each(|value| {
                        *value = Some(Err(io::Error::other("pipe reader stopped unexpectedly")));
                    });
            }
        }
    }

    let cleanup_result = cleanup();
    if timed_out {
        let cleanup_deadline = std::time::Instant::now() + Duration::from_secs(5);
        while values.iter().any(Option::is_none) {
            let remaining = cleanup_deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            if let Ok((index, value)) = rx.recv_timeout(remaining) {
                values[index] = Some(value);
            } else {
                break;
            }
        }
    }

    cleanup_result?;
    if values.iter().any(Option::is_none) {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "child pipe read timed out",
        ));
    }
    let stdout_join = stdout_worker.join();
    let stderr_join = stderr_worker.join();
    if stdout_join.is_err() || stderr_join.is_err() {
        return Err(io::Error::other("pipe reader thread panicked"));
    }
    if timed_out {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "child pipe read timed out",
        ));
    }
    let stdout = values[0]
        .take()
        .ok_or_else(|| io::Error::other("stdout reader returned no result"))??;
    let stderr = values[1]
        .take()
        .ok_or_else(|| io::Error::other("stderr reader returned no result"))??;
    Ok((stdout, stderr))
}

fn spawn_reader(
    file: PipeReader,
    limit: u64,
    tx: mpsc::Sender<(usize, io::Result<Vec<u8>>)>,
    index: usize,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = file.take(limit).read_to_end(&mut bytes).map(|_| bytes);
        let _ = tx.send((index, result));
    })
}

pub(crate) fn windows_executable(name: &str) -> PathBuf {
    let mut path = system_root();
    path.push("System32");
    path.push(name);
    path
}

pub(crate) fn powershell_executable() -> PathBuf {
    let mut path = system_root();
    path.push("System32");
    path.push("WindowsPowerShell");
    path.push("v1.0");
    path.push("powershell.exe");
    path
}

fn system_root() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
}

pub(crate) fn process_is_alive(pid: u32) -> io::Result<bool> {
    let output = Command::new(windows_executable("tasklist.exe"))
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "tasklist failed with status {}",
            output.status
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\"")))
}

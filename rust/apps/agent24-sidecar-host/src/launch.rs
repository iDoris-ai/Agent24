use std::{ffi::OsString, fmt, io, path::PathBuf};

use crate::{
    actor::Phase,
    cleanup::{CleanupStepError, cleanup_step},
    launch_order::LaunchControl,
    pipe_access::TargetPipes,
    pre_owned_cleanup::{CleanupOwner, PreOwnedCleanup},
    target::TreeObservation,
};
use agent24_sidecar_host_protocol::Request;

#[cfg(windows)]
use crate::{
    owner::{GenerationId, GenerationOwner},
    target::{OwnedPipes, OwnedTarget},
};
#[cfg(unix)]
use crate::{
    posix::{LaunchSpec, OwnedGeneration},
    target::{OwnedPipes, OwnedTarget},
};
#[cfg(windows)]
use processkit::IsolatedPipedCommand;

pub(crate) struct LaunchIntent {
    request_id: u64,
    executable: PathBuf,
    cwd: PathBuf,
    argv: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
}

impl LaunchIntent {
    pub(crate) fn from_request(request: Request) -> Result<Self, LaunchFailure> {
        let Request::Launch {
            request_id,
            executable,
            cwd,
            argv,
            env,
            ..
        } = request
        else {
            return Err(LaunchFailure::NotLaunch);
        };
        Ok(Self {
            request_id,
            executable: executable.into(),
            cwd: cwd.into(),
            argv: argv.into_iter().map(OsString::from).collect(),
            env: env
                .into_iter()
                .map(|(key, value)| (OsString::from(key), OsString::from(value)))
                .collect(),
        })
    }
}

impl fmt::Debug for LaunchIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LaunchIntent")
            .field("request_id", &self.request_id)
            .field("argv_count", &self.argv.len())
            .field("env_count", &self.env.len())
            .finish()
    }
}

pub(crate) enum LaunchFailure {
    NotLaunch,
    Start(io::ErrorKind),
    Pipes {
        kind: io::ErrorKind,
        cleanup: Box<CleanupOwner>,
    },
}

impl fmt::Debug for LaunchFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotLaunch => formatter.write_str("NotLaunch"),
            Self::Start(kind) => formatter.debug_tuple("Start").field(kind).finish(),
            Self::Pipes { kind, .. } => formatter
                .debug_struct("Pipes")
                .field("kind", kind)
                .finish_non_exhaustive(),
        }
    }
}

impl PartialEq for LaunchFailure {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::NotLaunch, Self::NotLaunch) => true,
            (Self::Start(left), Self::Start(right)) => left == right,
            (Self::Pipes { kind: left, .. }, Self::Pipes { kind: right, .. }) => left == right,
            _ => false,
        }
    }
}

impl Eq for LaunchFailure {}

impl LaunchFailure {
    pub(crate) fn into_pre_owned_cleanup(
        self,
        phase: Phase,
        limits: crate::actor::Deadlines,
    ) -> Result<PreOwnedCleanup, Self> {
        match self {
            Self::Pipes { cleanup, .. } => Ok(PreOwnedCleanup::new(*cleanup, phase, limits, false)),
            other => Err(other),
        }
    }
}

impl fmt::Display for LaunchFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("launch failed")
    }
}

#[cfg(any(unix, windows))]
pub(crate) struct OwnedLaunch {
    request_id: u64,
    target: OwnedTarget,
    pipes: TargetPipes,
}

#[cfg(any(unix, windows))]
fn take_preserving<T, P>(
    mut target: T,
    take: impl FnOnce(&mut T) -> io::Result<P>,
) -> Result<(T, P), (io::ErrorKind, T)> {
    match take(&mut target) {
        Ok(value) => Ok((target, value)),
        Err(error) => Err((error.kind(), target)),
    }
}

#[cfg(any(unix, windows))]
fn take_pipes(target: OwnedTarget) -> Result<(OwnedTarget, OwnedPipes), LaunchFailure> {
    match take_preserving(target, OwnedTarget::take_pipes) {
        Ok((target, pipes)) => Ok((target, pipes)),
        Err((kind, target)) => Err(LaunchFailure::Pipes {
            kind,
            cleanup: Box::new(CleanupOwner::new(target, None)),
        }),
    }
}

#[cfg(unix)]
impl OwnedLaunch {
    pub(crate) fn start(intent: LaunchIntent) -> Result<Self, LaunchFailure> {
        let request_id = intent.request_id;
        let mut spec = LaunchSpec::new(intent.executable, intent.cwd);
        for arg in intent.argv {
            spec = spec.arg(arg);
        }
        for (key, value) in intent.env {
            spec = spec.env(key, value);
        }
        let owner =
            OwnedGeneration::launch(spec).map_err(|error| LaunchFailure::Start(error.kind()))?;
        let (target, pipes) = take_pipes(OwnedTarget::from_owned(owner))?;
        Ok(Self {
            request_id,
            target,
            pipes: pipes.into(),
        })
    }

    pub(crate) const fn request_id(&self) -> u64 {
        self.request_id
    }

    fn target_mut(&mut self) -> &mut OwnedTarget {
        &mut self.target
    }

    fn parts_mut(&mut self) -> (&mut OwnedTarget, &mut TargetPipes) {
        (&mut self.target, &mut self.pipes)
    }

    pub(crate) fn pipes_mut(&mut self) -> &mut TargetPipes {
        &mut self.pipes
    }

    pub(crate) fn into_cleanup_parts(self) -> (OwnedTarget, TargetPipes) {
        (self.target, self.pipes)
    }
}

#[cfg(any(unix, windows))]
impl LaunchControl for OwnedLaunch {
    fn stop(&mut self, force: bool) -> io::Result<()> {
        if force {
            self.target_mut().request_stop(true)
        } else {
            self.parts_mut().1.close_stdin();
            self.target_mut().request_stop(false)
        }
    }
    fn observe_exit(&mut self) -> io::Result<crate::target::ExitObservation> {
        self.target_mut().observe_exit()
    }
    fn cleanup(&mut self, phase: &mut Phase) -> Result<TreeObservation, CleanupStepError> {
        cleanup_step(phase, self.target_mut())
    }
}

#[cfg(windows)]
impl OwnedLaunch {
    pub(crate) fn start(intent: LaunchIntent) -> Result<Self, LaunchFailure> {
        let request_id = intent.request_id;
        let generation =
            GenerationId::new(request_id).map_err(|error| LaunchFailure::Start(error.kind()))?;
        let owner =
            GenerationOwner::new(generation).map_err(|error| LaunchFailure::Start(error.kind()))?;
        let command = IsolatedPipedCommand::new(intent.executable)
            .current_dir(intent.cwd)
            .args(intent.argv)
            .env_clear()
            .envs(intent.env);
        let owner = owner
            .spawn(command)
            .map_err(|error| LaunchFailure::Start(error.kind()))?;
        let (target, pipes) = take_pipes(OwnedTarget::from_owned(owner))?;
        Ok(Self {
            request_id,
            target,
            pipes: pipes.into(),
        })
    }

    pub(crate) const fn request_id(&self) -> u64 {
        self.request_id
    }

    fn target_mut(&mut self) -> &mut OwnedTarget {
        &mut self.target
    }

    fn parts_mut(&mut self) -> (&mut OwnedTarget, &mut TargetPipes) {
        (&mut self.target, &mut self.pipes)
    }

    pub(crate) fn pipes_mut(&mut self) -> &mut TargetPipes {
        &mut self.pipes
    }

    pub(crate) fn into_cleanup_parts(self) -> (OwnedTarget, TargetPipes) {
        (self.target, self.pipes)
    }
}

#[cfg(all(test, unix))]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{posix::tests::wait_for_reaper_idle, target::TreeObservation};
    use std::{
        collections::BTreeMap,
        io::Read,
        sync::mpsc,
        time::{Duration, Instant},
    };

    fn request(executable: &str, cwd: &str) -> Request {
        Request::Launch {
            version: 1,
            request_id: 17,
            executable: executable.to_owned(),
            cwd: cwd.to_owned(),
            argv: vec![
                "-c".to_owned(),
                "printf \"$PWD|$SIDE|$1|${HOME-unset}\"; sleep 30".to_owned(),
                "ignored-zero".to_owned(),
                "argv-value".to_owned(),
            ],
            env: BTreeMap::from([(String::from("SIDE"), String::from("env-value"))]),
        }
    }

    fn reap(launch: &mut OwnedLaunch) {
        launch.parts_mut().0.request_stop(true).expect("force stop");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !matches!(
            launch.parts_mut().0.reap_step().expect("reap step"),
            TreeObservation::ConfirmedEmpty
        ) {
            assert!(Instant::now() < deadline, "child was not reaped");
        }
    }

    fn start_bounded_read<const N: usize, R>(
        mut reader: R,
    ) -> mpsc::Receiver<(io::Result<[u8; N]>, R)>
    where
        R: Read + Send + 'static,
    {
        let (send, receive) = mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = [0; N];
            let result = reader.read_exact(&mut bytes).map(|()| bytes);
            let _ = send.send((result, reader));
        });
        receive
    }

    fn finish_bounded_read<const N: usize, R>(
        receive: mpsc::Receiver<(io::Result<[u8; N]>, R)>,
    ) -> io::Result<([u8; N], R)> {
        let (result, reader) = receive.recv_timeout(Duration::from_secs(2)).map_err(|_| {
            io::Error::new(io::ErrorKind::TimedOut, "child marker deadline expired")
        })?;
        result.map(|bytes| (bytes, reader))
    }

    #[test]
    fn launch_intent_preserves_fields_and_redacts_debug() {
        let intent = LaunchIntent::from_request(request("/bin/sh", "/")).expect("launch");
        assert_eq!(intent.request_id, 17);
        assert_eq!(intent.executable, PathBuf::from("/bin/sh"));
        assert_eq!(intent.cwd, PathBuf::from("/"));
        assert_eq!(intent.argv.len(), 4);
        let debug = format!("{intent:?}");
        assert!(
            ["request_id: 17", "argv_count: 4", "env_count: 1"]
                .iter()
                .all(|field| debug.contains(field))
        );
        assert!(
            ["/bin/sh", "env-value", "argv-value", "ignored-zero"]
                .iter()
                .all(|secret| !debug.contains(secret))
        );
    }

    #[test]
    fn unix_launch_owns_pipes_and_authoritative_target_until_reap() {
        let _test_guard = crate::posix::tests::test_lock();
        assert!(std::env::var_os("HOME").is_some());
        let intent = LaunchIntent::from_request(request("/bin/sh", "/")).expect("launch");
        let mut launch = OwnedLaunch::start(intent).expect("owned launch");
        let mut stdout_text = [0; 28];
        let (mut stdout, _stderr) = {
            let (_, pipes) = launch.parts_mut();
            let _ = pipes.stdin_mut().expect("stdin");
            (
                pipes.take_stdout().expect("stdout moves once"),
                pipes.take_stderr().expect("stderr stays available"),
            )
        };
        stdout
            .read_exact(&mut stdout_text)
            .expect("read moved stdout");
        assert_eq!(stdout_text, *b"/|env-value|argv-value|unset");
        reap(&mut launch);
        drop(launch);
        wait_for_reaper_idle();
    }

    #[test]
    fn moved_pipes_deliver_eof_and_leave_the_launch_authoritative() {
        let _test_guard = crate::posix::tests::test_lock();
        let request = Request::Launch {
            version: 1,
            request_id: 18,
            executable: "/bin/sh".into(),
            cwd: "/".into(),
            argv: vec![
                "-c".into(),
                "cat >/dev/null; printf eof-marker; printf err-marker >&2; exec sleep 30".into(),
            ],
            env: BTreeMap::new(),
        };
        let mut launch = OwnedLaunch::start(LaunchIntent::from_request(request).unwrap()).unwrap();
        assert_eq!(launch.request_id(), 18);
        let (stdout, stderr) = {
            let (_, pipes) = launch.parts_mut();
            let stdout = pipes.take_stdout().expect("stdout moves once");
            assert!(pipes.take_stdout().is_none(), "stdout moved twice");
            assert!(pipes.stdin_mut().is_some(), "moving stdout closed stdin");
            let stderr = pipes.take_stderr().expect("stderr moves once");
            assert!(pipes.take_stderr().is_none(), "stderr moved twice");
            assert!(pipes.stdin_mut().is_some(), "moving stderr closed stdin");
            (stdout, stderr)
        };
        let stdout_reader = start_bounded_read::<10, _>(stdout);
        let stderr_reader = start_bounded_read::<10, _>(stderr);
        let stdin_unavailable = {
            let (_, pipes) = launch.parts_mut();
            pipes.close_stdin();
            pipes.close_stdin();
            pipes.stdin_mut().is_none()
        };
        let stdout = finish_bounded_read(stdout_reader);
        let stderr = finish_bounded_read(stderr_reader);
        let observation = launch.target_mut().observe_exit();
        reap(&mut launch);
        drop(launch);
        wait_for_reaper_idle();
        assert!(stdin_unavailable, "closed stdin remained available");
        assert_eq!(
            observation.expect("observe owner"),
            crate::target::ExitObservation::Running
        );
        let (out, mut stdout) = stdout.expect("stdout EOF marker");
        let (err, _stderr) = stderr.expect("stderr EOF marker");
        assert_eq!(&out, b"eof-marker");
        assert_eq!(&err, b"err-marker");
        let mut eof = [0];
        assert_eq!(stdout.read(&mut eof).expect("moved stdout stays open"), 0);
    }

    #[test]
    fn missing_executable_is_static_and_redacted() {
        let _test_guard = crate::posix::tests::test_lock();
        let missing = "/definitely/missing/agent24-sidecar-executable";
        let error =
            match OwnedLaunch::start(LaunchIntent::from_request(request(missing, "/")).unwrap()) {
                Err(error) => error,
                Ok(_) => panic!("missing executable must fail"),
            };
        assert_eq!(error, LaunchFailure::Start(io::ErrorKind::NotFound));
        assert!(!format!("{error:?}").contains(missing));
    }

    #[test]
    fn failed_transfer_returns_the_same_owner() {
        struct OwnerMarker {
            generation: u64,
            attempted: bool,
        }

        let failure = take_preserving(
            OwnerMarker {
                generation: 17,
                attempted: false,
            },
            |owner| {
                owner.attempted = true;
                Err::<(), _>(io::Error::from(io::ErrorKind::InvalidInput))
            },
        );
        let (kind, owner) = match failure {
            Err(failure) => failure,
            Ok(_) => panic!("transfer must fail"),
        };
        assert_eq!(kind, io::ErrorKind::InvalidInput);
        assert_eq!(owner.generation, 17);
        assert!(owner.attempted);
    }

    #[test]
    fn pipe_failure_is_consumed_directly_into_pre_owned_cleanup() {
        let _test_guard = crate::posix::tests::test_lock();
        let owner =
            OwnedGeneration::launch(LaunchSpec::new("/bin/sh", "/").arg("-c").arg("sleep 30"))
                .expect("launch child");
        let mut target = OwnedTarget::from_owned(owner);
        drop(target.take_pipes().expect("first pipe transfer"));
        let failure = match take_pipes(target) {
            Err(failure) => failure,
            Ok(_) => panic!("second pipe transfer unexpectedly succeeded"),
        };
        let now = std::time::Instant::now();
        let limits = crate::actor::Deadlines {
            launch: std::time::Duration::ZERO,
            ready: std::time::Duration::ZERO,
            graceful: std::time::Duration::ZERO,
            force: std::time::Duration::from_secs(2),
            drain: std::time::Duration::from_secs(2),
        };
        let mut cleanup = failure
            .into_pre_owned_cleanup(Phase::ForceStopping(now + limits.force), limits)
            .expect("Pipes failure transfers its owner");
        let deadline = now + std::time::Duration::from_secs(2);
        loop {
            match cleanup
                .step(std::time::Instant::now())
                .expect("cleanup turn")
            {
                TreeObservation::ConfirmedEmpty => break,
                TreeObservation::Present if std::time::Instant::now() < deadline => {
                    std::thread::yield_now();
                }
                result => panic!("cleanup did not confirm empty: {result:?}"),
            }
        }
        crate::posix::tests::wait_for_reaper_idle();
    }
}

#[cfg(all(test, windows))]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod windows_tests {
    use super::*;
    use crate::target::{ExitObservation, TreeObservation};
    use crate::windows_test_io::{powershell_executable, read_pair_then_cleanup};
    use std::{
        collections::BTreeMap, io::Read as _, path::Path, sync::mpsc, thread, time::Duration,
    };

    fn request(cwd: &Path) -> Request {
        let system_root = std::env::var("SystemRoot").expect("SystemRoot");
        Request::Launch {
            version: 1,
            request_id: 17,
            executable: powershell_executable().display().to_string(),
            cwd: cwd.display().to_string(),
            argv: vec![
                String::from("-NoLogo"),
                String::from("-NoProfile"),
                String::from("-NonInteractive"),
                String::from("-File"),
                cwd.join("launch.ps1").display().to_string(),
                String::from("ignored-zero"),
                String::from("argv-value"),
            ],
            env: BTreeMap::from([
                (String::from("SIDE"), String::from("env-value")),
                (String::from("SystemRoot"), system_root),
            ]),
        }
    }

    fn descendant_request(cwd: &Path) -> Request {
        let mut request = request(cwd);
        if let Request::Launch { argv, env, .. } = &mut request {
            let child_powershell = powershell_executable()
                .display()
                .to_string()
                .replace('\'', "''");
            argv[3] = String::from("-Command");
            argv[4] = format!(
                "$start = [System.Diagnostics.ProcessStartInfo]::new(); $start.FileName = '{child_powershell}'; $start.Arguments = '-NoLogo -NoProfile -NonInteractive -Command \"[System.Threading.Thread]::Sleep(30000)\"'; $start.UseShellExecute = $false; $child = [System.Diagnostics.Process]::Start($start); [Console]::Out.Write('ready'); [Console]::Out.Flush(); [Console]::Error.Write('error'); [Console]::Error.Flush(); exit 0"
            );
            argv.truncate(5);
            env.remove("SIDE");
        }
        request
    }

    fn reap(launch: &mut OwnedLaunch) {
        launch
            .parts_mut()
            .0
            .request_stop(true)
            .expect("force Job tree");
        for _ in 0..100 {
            if launch.parts_mut().0.reap_step().expect("reap Job tree")
                == TreeObservation::ConfirmedEmpty
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("Job tree was not reaped before deadline");
    }

    #[test]
    fn windows_launch_preserves_request_and_all_pipes() {
        let parent_current_dir = std::env::current_dir().expect("current cwd");
        let mut cwd = std::env::temp_dir();
        cwd.push(format!("agent24-sidecar-cwd-{}", std::process::id()));
        std::fs::create_dir_all(&cwd).expect("cwd");
        std::fs::write(cwd.join("cwd-sentinel"), b"").expect("sentinel");
        std::fs::write(
            cwd.join("launch.ps1"),
            b"param($first,$second)\n$inherited = if ($env:PATH) {$env:PATH} else {'unset'}\n$cwd=if(Test-Path -LiteralPath 'cwd-sentinel') {'cwd-ok'} else {'cwd-bad'}\n$args=\"$first,$second\"\n$out=('{0,-6}|{1,-9}|{2,-23}|{3,-5}' -f $cwd,$env:SIDE,$args,$inherited)\n[Console]::Out.Write($out)\n[Console]::Out.Flush()\n[Console]::Error.Write('err')\n[Console]::Error.Flush()",
        )
        .expect("script");
        assert_ne!(cwd, parent_current_dir);
        assert!(!parent_current_dir.join("cwd-sentinel").exists());
        assert!(std::env::var_os("PATH").is_some());
        let mut launch =
            OwnedLaunch::start(LaunchIntent::from_request(request(&cwd)).expect("intent"))
                .expect("owned launch");
        let request_id = launch.request_id();
        let (stdout_pipe, stderr_pipe) = {
            let (_, pipes) = launch.parts_mut();
            let _ = pipes.stdin_mut().expect("stdin");
            (
                pipes.take_stdout().expect("stdout moves once"),
                pipes.take_stderr().expect("stderr moves once"),
            )
        };
        let (stdout, stderr) = read_pair_then_cleanup(
            stdout_pipe,
            stderr_pipe,
            46,
            3,
            Duration::from_secs(5),
            || {
                reap(&mut launch);
                Ok(())
            },
        )
        .expect("bounded output read and launch cleanup");
        let stdout = String::from_utf8(stdout).expect("stdout UTF-8");
        let stderr = String::from_utf8(stderr).expect("stderr UTF-8");
        let fields: Vec<_> = stdout.split('|').collect();
        std::fs::remove_dir_all(&cwd).expect("cleanup cwd");
        assert_eq!(request_id, 17);
        assert_eq!(fields[0], "cwd-ok");
        assert_eq!(
            &fields[1..],
            ["env-value", "ignored-zero,argv-value", "unset"]
        );
        assert_eq!(stderr, "err");
    }

    #[test]
    fn windows_moved_pipes_deliver_eof_and_leave_the_launch_authoritative() {
        let cwd = std::env::temp_dir();
        let mut launch = OwnedLaunch::start(LaunchIntent::from_request(Request::Launch {
            version: 1,
            request_id: 20,
            executable: powershell_executable().display().to_string(),
            cwd: cwd.display().to_string(),
            argv: vec!["-NoLogo".into(), "-NoProfile".into(), "-NonInteractive".into(), "-Command".into(),
                "$null = [Console]::In.ReadToEnd(); [Console]::Out.Write('eof-marker'); [Console]::Out.Flush(); [Console]::Error.Write('err-marker'); [Console]::Error.Flush(); Start-Sleep -Seconds 30".into()],
            env: BTreeMap::from([(String::from("SystemRoot"), std::env::var("SystemRoot").unwrap())]),
        }).unwrap()).unwrap();
        let request_id = launch.request_id();
        let (stdout, stderr, stdout_moved_once, stderr_moved_once, stdin_preserved) = {
            let (_, pipes) = launch.parts_mut();
            let stdout = pipes.take_stdout().expect("stdout moves once");
            let stdout_moved_once = pipes.take_stdout().is_none();
            let stdin_after_stdout_move = pipes.stdin_mut().is_some();
            let stderr = pipes.take_stderr().expect("stderr moves once");
            let stderr_moved_once = pipes.take_stderr().is_none();
            let stdin_after_stderr_move = pipes.stdin_mut().is_some();
            (
                stdout,
                stderr,
                stdout_moved_once,
                stderr_moved_once,
                stdin_after_stdout_move && stdin_after_stderr_move,
            )
        };
        let stdin_unavailable = {
            let (_, pipes) = launch.parts_mut();
            pipes.close_stdin();
            pipes.close_stdin();
            pipes.stdin_mut().is_none()
        };
        let (out_tx, out_rx) = mpsc::channel();
        let (err_tx, err_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let (eof_tx, eof_rx) = mpsc::channel();
        let stdout_reader = thread::spawn(move || {
            let mut stdout = stdout;
            let mut bytes = Vec::new();
            let result = stdout
                .by_ref()
                .take(10)
                .read_to_end(&mut bytes)
                .map(|_| bytes);
            let _ = out_tx.send(result);
            let _ = resume_rx.recv();
            let mut byte = [0];
            let _ = eof_tx.send(stdout.read(&mut byte));
        });
        let stderr_reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stderr.take(10).read_to_end(&mut bytes).map(|_| bytes);
            let _ = err_tx.send(result);
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let remaining = || deadline.saturating_duration_since(std::time::Instant::now());
        let out = out_rx
            .recv_timeout(remaining())
            .expect("stdout marker deadline")
            .expect("stdout read");
        let err = err_rx
            .recv_timeout(remaining())
            .expect("stderr marker deadline")
            .expect("stderr read");
        let observed = launch.target_mut().observe_exit();
        reap(&mut launch);
        let observation = observed.expect("observe child before cleanup");
        drop(launch);
        resume_tx.send(()).expect("resume stdout EOF check");
        let stdout_eof = eof_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("moved stdout EOF deadline")
            .expect("moved stdout EOF read");
        stdout_reader.join().expect("stdout reader join");
        stderr_reader.join().expect("stderr reader join");
        assert_eq!(request_id, 20);
        assert!(stdout_moved_once, "stdout moved twice");
        assert!(stderr_moved_once, "stderr moved twice");
        assert!(stdin_preserved, "moving a pipe closed stdin");
        assert!(stdin_unavailable, "closed stdin remained available");
        assert_eq!(observation, ExitObservation::Running);
        assert_eq!(&out, b"eof-marker");
        assert_eq!(&err, b"err-marker");
        assert_eq!(stdout_eof, 0);
    }

    #[test]
    fn windows_launch_keeps_job_authority_after_leader_exit() {
        let cwd = std::env::temp_dir();
        let mut launch = OwnedLaunch::start(
            LaunchIntent::from_request(descendant_request(&cwd)).expect("intent"),
        )
        .expect("owned launch");
        let stdout = launch
            .parts_mut()
            .1
            .take_stdout()
            .expect("stdout moves once");
        let stderr = launch
            .parts_mut()
            .1
            .take_stderr()
            .expect("stderr moves once");
        let mut leader = None;
        let mut tree = None;
        let (ready, error) =
            read_pair_then_cleanup(stdout, stderr, 5, 5, Duration::from_secs(10), || {
                let lifecycle = (|| {
                    let deadline = std::time::Instant::now() + Duration::from_secs(10);
                    let exited = loop {
                        match launch.target_mut().observe_exit()? {
                            exited @ ExitObservation::Exited { .. } => break exited,
                            ExitObservation::Running if std::time::Instant::now() < deadline => {
                                std::thread::sleep(Duration::from_millis(25));
                            }
                            ExitObservation::Running => {
                                return Err(io::Error::new(
                                    io::ErrorKind::TimedOut,
                                    "leader did not exit before deadline",
                                ));
                            }
                        }
                    };
                    let tree = launch.target_mut().reap_step()?;
                    Ok((exited, tree))
                })();
                reap(&mut launch);
                let (exited, observed_tree) = lifecycle?;
                leader = Some(exited);
                tree = Some(observed_tree);
                Ok(())
            })
            .expect("bounded ready read and Job cleanup");
        assert_eq!(ready, b"ready");
        assert_eq!(error, b"error");
        assert!(matches!(leader, Some(ExitObservation::Exited { .. })));
        assert_eq!(tree, Some(TreeObservation::Present));
    }

    #[test]
    fn windows_missing_executable_is_static_and_redacted() {
        let missing = std::env::temp_dir().join("agent24-sidecar-program-that-does-not-exist.exe");
        let error = OwnedLaunch::start(
            LaunchIntent::from_request(Request::Launch {
                version: 1,
                request_id: 19,
                executable: missing.display().to_string(),
                cwd: std::env::temp_dir().display().to_string(),
                argv: Vec::new(),
                env: BTreeMap::new(),
            })
            .expect("intent"),
        )
        .err()
        .expect("missing executable must fail");
        assert!(matches!(error, LaunchFailure::Start(_)));
        assert!(!format!("{error:?}").contains(&missing.display().to_string()));
    }
}

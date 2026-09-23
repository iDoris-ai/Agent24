use std::{ffi::OsString, fmt, io, path::PathBuf};

use crate::{
    actor::Phase,
    cleanup::{CleanupStepError, cleanup_step},
    launch_order::LaunchControl,
    pipe_access::TargetPipes,
    target::{ExitObservation, TreeObservation},
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
use tokio::process::Command;

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

pub(crate) struct CleanupOwner(OwnedTarget);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LaunchCleanupError {
    Stop(io::ErrorKind),
    Observe(io::ErrorKind),
    Reap(io::ErrorKind),
    InvalidPhase,
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
    /// The failed pipe transfer retains containment, but exposes only cleanup.
    pub(crate) fn cleanup_tick(
        &mut self,
        phase: &mut Phase,
    ) -> Option<Result<TreeObservation, LaunchCleanupError>> {
        match self {
            Self::Pipes { cleanup, .. } => Some(cleanup.tick(phase)),
            Self::NotLaunch | Self::Start(_) => None,
        }
    }
}

impl CleanupOwner {
    fn tick(&mut self, phase: &mut Phase) -> Result<TreeObservation, LaunchCleanupError> {
        if matches!(phase, Phase::GracefulStopping(_)) {
            match self
                .0
                .observe_exit()
                .map_err(|error| LaunchCleanupError::Observe(error.kind()))?
            {
                ExitObservation::Running => return Ok(TreeObservation::Present),
                ExitObservation::Exited { .. } => {}
            }
        }
        if matches!(
            phase,
            Phase::GracefulStopping(_)
                | Phase::ForceStopping(_)
                | Phase::Draining(_)
                | Phase::Unconfirmed
        ) {
            self.0
                .request_stop(true)
                .map_err(|error| LaunchCleanupError::Stop(error.kind()))?;
        }
        cleanup_step(phase, &mut self.0).map_err(|error| match error {
            CleanupStepError::Reap(error) => LaunchCleanupError::Reap(error.kind()),
            CleanupStepError::Phase(_) => LaunchCleanupError::InvalidPhase,
        })
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
            cleanup: Box::new(CleanupOwner(target)),
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
        let mut command = Command::new(intent.executable);
        command
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
}

#[cfg(all(test, windows))]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod windows_tests {
    use super::*;
    use crate::target::{ExitObservation, TreeObservation};
    use std::{collections::BTreeMap, path::Path, time::Duration};
    use tokio::io::AsyncReadExt;

    fn request(cwd: &Path) -> Request {
        let system_root = std::env::var("SystemRoot").expect("SystemRoot");
        Request::Launch {
            version: 1,
            request_id: 17,
            executable: String::from("powershell.exe"),
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
            argv[3] = String::from("-Command");
            argv[4] = String::from(
                "$child = Start-Process \"$PSHOME\\powershell.exe\" -ArgumentList '-NoLogo','-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 30' -PassThru; [Console]::Out.Write('ready'); [Console]::Out.Flush()",
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

    #[tokio::test]
    async fn windows_launch_preserves_request_and_all_pipes() {
        let parent_current_dir = std::env::current_dir().expect("current cwd");
        let mut cwd = std::env::temp_dir();
        cwd.push(format!("agent24-sidecar-cwd-{}", std::process::id()));
        std::fs::create_dir_all(&cwd).expect("cwd");
        std::fs::write(cwd.join("cwd-sentinel"), b"").expect("sentinel");
        std::fs::write(
            cwd.join("launch.ps1"),
            b"param($first,$second)\n$inherited = if ($env:PATH) {$env:PATH} else {'unset'}\n$cwd=if(Test-Path -LiteralPath 'cwd-sentinel') {'cwd-ok'} else {'cwd-bad'}\n$out=\"$cwd|$env:SIDE|$first,$second|$inherited\"\n[Console]::Out.Write($out)\n[Console]::Out.Flush()\n[Console]::Error.Write('err')\n[Console]::Error.Flush()",
        )
        .expect("script");
        assert_ne!(cwd, parent_current_dir);
        assert!(!parent_current_dir.join("cwd-sentinel").exists());
        assert!(std::env::var_os("PATH").is_some());
        let mut launch =
            OwnedLaunch::start(LaunchIntent::from_request(request(&cwd)).expect("intent"))
                .expect("owned launch");
        assert_eq!(launch.request_id(), 17);
        let mut stdout = String::new();
        let (mut stdout_pipe, mut stderr_pipe) = {
            let (_, pipes) = launch.parts_mut();
            let _ = pipes.stdin_mut().expect("stdin");
            (
                pipes.take_stdout().expect("stdout moves once"),
                pipes.take_stderr().expect("stderr moves once"),
            )
        };
        stdout_pipe.read_to_string(&mut stdout).await.unwrap();
        let mut stderr = String::new();
        stderr_pipe.read_to_string(&mut stderr).await.unwrap();
        let fields: Vec<_> = stdout.split('|').collect();
        assert_eq!(fields[0], "cwd-ok");
        assert_eq!(
            &fields[1..],
            ["env-value", "ignored-zero,argv-value", "unset"]
        );
        assert_eq!(stderr, "err");
        reap(&mut launch);
        std::fs::remove_dir_all(&cwd).expect("cleanup cwd");
    }

    #[tokio::test]
    async fn windows_moved_pipes_deliver_eof_and_leave_the_launch_authoritative() {
        let cwd = std::env::temp_dir();
        let mut launch = OwnedLaunch::start(LaunchIntent::from_request(Request::Launch {
            version: 1,
            request_id: 20,
            executable: "powershell.exe".into(),
            cwd: cwd.display().to_string(),
            argv: vec!["-NoLogo".into(), "-NoProfile".into(), "-NonInteractive".into(), "-Command".into(),
                "$null = [Console]::In.ReadToEnd(); [Console]::Out.Write('eof-marker'); [Console]::Out.Flush(); [Console]::Error.Write('err-marker'); [Console]::Error.Flush(); Start-Sleep -Seconds 30".into()],
            env: BTreeMap::from([(String::from("SystemRoot"), std::env::var("SystemRoot").unwrap())]),
        }).unwrap()).unwrap();
        assert_eq!(launch.request_id(), 20);
        let (mut stdout, mut stderr) = {
            let (_, pipes) = launch.parts_mut();
            let stdout = pipes.take_stdout().expect("stdout moves once");
            assert!(pipes.take_stdout().is_none(), "stdout moved twice");
            assert!(pipes.stdin_mut().is_some(), "moving stdout closed stdin");
            let stderr = pipes.take_stderr().expect("stderr moves once");
            assert!(pipes.take_stderr().is_none(), "stderr moved twice");
            assert!(pipes.stdin_mut().is_some(), "moving stderr closed stdin");
            (stdout, stderr)
        };
        let stdin_unavailable = {
            let (_, pipes) = launch.parts_mut();
            pipes.close_stdin();
            pipes.close_stdin();
            pipes.stdin_mut().is_none()
        };
        let mut out = [0; 10];
        let stdout_result =
            tokio::time::timeout(Duration::from_secs(3), stdout.read_exact(&mut out)).await;
        let mut err = [0; 10];
        let stderr_result =
            tokio::time::timeout(Duration::from_secs(3), stderr.read_exact(&mut err)).await;
        assert!(matches!(
            launch.target_mut().observe_exit().expect("observe owner"),
            ExitObservation::Running
        ));
        reap(&mut launch);
        drop(launch);
        assert!(stdin_unavailable, "closed stdin remained available");
        assert!(
            stdout_result.is_ok_and(|result| result.is_ok()),
            "stdout EOF marker timed out or failed"
        );
        assert!(
            stderr_result.is_ok_and(|result| result.is_ok()),
            "stderr marker timed out or failed"
        );
        assert_eq!(&out, b"eof-marker");
        assert_eq!(&err, b"err-marker");
        let mut eof = [0];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), stdout.read(&mut eof))
                .await
                .expect("moved stdout stays open")
                .expect("moved stdout read"),
            0
        );
    }

    #[tokio::test]
    async fn windows_launch_keeps_job_authority_after_leader_exit() {
        let cwd = std::env::temp_dir();
        let mut launch = OwnedLaunch::start(
            LaunchIntent::from_request(descendant_request(&cwd)).expect("intent"),
        )
        .expect("owned launch");
        let mut ready = [0; 5];
        let mut stdout = launch
            .parts_mut()
            .1
            .take_stdout()
            .expect("stdout moves once");
        stdout.read_exact(&mut ready).await.expect("ready");
        assert_eq!(&ready, b"ready");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            match launch.target_mut().observe_exit().expect("observe leader") {
                ExitObservation::Exited { .. } => break,
                ExitObservation::Running if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                ExitObservation::Running => panic!("leader did not exit before deadline"),
            }
        }
        assert_eq!(
            launch.target_mut().reap_step().expect("observe Job"),
            TreeObservation::Present
        );
        reap(&mut launch);
    }

    #[test]
    fn windows_missing_executable_is_static_and_redacted() {
        let missing = "agent24-sidecar-program-that-does-not-exist.exe";
        let error = OwnedLaunch::start(
            LaunchIntent::from_request(Request::Launch {
                version: 1,
                request_id: 19,
                executable: missing.to_owned(),
                cwd: std::env::temp_dir().display().to_string(),
                argv: Vec::new(),
                env: BTreeMap::new(),
            })
            .expect("intent"),
        )
        .err()
        .expect("missing executable must fail");
        assert!(matches!(error, LaunchFailure::Start(_)));
        assert!(!format!("{error:?}").contains(missing));
    }
}

use std::{ffi::OsString, fmt, io, path::PathBuf};

use crate::pipe_access::TargetPipes;
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
        target: Box<OwnedTarget>,
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
    pub(crate) fn target_mut(&mut self) -> Option<&mut OwnedTarget> {
        match self {
            Self::Pipes { target, .. } => Some(target),
            Self::NotLaunch | Self::Start(_) => None,
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
            target: Box::new(target),
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

    pub(crate) fn target_mut(&mut self) -> &mut OwnedTarget {
        &mut self.target
    }

    pub(crate) fn parts_mut(&mut self) -> (&mut OwnedTarget, &mut TargetPipes) {
        (&mut self.target, &mut self.pipes)
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

    pub(crate) fn target_mut(&mut self) -> &mut OwnedTarget {
        &mut self.target
    }

    pub(crate) fn parts_mut(&mut self) -> (&mut OwnedTarget, &mut TargetPipes) {
        (&mut self.target, &mut self.pipes)
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
        let deadline = Instant::now() + Duration::from_secs(2);
        while !matches!(
            launch.parts_mut().0.reap_step().expect("reap step"),
            TreeObservation::ConfirmedEmpty
        ) {
            assert!(Instant::now() < deadline, "child was not reaped");
        }
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
        {
            let (_, pipes) = launch.parts_mut();
            let _ = pipes.stdin_mut().expect("stdin");
            pipes
                .stdout_mut()
                .read_exact(&mut stdout_text[..1])
                .expect("first stdout borrow");
        }
        {
            let (_, pipes) = launch.parts_mut();
            pipes
                .stdout_mut()
                .read_exact(&mut stdout_text[1..])
                .expect("second stdout borrow");
            let _ = pipes.stderr_mut();
        }
        assert_eq!(stdout_text, *b"/|env-value|argv-value|unset");
        reap(&mut launch);
        drop(launch);
        wait_for_reaper_idle();
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
        {
            let (_, pipes) = launch.parts_mut();
            let _ = pipes.stdin_mut().expect("stdin");
            let mut first = [0; 1];
            pipes.stdout_mut().read_exact(&mut first).await.unwrap();
            stdout.push(first[0] as char);
        }
        {
            let (_, pipes) = launch.parts_mut();
            pipes
                .stdout_mut()
                .read_to_string(&mut stdout)
                .await
                .unwrap();
        }
        let mut stderr = String::new();
        launch
            .parts_mut()
            .1
            .stderr_mut()
            .read_to_string(&mut stderr)
            .await
            .unwrap();
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
    async fn windows_launch_keeps_job_authority_after_leader_exit() {
        let cwd = std::env::temp_dir();
        let mut launch = OwnedLaunch::start(
            LaunchIntent::from_request(descendant_request(&cwd)).expect("intent"),
        )
        .expect("owned launch");
        let mut ready = [0; 5];
        launch
            .parts_mut()
            .1
            .stdout_mut()
            .read_exact(&mut ready)
            .await
            .expect("ready");
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

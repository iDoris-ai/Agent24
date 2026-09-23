use std::{ffi::OsString, fmt, io, path::PathBuf};

use agent24_sidecar_host_protocol::Request;

#[cfg(unix)]
use crate::{
    posix::{LaunchSpec, OwnedGeneration},
    target::{OwnedPipes, OwnedTarget},
};

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

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LaunchFailure {
    NotLaunch,
    Start(io::ErrorKind),
    Pipes(io::ErrorKind),
}

impl fmt::Display for LaunchFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("launch failed")
    }
}

#[cfg(unix)]
pub(crate) struct OwnedLaunch {
    request_id: u64,
    target: OwnedTarget,
    pipes: OwnedPipes,
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
        let mut target = OwnedTarget::from_owned(owner);
        let pipes = target
            .take_pipes()
            .map_err(|error| LaunchFailure::Pipes(error.kind()))?;
        Ok(Self {
            request_id,
            target,
            pipes,
        })
    }

    pub(crate) const fn request_id(&self) -> u64 {
        self.request_id
    }

    pub(crate) fn target_mut(&mut self) -> &mut OwnedTarget {
        &mut self.target
    }

    pub(crate) fn pipes_mut(&mut self) -> &mut OwnedPipes {
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
        launch.target_mut().request_stop(true).expect("force stop");
        let deadline = Instant::now() + Duration::from_secs(2);
        while !matches!(
            launch.target_mut().reap_step().expect("reap step"),
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
        let pipes = launch.pipes_mut();
        let _ = (&pipes.stdin, &pipes.stderr);
        let mut stdout_text = [0; 28];
        pipes.stdout.read_exact(&mut stdout_text).expect("stdout");
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
}

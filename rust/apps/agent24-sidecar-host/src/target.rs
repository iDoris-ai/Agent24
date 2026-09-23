use std::io;

#[cfg(unix)]
pub(crate) type PlatformOwner = crate::posix::OwnedGeneration;
#[cfg(windows)]
pub(crate) type PlatformOwner = crate::owner::OwnedProcess;

#[cfg(windows)]
pub(crate) use crate::owner::OwnedPipes;
#[cfg(unix)]
pub(crate) use crate::posix::OwnedPipes;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExitObservation {
    Running,
    Exited { code: Option<i32> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TreeObservation {
    Present,
    ConfirmedEmpty,
    Unconfirmed,
}

/// The common lifecycle boundary keeps its native owner private.
pub(crate) struct OwnedTarget {
    owner: PlatformOwner,
    confirmed_empty: bool,
}

impl OwnedTarget {
    pub(crate) fn from_owned(owner: PlatformOwner) -> Self {
        Self {
            owner,
            confirmed_empty: false,
        }
    }

    pub(crate) fn take_pipes(&mut self) -> io::Result<OwnedPipes> {
        self.owner.take_pipes()
    }

    pub(crate) fn request_stop(&mut self, force: bool) -> io::Result<()> {
        self.request_stop_with(force, |owner, force| {
            if force {
                return owner.force_kill();
            }
            #[cfg(unix)]
            {
                owner.terminate()
            }
            #[cfg(windows)]
            {
                Ok(())
            }
        })
    }

    pub(crate) fn observe_exit(&mut self) -> io::Result<ExitObservation> {
        #[cfg(unix)]
        {
            self.owner.observe_exit()
        }
        #[cfg(windows)]
        {
            self.owner.observe_exit().map(|status| match status {
                Some(status) => ExitObservation::Exited {
                    code: status.code(),
                },
                None => ExitObservation::Running,
            })
        }
    }

    pub(crate) fn reap_step(&mut self) -> io::Result<TreeObservation> {
        self.reap_step_with(|owner| owner.reap_step())
    }

    fn request_stop_with<F>(&mut self, force: bool, stop: F) -> io::Result<()>
    where
        F: FnOnce(&mut PlatformOwner, bool) -> io::Result<()>,
    {
        if self.confirmed_empty {
            return Ok(());
        }
        stop(&mut self.owner, force)
    }

    fn reap_step_with<F>(&mut self, reap: F) -> io::Result<TreeObservation>
    where
        F: FnOnce(&mut PlatformOwner) -> io::Result<TreeObservation>,
    {
        if self.confirmed_empty {
            return Ok(TreeObservation::ConfirmedEmpty);
        }
        let observation = reap(&mut self.owner)?;
        if observation == TreeObservation::ConfirmedEmpty {
            self.confirmed_empty = true;
        }
        Ok(observation)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn unix_contract_transfers_pipes_once_and_stops_the_same_owner() {
        use crate::posix::{LaunchSpec, tests::test_lock, tests::wait_for_reaper_idle};
        use std::io::{Read, Write};

        let _test_guard = test_lock();
        let owner = PlatformOwner::launch(LaunchSpec::new("/bin/sh", "/").arg("-c").arg(
            "trap '' TERM; read line; printf 'out:%s' \"$line\"; \
                 printf 'err:%s' \"$line\" >&2; exec /bin/sleep 30",
        ))
        .expect("spawn /bin/sh");
        let mut target = OwnedTarget::from_owned(owner);
        let pipes = target.take_pipes().expect("owned pipes");
        assert!(target.take_pipes().is_err());
        let OwnedPipes {
            mut stdin,
            stdout,
            stderr,
        } = pipes;
        stdin.write_all(b"hello\n").expect("write stdin");
        drop(stdin);
        let mut stdout_text = String::new();
        let mut stderr_text = String::new();
        stdout
            .take(9)
            .read_to_string(&mut stdout_text)
            .expect("read stdout");
        stderr
            .take(9)
            .read_to_string(&mut stderr_text)
            .expect("read stderr");
        assert_eq!(stdout_text, "out:hello");
        assert_eq!(stderr_text, "err:hello");
        target.request_stop(false).expect("graceful stop");
        target.request_stop(true).expect("force stop");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            match target.reap_step().expect("reap stopped target") {
                TreeObservation::ConfirmedEmpty => break,
                TreeObservation::Present if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                observation => panic!("target did not become empty: {observation:?}"),
            }
        }
        drop(target);
        wait_for_reaper_idle();
    }

    #[cfg(unix)]
    #[test]
    fn unix_contract_observes_exit_without_consuming_the_owner() {
        use crate::posix::{LaunchSpec, tests::test_lock, tests::wait_for_reaper_idle};
        use std::time::Duration;

        let _test_guard = test_lock();
        let owner = PlatformOwner::launch(LaunchSpec::new("/bin/sh", "/").arg("-c").arg("exit 7"))
            .expect("spawn /bin/sh");
        let mut target = OwnedTarget::from_owned(owner);
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            match target.observe_exit().expect("observe exit") {
                ExitObservation::Exited { code } => {
                    assert_eq!(code, Some(7));
                    break;
                }
                ExitObservation::Running if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                ExitObservation::Running => panic!("exit was not observed before deadline"),
            }
        }
        assert_eq!(
            target.observe_exit().expect("repeat observation"),
            ExitObservation::Exited { code: Some(7) }
        );
        drop(target);
        wait_for_reaper_idle();
    }

    #[cfg(unix)]
    #[test]
    fn unix_contract_reaps_through_the_common_owner_boundary() {
        use crate::posix::{LaunchSpec, tests::test_lock, tests::wait_for_reaper_idle};
        use std::time::{Duration, Instant};

        let _test_guard = test_lock();
        let owner =
            PlatformOwner::launch(LaunchSpec::new("/bin/sh", "/").arg("-c").arg("sleep 30"))
                .expect("spawn /bin/sh");
        let mut target = OwnedTarget::from_owned(owner);
        target.request_stop(true).expect("force stop");
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match target.reap_step().expect("reap step") {
                TreeObservation::ConfirmedEmpty => break,
                TreeObservation::Present if Instant::now() < deadline => {}
                TreeObservation::Present => panic!("process group was not reaped before deadline"),
                TreeObservation::Unconfirmed => panic!("POSIX reap must not be unconfirmed"),
            }
        }
        drop(target);
        wait_for_reaper_idle();
    }

    #[cfg(unix)]
    #[test]
    fn confirmed_empty_is_a_common_tombstone_for_stop_and_reap() {
        use crate::posix::{LaunchSpec, tests::test_lock, tests::wait_for_reaper_idle};

        let _test_guard = test_lock();
        let owner = PlatformOwner::launch(LaunchSpec::new("/bin/sh", "/").arg("-c").arg("true"))
            .expect("spawn /bin/sh");
        let mut target = OwnedTarget::from_owned(owner);
        let mut reap_calls = 0;
        assert_eq!(
            target
                .reap_step_with(|_| {
                    reap_calls += 1;
                    Ok(TreeObservation::ConfirmedEmpty)
                })
                .expect("initial reap"),
            TreeObservation::ConfirmedEmpty
        );
        let mut stop_calls = 0;
        target
            .request_stop_with(false, |_, _| {
                stop_calls += 1;
                Err(io::Error::other("native stop must be bypassed"))
            })
            .expect("soft stop after tombstone");
        target
            .request_stop_with(true, |_, _| {
                stop_calls += 1;
                Err(io::Error::other("native stop must be bypassed"))
            })
            .expect("force stop after tombstone");
        assert_eq!(
            target
                .reap_step_with(|_| {
                    reap_calls += 1;
                    Err(io::Error::other("native reap must be bypassed"))
                })
                .expect("repeat reap"),
            TreeObservation::ConfirmedEmpty
        );
        assert_eq!(reap_calls, 1);
        assert_eq!(stop_calls, 0);
        drop(target);
        wait_for_reaper_idle();
    }

    #[cfg(unix)]
    #[test]
    fn present_and_error_do_not_latch_confirmed_empty() {
        use crate::posix::{LaunchSpec, tests::test_lock, tests::wait_for_reaper_idle};

        let _test_guard = test_lock();
        let owner = PlatformOwner::launch(LaunchSpec::new("/bin/sh", "/").arg("-c").arg("true"))
            .expect("spawn /bin/sh");
        let mut target = OwnedTarget::from_owned(owner);
        let mut reap_calls = 0;
        assert_eq!(
            target
                .reap_step_with(|_| {
                    reap_calls += 1;
                    Ok(TreeObservation::Present)
                })
                .expect("present reap"),
            TreeObservation::Present
        );
        assert!(
            target
                .reap_step_with(|_| {
                    reap_calls += 1;
                    Err(io::Error::other("probe failed"))
                })
                .is_err()
        );
        assert_eq!(
            target
                .reap_step_with(|_| {
                    reap_calls += 1;
                    Ok(TreeObservation::ConfirmedEmpty)
                })
                .expect("confirm reap"),
            TreeObservation::ConfirmedEmpty
        );
        assert_eq!(reap_calls, 3);
        drop(target);
        wait_for_reaper_idle();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_contract_transfers_pipes_once_and_stops_the_same_owner() {
        use std::time::Duration;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::process::Command;

        let generation = crate::owner::GenerationId::new(7).expect("generation");
        let owner = crate::owner::GenerationOwner::new(generation).expect("Job Object");
        let mut command = Command::new("powershell.exe");
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$line=[Console]::In.ReadLine(); [Console]::Out.Write(\"out:$line\"); [Console]::Out.Flush(); [Console]::Error.Write(\"err:$line\"); [Console]::Error.Flush(); Start-Sleep -Seconds 30",
        ]);
        let process = owner.spawn(command).expect("spawn process");
        let mut target = OwnedTarget::from_owned(process);
        let pipes = target.take_pipes().expect("owned pipes");
        assert!(target.take_pipes().is_err());
        let OwnedPipes {
            mut stdin,
            stdout,
            stderr,
        } = pipes;
        let (stdout_text, stderr_text) = tokio::time::timeout(Duration::from_secs(10), async {
            stdin.write_all(b"hello\n").await.expect("write stdin");
            drop(stdin);
            let mut stdout_text = String::new();
            let mut stderr_text = String::new();
            stdout
                .take(9)
                .read_to_string(&mut stdout_text)
                .await
                .expect("read stdout");
            stderr
                .take(9)
                .read_to_string(&mut stderr_text)
                .await
                .expect("read stderr");
            (stdout_text, stderr_text)
        })
        .await
        .expect("pipe roundtrip deadline");
        assert_eq!(stdout_text, "out:hello");
        assert_eq!(stderr_text, "err:hello");
        target.request_stop(false).expect("graceful stop");
        assert!(
            target
                .owner
                .observe_exit()
                .expect("observe process")
                .is_none(),
            "Windows soft stop must not kill the target"
        );
        target.request_stop(true).expect("force stop");
        drop(target);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_contract_reaps_through_the_common_owner_boundary() {
        use std::time::{Duration, Instant};
        use tokio::process::Command;

        let owner = crate::owner::GenerationOwner::new(
            crate::owner::GenerationId::new(8).expect("generation"),
        )
        .expect("Job Object");
        let mut command = Command::new("cmd.exe");
        command.args(["/C", "exit", "0"]);
        let process = owner.spawn(command).expect("spawn process");
        let mut target = OwnedTarget::from_owned(process);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match target.reap_step().expect("reap step") {
                TreeObservation::ConfirmedEmpty => break,
                TreeObservation::Present if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                TreeObservation::Present => panic!("Job tree was not reaped before deadline"),
                TreeObservation::Unconfirmed => panic!("Windows reap must not be unconfirmed"),
            }
        }
    }
}

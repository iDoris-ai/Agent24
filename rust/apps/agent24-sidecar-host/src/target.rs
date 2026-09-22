use std::io;

#[cfg(unix)]
pub(crate) type PlatformOwner = crate::posix::OwnedGeneration;
#[cfg(windows)]
pub(crate) type PlatformOwner = crate::owner::OwnedProcess;

#[cfg(windows)]
pub(crate) use crate::owner::OwnedPipes;
#[cfg(unix)]
pub(crate) use crate::posix::OwnedPipes;

/// The common lifecycle boundary keeps its native owner private.
pub(crate) struct OwnedTarget {
    owner: PlatformOwner,
}

impl OwnedTarget {
    pub(crate) fn from_owned(owner: PlatformOwner) -> Self {
        Self { owner }
    }

    pub(crate) fn take_pipes(&mut self) -> io::Result<OwnedPipes> {
        self.owner.take_pipes()
    }

    pub(crate) fn request_stop(&mut self, force: bool) -> io::Result<()> {
        if force {
            return self.owner.force_kill();
        }
        #[cfg(unix)]
        {
            self.owner.terminate()
        }
        #[cfg(windows)]
        {
            Ok(())
        }
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
        let owner =
            PlatformOwner::launch(LaunchSpec::new("/bin/sh", "/").arg("-c").arg(
                "read line; printf 'out:%s' \"$line\"; printf 'err:%s' \"$line\" >&2; sleep 30",
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
            mut stdout,
            mut stderr,
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
}

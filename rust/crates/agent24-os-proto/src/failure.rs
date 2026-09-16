//! Why a supervised run failed, in one of five kinds (FU-57; semantics in
//! `docs/design/FU-57-run-failure-kinds.md`).
//!
//! A kind names the step the supervisor saw fail first — not a diagnosis of
//! the cause; the original message always travels with it. Classification is
//! by *stage*, not by error type: the same `EndpointError::Io` is `setup` when
//! preparing the callback socket and `io` when waiting for the module on it.
//! So there is one exhaustive classifier per stage, with no wildcard arm: a new
//! error variant does not compile until someone decides its kind.

use crate::endpoint::{EndpointError, HandshakeFailed};
use crate::frame::FrameError;
use crate::initialize::HandshakeError;
use crate::launch::LaunchError;
use crate::rpc::Ended;
use crate::supervise::{Exit, Stopped};

/// The longest `detail` kept, in bytes. A handshake diagnostic can quote a
/// frame of up to a mebibyte, and the failure is cloned into every status
/// published and into API responses.
pub const MAX_DETAIL_BYTES: usize = 512;

/// Which step of a run failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// The process was never born: the kernel's own preparation failed (the
    /// module's port, the callback socket, resolving or starting the command).
    Setup,
    /// The module and the kernel disagree about the protocol: refused at the
    /// handshake, a frame over the limit, or a peer that is another user.
    Refused,
    /// No handshake before the startup deadline.
    Timeout,
    /// The callback channel broke or ended while the process had not been
    /// seen to exit — or whether it had could not be found out.
    Io,
    /// The process exited by itself.
    Exited,
}

impl FailureKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Refused => "refused",
            Self::Timeout => "timeout",
            Self::Io => "io",
            Self::Exited => "exited",
        }
    }

    /// The coarse signal [`crate::supervise::RestartPolicy::failed`] takes.
    /// The policy does not look at it; `Exited` there no longer means the
    /// process exited.
    #[must_use]
    pub const fn legacy(self) -> Stopped {
        match self {
            Self::Timeout => Stopped::StartupTimeout,
            Self::Setup | Self::Refused | Self::Io | Self::Exited => Stopped::Exited,
        }
    }
}

impl std::fmt::Display for FailureKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One failed run: its kind, and what was said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunFailure {
    pub kind: FailureKind,
    /// The original message: at most [`MAX_DETAIL_BYTES`] bytes total,
    /// including the `…` this adds when it cuts.
    pub detail: String,
}

impl RunFailure {
    #[must_use]
    pub fn new(kind: FailureKind, detail: impl std::fmt::Display) -> Self {
        let mut detail = detail.to_string();
        if detail.len() > MAX_DETAIL_BYTES {
            // Leave room for the ellipsis so the total never exceeds the cap
            // (review of FU-57, round 1).
            let mut cut = MAX_DETAIL_BYTES - '…'.len_utf8();
            while !detail.is_char_boundary(cut) {
                cut -= 1;
            }
            detail.truncate(cut);
            detail.push('…');
        }
        Self { kind, detail }
    }
}

impl std::fmt::Display for RunFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.kind, self.detail)
    }
}

/// Opening this generation's sockets (`CallbackDir::open_generation` — FU-60
/// folded the old separate TCP-port bind into this same step, so `bind` above
/// this comment used to exist as its own function and no longer does; nothing
/// else called it). One call covers both the callback socket and the
/// module's inbound listener, and from the `Result` alone a caller cannot
/// tell which of the two failed — so the wording stays neutral, not
/// "callback" (design v4, round 2: `open_generation` exposes one
/// `Result<_, EndpointError>` for a pair).
#[must_use]
pub fn listen(e: &EndpointError) -> RunFailure {
    let kind = match e {
        EndpointError::UnsafeDirectory { .. }
        | EndpointError::PathTooLong(_)
        | EndpointError::Io(_)
        // Not returned by `open_generation`; still the kernel's own
        // preparation, and there is no other stage to attribute it to.
        | EndpointError::Timeout
        | EndpointError::ForeignPeer { .. }
        | EndpointError::AlreadyCreated(_) => FailureKind::Setup,
    };
    RunFailure::new(kind, format!("could not open the module's sockets: {e}"))
}

/// Starting the module's process.
#[must_use]
pub fn launch(e: &LaunchError) -> RunFailure {
    let kind = match e {
        LaunchError::Unresolved(_)
        | LaunchError::EscapesPackage { .. }
        | LaunchError::Spawn(_)
        | LaunchError::NoEntropy(_)
        | LaunchError::UnsafeOwnership(_)
        | LaunchError::Inspect { .. }
        | LaunchError::TooLarge { .. } => FailureKind::Setup,
    };
    RunFailure::new(kind, format!("could not start the module: {e}"))
}

/// Waiting for the module to connect (`CallbackListener::accept_one`).
#[must_use]
pub fn accept(e: &EndpointError) -> RunFailure {
    let kind = match e {
        EndpointError::Timeout => FailureKind::Timeout,
        EndpointError::ForeignPeer { .. } => FailureKind::Refused,
        EndpointError::Io(_)
        // Not returned by `accept_one`: set up before the run.
        | EndpointError::UnsafeDirectory { .. }
        | EndpointError::PathTooLong(_)
        | EndpointError::AlreadyCreated(_) => FailureKind::Io,
    };
    RunFailure::new(kind, format!("the module did not connect: {e}"))
}

/// The handshake on an accepted connection.
#[must_use]
pub fn handshake(e: &HandshakeFailed) -> RunFailure {
    let kind = match e {
        HandshakeFailed::Timeout => FailureKind::Timeout,
        HandshakeFailed::Refused(refusal) => match refusal {
            HandshakeError::Parse(_)
            | HandshakeError::NotInitialize(_)
            | HandshakeError::BadParams(_)
            | HandshakeError::AuthFailed
            | HandshakeError::ManifestMismatch { .. }
            | HandshakeError::VersionMismatch(_) => FailureKind::Refused,
        },
        HandshakeFailed::Frame(frame) => match frame {
            FrameError::TooLong { .. } => FailureKind::Refused,
            FrameError::Eof | FrameError::Io(_) => FailureKind::Io,
        },
        HandshakeFailed::Write(_) => FailureKind::Io,
    };
    RunFailure::new(
        kind,
        format!("the module did not complete its handshake: {e}"),
    )
}

/// The callback connection of a running module ended.
#[must_use]
pub fn serve(ended: &Ended) -> RunFailure {
    let kind = match ended {
        Ended::TooLong => FailureKind::Refused,
        Ended::PeerClosed | Ended::ReadFailed(_) | Ended::WriteFailed(_) => FailureKind::Io,
        // Only a revocation fires it, and only this run revokes its
        // generation (by stopping it) — not expected here.
        Ended::Stopped => {
            return RunFailure::new(
                FailureKind::Io,
                "the callback connection was stopped by a revocation (not expected)",
            );
        }
    };
    RunFailure::new(kind, format!("the callback connection ended: {ended:?}"))
}

/// The process was seen to end — or asking whether it had failed.
#[must_use]
pub fn exit(exited: &std::io::Result<Exit>) -> RunFailure {
    match exited {
        Ok(exit) => RunFailure::new(FailureKind::Exited, format!("the module {exit}")),
        Err(e) => RunFailure::new(
            FailureKind::Io,
            format!("could not find out whether the module exited: {e}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::VersionMismatch;
    use FailureKind::{Exited, Io, Refused, Setup, Timeout};

    fn io_err() -> std::io::Error {
        std::io::Error::other("x")
    }

    fn endpoint_errors() -> Vec<EndpointError> {
        vec![
            EndpointError::UnsafeDirectory {
                path: "/d".into(),
                why: "w".into(),
            },
            EndpointError::PathTooLong("/p".into()),
            EndpointError::Timeout,
            EndpointError::ForeignPeer { uid: 1 },
            EndpointError::AlreadyCreated("/d".into()),
            EndpointError::Io(io_err()),
        ]
    }

    /// The same error, two stages, two kinds: `Io` preparing the socket is
    /// the kernel's setup; `Io` waiting on it is the channel.
    #[test]
    fn an_endpoint_error_is_classified_by_the_stage_it_came_from() {
        let listened: Vec<_> = endpoint_errors().iter().map(|e| listen(e).kind).collect();
        assert_eq!(listened, [Setup; 6]);
        let accepted: Vec<_> = endpoint_errors().iter().map(|e| accept(e).kind).collect();
        assert_eq!(accepted, [Io, Io, Timeout, Refused, Io, Io]);
    }

    /// FU-60: `open_generation` opens a pair — the callback socket and the
    /// module's inbound listener — from one call, so a caller cannot tell
    /// which one a bind-stage failure came from. `listen`'s own wording must
    /// not name either — checked on the variants `open_generation` can
    /// actually return for *either* socket's bind (`UnsafeDirectory`,
    /// `PathTooLong`, `Io`, `AlreadyCreated`); `Timeout`/`ForeignPeer` are
    /// genuinely callback-only (only the callback is ever accepted and
    /// handshaken here) and correctly still say so (design v4, round 2).
    #[test]
    fn listen_failures_say_neither_callback_nor_listener_for_shared_variants() {
        let shared = [
            EndpointError::UnsafeDirectory {
                path: "/d".into(),
                why: "w".into(),
            },
            EndpointError::PathTooLong("/p".into()),
            EndpointError::AlreadyCreated("/d".into()),
            EndpointError::Io(io_err()),
        ];
        for e in &shared {
            let text = listen(e).to_string();
            assert!(!text.contains("callback"), "{text}");
        }
    }

    #[test]
    fn every_launch_error_is_setup() {
        let all = [
            LaunchError::Unresolved("x".into()),
            LaunchError::EscapesPackage {
                resolved: "/a".into(),
                package: "/b".into(),
            },
            LaunchError::Spawn(io_err()),
            LaunchError::NoEntropy(io_err()),
            LaunchError::UnsafeOwnership("/a".into()),
            LaunchError::Inspect {
                path: "/a".into(),
                error: io_err(),
            },
            LaunchError::TooLarge {
                package: "/a".into(),
            },
        ];
        for e in &all {
            assert_eq!(launch(e).kind, Setup, "{e}");
        }
    }

    #[test]
    fn every_handshake_failure_has_its_kind() {
        let refusals = [
            HandshakeError::Parse("x".into()),
            HandshakeError::NotInitialize("x".into()),
            HandshakeError::BadParams("x".into()),
            HandshakeError::AuthFailed,
            HandshakeError::ManifestMismatch {
                expected: "a".into(),
                got: "b".into(),
            },
            HandshakeError::VersionMismatch(VersionMismatch::NotDeclared {
                kernel: crate::version::kernel_range(),
            }),
        ];
        for r in refusals {
            assert_eq!(handshake(&HandshakeFailed::Refused(r)).kind, Refused);
        }
        let rest = [
            (HandshakeFailed::Timeout, Timeout),
            (
                HandshakeFailed::Frame(FrameError::TooLong { limit: 1 }),
                Refused,
            ),
            (HandshakeFailed::Frame(FrameError::Eof), Io),
            (HandshakeFailed::Frame(FrameError::Io(io_err())), Io),
            (HandshakeFailed::Write(io_err()), Io),
        ];
        for (e, kind) in rest {
            assert_eq!(handshake(&e).kind, kind, "{e}");
        }
    }

    #[test]
    fn every_end_of_a_served_connection_has_its_kind() {
        let all = [
            (Ended::PeerClosed, Io),
            (Ended::TooLong, Refused),
            (Ended::ReadFailed(io_err()), Io),
            (Ended::WriteFailed(io_err()), Io),
            (Ended::Stopped, Io),
        ];
        for (e, kind) in all {
            assert_eq!(serve(&e).kind, kind, "{e:?}");
        }
    }

    /// Only a seen exit is `exited`; failing to find out is not one.
    #[test]
    fn only_an_exit_seen_is_exited() {
        let seen = exit(&Ok(Exit {
            code: Some(3),
            signal: None,
        }));
        assert_eq!(seen.kind, Exited);
        assert!(seen.detail.contains("code 3"), "{}", seen.detail);
        assert_eq!(exit(&Err(io_err())).kind, Io);
    }

    #[test]
    fn a_long_detail_is_cut_at_a_char_boundary() {
        let f = RunFailure::new(Io, "é".repeat(MAX_DETAIL_BYTES));
        assert!(f.detail.len() <= MAX_DETAIL_BYTES);
        assert!(f.detail.ends_with('…'));
        let short = RunFailure::new(Io, "short");
        assert_eq!(short.detail, "short");
    }

    #[test]
    fn the_policy_signal_keeps_timeouts_apart() {
        assert_eq!(Timeout.legacy(), Stopped::StartupTimeout);
        for k in [Setup, Refused, Io, Exited] {
            assert_eq!(k.legacy(), Stopped::Exited);
        }
    }
}

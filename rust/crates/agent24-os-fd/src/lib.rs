//! agent24-os-fd — the ONE place an inherited fd number becomes a socket
//! (ME4-S3 §4.3, §8 Q2). Exports no safe function that accepts a raw fd.
//!
//! The handshake token stays in `environ` on purpose (ME4-S3 §4.3): removing
//! it needs `std::env::remove_var`, which is `unsafe` in edition 2024 and
//! whose SAFETY precondition (no other thread touches the environment) a
//! library called from inside a multi-threaded tokio runtime cannot prove.

use std::os::fd::{BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicBool, Ordering};

/// Same value as `agent24_os_proto::launch::ENV_LISTEN_FD` (proto asserts it).
pub const ENV_LISTEN_FD: &str = "A24_LISTEN_FD";
/// Same value as `agent24_os_proto::launch::LISTEN_FD` (proto asserts it).
pub const LISTEN_FD: RawFd = 3;

static TAKEN: AtomicBool = AtomicBool::new(false);

#[derive(Debug)]
pub enum InheritError {
    /// A previous call (successful or not) already consumed the one attempt.
    AlreadyTaken,
    Missing,
    NotANumber,
    /// The kernel always passes fd 3; anything else is not ours to take.
    WrongFd(RawFd),
    NotASocket,
    NotUnix,
    NotStream,
    NotListening,
    /// The fd carries `FD_CLOEXEC`, which means this process opened it
    /// itself (std, tokio, and rustix's plain `socket()`/`socket_with`
    /// default all set it); only a fd the kernel `dup2`'d into fd 3 across
    /// `exec` (never CLOEXEC, see `launch.rs`) may be adopted. Rejecting
    /// this closes the gap where a child that inherits `A24_LISTEN_FD=3`
    /// but never actually got fd 3 from the kernel has itself opened some
    /// other socket that happens to land on fd 3.
    NotInherited,
    Io(std::io::Error),
}

/// Take over the kernel-bound listening socket, exactly once per process.
///
/// Reads `A24_LISTEN_FD` itself, requires it to be `3`, checks the fd is an
/// `AF_UNIX` `SOCK_STREAM` socket that is listening (Linux: `SO_ACCEPTCONN`;
/// macOS: the weaker "no peer" check, see `is_listening`) and does NOT
/// already carry `FD_CLOEXEC` — proof it crossed `exec` from the kernel
/// launcher rather than being something this process opened itself onto the
/// same fd number (see `NotInherited`) — then sets `FD_CLOEXEC` (children
/// must not inherit it) and `O_NONBLOCK` (tokio needs it).
/// A failed check leaves the fd open and un-owned (never closed: it might be
/// someone else's), and still consumes the one attempt.
///
/// # Errors
/// See [`InheritError`].
pub fn take_inherited_listener() -> Result<UnixListener, InheritError> {
    if TAKEN.swap(true, Ordering::SeqCst) {
        return Err(InheritError::AlreadyTaken);
    }
    let raw = std::env::var(ENV_LISTEN_FD).map_err(|_| InheritError::Missing)?;
    let fd: RawFd = raw.parse().map_err(|_| InheritError::NotANumber)?;
    if fd != LISTEN_FD {
        return Err(InheritError::WrongFd(fd));
    }
    let owned = adopt(fd)?;
    rustix::io::fcntl_setfd(&owned, rustix::io::FdFlags::CLOEXEC)
        .map_err(|e| InheritError::Io(e.into()))?;
    let listener = UnixListener::from(owned);
    listener.set_nonblocking(true).map_err(InheritError::Io)?;
    Ok(listener)
}

/// The single `unsafe` site of the workspace. Validates through a borrow
/// first and only then claims ownership, so a wrong fd is never closed.
#[allow(unsafe_code)]
fn adopt(fd: RawFd) -> Result<OwnedFd, InheritError> {
    // SAFETY: `fd` is 3 (checked by the caller). This borrow does not by
    // itself prove fd 3 is the kernel's inherited fd and not something this
    // process already had open at that number — `validate` below is what
    // gives that evidence (via the `FD_CLOEXEC` check, a heuristic: it holds
    // for every fd-creation path this codebase uses, not as a language
    // guarantee — see the comment on that check for the exact reasoning).
    // Safety here does not rest on the heuristic alone: it is the
    // combination of every check in `validate` (socket, AF_UNIX, STREAM,
    // listening, no `FD_CLOEXEC`) together with the `TAKEN` swap, which
    // guarantees this function runs at most once, so no second owner can
    // exist and the borrow does not outlive this call.
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    validate(borrowed)?;
    // SAFETY: as above — `validate` proved it is the listening socket AND
    // that it lacks `FD_CLOEXEC`, which is this crate's evidence (not a
    // language guarantee) that it was not opened by this process and can
    // only be the fd the kernel handed us across `exec`. No other owner
    // exists.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn validate(fd: BorrowedFd<'_>) -> Result<(), InheritError> {
    let st = rustix::fs::fstat(fd).map_err(|_| InheritError::NotASocket)?;
    if rustix::fs::FileType::from_raw_mode(st.st_mode) != rustix::fs::FileType::Socket {
        return Err(InheritError::NotASocket);
    }
    let local = rustix::net::getsockname(fd).map_err(|e| InheritError::Io(e.into()))?;
    if local.address_family() != rustix::net::AddressFamily::UNIX {
        return Err(InheritError::NotUnix);
    }
    let ty = rustix::net::sockopt::socket_type(fd).map_err(|e| InheritError::Io(e.into()))?;
    if ty != rustix::net::SocketType::STREAM {
        return Err(InheritError::NotStream);
    }
    if !is_listening(fd)? {
        return Err(InheritError::NotListening);
    }
    // Heuristic, not a language guarantee: a fd this process opened itself —
    // std, tokio, rustix's `socket_with` with `SocketFlags::CLOEXEC` —
    // carries `FD_CLOEXEC` on every fd-creation path this codebase actually
    // uses (Rust has set it on every fd it creates for a long time; rustix's
    // plain `socket()` is the one exception used by this crate's own
    // fixtures, see the tests). The kernel launcher hands fd 3 to the child
    // via `dup2` (`launch.rs`), which never sets `FD_CLOEXEC`. So its absence
    // is this crate's evidence that the fd crossed `exec` from the kernel,
    // rather than being some other socket this process already had open at
    // fd 3 while a misconfigured child also believes `A24_LISTEN_FD=3` was
    // inherited — this check does not stand alone: it is combined with the
    // socket/AF_UNIX/STREAM/listening checks above and the one-shot `TAKEN`
    // swap in `validate`'s caller.
    if rustix::io::fcntl_getfd(fd)
        .map_err(|e| InheritError::Io(e.into()))?
        .contains(rustix::io::FdFlags::CLOEXEC)
    {
        return Err(InheritError::NotInherited);
    }
    Ok(())
}

/// Linux (and every non-Apple target): `SO_ACCEPTCONN` is a direct answer —
/// true exactly when `listen()` has been called. CI compiles and tests this
/// branch on `ubuntu-latest` (J-S20).
#[cfg(not(target_vendor = "apple"))]
fn is_listening(fd: BorrowedFd<'_>) -> Result<bool, InheritError> {
    rustix::net::sockopt::socket_acceptconn(fd).map_err(|e| InheritError::Io(e.into()))
}

/// Apple declares `SO_ACCEPTCONN` but does not implement it (`ENOPROTOOPT`;
/// rustix gates it off), so this is a WEAKER, indirect check: "has no peer".
/// `getpeername` answers `ENOTCONN` for a listening socket — and ALSO for a
/// stream socket that was only `socket()`ed or `bind()`ed and never
/// `listen()`ed. KNOWN GAP (accepted, ME4-S3 §4.3 step 3): on macOS such a
/// socket passes validation; the kernel only ever hands fd 3 after
/// `listen()` (`launch.rs`), so meeting one means a forged environment, and
/// the failure then surfaces at `accept` (module exits), not as privilege.
/// A connected stream or live socketpair end has a peer (`Ok`); one whose
/// peer is gone answers `EINVAL` — both are "not listening". The test
/// `apple_gap_bound_but_not_listening_passes` pins the gap so a change in
/// either direction is noticed.
#[cfg(target_vendor = "apple")]
fn is_listening(fd: BorrowedFd<'_>) -> Result<bool, InheritError> {
    match rustix::net::getpeername(fd) {
        Err(rustix::io::Errno::NOTCONN) => Ok(true),
        Ok(_) | Err(rustix::io::Errno::INVAL) => Ok(false),
        Err(e) => Err(InheritError::Io(e.into())),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::os::fd::AsFd;

    /// The checks, on fds this test owns (never fd 3, never the env path).
    fn check_owned(fd: OwnedFd) -> Result<(), InheritError> {
        validate(fd.as_fd())
    }

    /// Std sets `FD_CLOEXEC` on every fd it creates; strip it back off so a
    /// std-created fixture can stand in for a fd the kernel handed across
    /// `exec` (which never carries it).
    fn clear_cloexec(fd: &OwnedFd) {
        rustix::io::fcntl_setfd(fd, rustix::io::FdFlags::empty()).unwrap();
    }

    #[test]
    fn a_listening_unix_socket_passes() {
        let dir = std::env::temp_dir().join(format!("a24fd-{}", std::process::id()));
        let _ = std::fs::remove_file(&dir);
        let l = UnixListener::bind(&dir).unwrap();
        let fd: OwnedFd = l.into();
        // std sets FD_CLOEXEC by default; clear it so this fixture reads as
        // an inherited-across-exec fd, which is what `validate` now requires.
        clear_cloexec(&fd);
        assert!(check_owned(fd).is_ok());
        let _ = std::fs::remove_file(&dir);
    }

    /// A plain std `UnixListener` keeps `FD_CLOEXEC` set — i.e. it was opened
    /// by this process, not handed across `exec` by the kernel — so it must
    /// be rejected even though it is otherwise a perfectly good listening
    /// AF_UNIX/SOCK_STREAM socket.
    #[test]
    fn a_cloexec_listener_is_rejected_as_not_inherited() {
        let dir = std::env::temp_dir().join(format!("a24fd-ni-{}", std::process::id()));
        let _ = std::fs::remove_file(&dir);
        let l = UnixListener::bind(&dir).unwrap();
        let fd: OwnedFd = l.into();
        assert!(matches!(check_owned(fd), Err(InheritError::NotInherited)));
        let _ = std::fs::remove_file(&dir);
    }

    #[test]
    fn a_socketpair_end_is_not_listening() {
        let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
        assert!(matches!(
            check_owned(a.into()),
            Err(InheritError::NotListening)
        ));
    }

    /// A socketpair end whose peer is gone: macOS `getpeername` says
    /// `EINVAL` (mapped to "not listening"), Linux `SO_ACCEPTCONN` says 0.
    #[test]
    fn a_socketpair_end_with_peer_closed_is_not_listening() {
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        drop(b);
        assert!(matches!(
            check_owned(a.into()),
            Err(InheritError::NotListening)
        ));
    }

    /// `socket()` + `bind()`, never `listen()`.
    fn bound_not_listening() -> (OwnedFd, std::path::PathBuf) {
        use rustix::net::{AddressFamily, SocketAddrUnix, SocketType, bind, socket};
        let p = std::env::temp_dir().join(format!("a24fd-b-{}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let s = socket(AddressFamily::UNIX, SocketType::STREAM, None).unwrap();
        bind(&s, &SocketAddrUnix::new(&p).unwrap()).unwrap();
        (s, p)
    }

    /// Linux: the direct `SO_ACCEPTCONN` check rejects it.
    #[cfg(not(target_vendor = "apple"))]
    #[test]
    fn bound_but_not_listening_is_rejected() {
        let (s, p) = bound_not_listening();
        assert!(matches!(check_owned(s), Err(InheritError::NotListening)));
        let _ = std::fs::remove_file(&p);
    }

    /// macOS KNOWN GAP (ME4-S3 §4.3 step 3): the indirect check cannot tell
    /// it from a listening socket. Pinned so that a fix (or a regression in
    /// the other direction) turns this red and the doc gets updated.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn apple_gap_bound_but_not_listening_passes() {
        let (s, p) = bound_not_listening();
        assert!(check_owned(s).is_ok());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_regular_file_is_not_a_socket() {
        let f = std::fs::File::open("/dev/null").unwrap();
        assert!(matches!(
            check_owned(f.into()),
            Err(InheritError::NotASocket)
        ));
    }

    #[test]
    fn a_tcp_listener_is_not_unix() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        assert!(matches!(check_owned(l.into()), Err(InheritError::NotUnix)));
    }

    /// The ONLY test in this binary that may call `take_inherited_listener`:
    /// the one attempt is per process, and all tests of one test binary share
    /// a process (M-3). Any other test calling it would race this one.
    #[test]
    fn only_one_attempt_per_process() {
        // Whatever the first attempt returns (no env var in `cargo test`), the
        // second is refused.
        let _ = take_inherited_listener();
        assert!(matches!(
            take_inherited_listener(),
            Err(InheritError::AlreadyTaken)
        ));
    }
}

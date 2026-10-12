//! The daemon lock and the daemon's owner-only Unix sockets, `acd.sock` and
//! `hook.sock` (amendment A1). One `acd run` per config dir holds
//! `acd.lock` for its whole life. A socket path left behind is replaced only
//! when it is definitely stale; a socket is removed at shutdown only while
//! it is still the one this process created.
use std::fs::OpenOptions;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};

pub const LOCK_FILE: &str = "acd.lock";

/// How long a connect to an existing socket may take before acd gives up
/// and refuses to start.
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// Held for the daemon's whole life; released when the process exits.
pub struct DaemonLock(#[allow(dead_code)] std::fs::File);

/// Takes `<dir>/acd.lock` without waiting: a held lock means another
/// `acd run` owns this config dir.
pub fn lock_daemon(dir: &Path) -> Result<DaemonLock> {
    let path = dir.join(LOCK_FILE);
    let f = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path)
        .with_context(|| format!("could not open {} (must be a regular file)", path.display()))?;
    crate::writer_lock::validate_descriptor(&f, &path)?;
    // SAFETY: a valid open descriptor; flock has no other preconditions.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::EWOULDBLOCK) {
            bail!("another acd is running ({} is locked)", path.display());
        }
        return Err(e).with_context(|| format!("could not lock {}", path.display()));
    }
    Ok(DaemonLock(f))
}

/// A socket this process bound. Dropping it removes the path, but only if
/// it still names the same inode (a successor's socket is left alone).
pub struct OwnedSocket {
    path: PathBuf,
    dev: u64,
    ino: u64,
}

impl Drop for OwnedSocket {
    fn drop(&mut self) {
        if let Ok(m) = std::fs::symlink_metadata(&self.path)
            && m.file_type().is_socket()
            && (m.dev(), m.ino()) == (self.dev, self.ino)
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Binds `path` with mode 0600, set before the caller serves it. The lock
/// serializes startup, so the stale check and the bind do not race another
/// acd.
pub fn bind(_lock: &DaemonLock, path: &Path) -> Result<(UnixListener, OwnedSocket)> {
    bind_as(path, acd_securefs::euid())
}

fn bind_as(path: &Path, euid: u32) -> Result<(UnixListener, OwnedSocket)> {
    clear_stale(path, euid)?;
    let listener =
        UnixListener::bind(path).with_context(|| format!("cannot bind {}", path.display()))?;
    let m = std::fs::symlink_metadata(path)
        .with_context(|| format!("cannot inspect {} after binding it", path.display()))?;
    let owned = OwnedSocket {
        path: path.to_path_buf(),
        dev: m.dev(),
        ino: m.ino(),
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("cannot restrict {} to 0600", path.display()))?;
    listener.set_nonblocking(true)?;
    Ok((listener, owned))
}

/// Refuses a path that `bind` would not replace, without touching it. Startup
/// runs this before it binds any listener, so a refusal leaves nothing bound.
pub fn check_path(path: &Path) -> Result<()> {
    stale_socket(path, acd_securefs::euid()).map(drop)
}

/// Removes `path` only when it is definitely stale (see `stale_socket`).
fn clear_stale(path: &Path, euid: u32) -> Result<()> {
    if stale_socket(path, euid)? {
        let shown = path.display();
        std::fs::remove_file(path).with_context(|| format!("removing the stale socket {shown}"))?;
    }
    Ok(())
}

/// Whether `path` is a stale socket that `euid` may replace: a socket owned
/// by `euid` whose connect fails with `ECONNREFUSED`. An absent path is not
/// stale. A live socket, another file type or owner, or any other error
/// refuses.
fn stale_socket(path: &Path, euid: u32) -> Result<bool> {
    let shown = path.display();
    let m = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e).with_context(|| format!("cannot inspect {shown}")),
    };
    if !m.file_type().is_socket() {
        bail!("{shown} exists and is not a socket; remove it");
    }
    if m.uid() != euid {
        bail!(
            "{shown} is owned by another user (uid {}); refusing to replace it",
            m.uid()
        );
    }
    match connect_within(path, PROBE_TIMEOUT) {
        Ok(()) => bail!("{shown} is in use (another acd is running?); refusing to replace it"),
        Err(e) if e.raw_os_error() == Some(libc::ECONNREFUSED) => Ok(true),
        Err(e) => Err(e)
            .with_context(|| format!("cannot tell whether {shown} is in use; not replacing it")),
    }
}

/// Connects in a helper thread so a socket that never answers (for example
/// one with a full accept backlog) cannot stall the caller.
pub(crate) fn connect_within(path: &Path, timeout: Duration) -> std::io::Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        let _ = tx.send(UnixStream::connect(&path).map(drop));
    });
    rx.recv_timeout(timeout).unwrap_or_else(|_| {
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "connect timed out",
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ino(p: &Path) -> u64 {
        std::fs::symlink_metadata(p).unwrap().ino()
    }

    fn euid() -> u32 {
        acd_securefs::euid()
    }

    /// Leaves a socket file that refuses connections. It is bound but never
    /// listens, so no copy of its descriptor (one a concurrent spawn in
    /// another test thread can hold until its exec) can make it accept.
    fn leave_stale_socket(path: &Path) {
        use std::os::unix::ffi::OsStrExt;
        // SAFETY: plain socket calls on a descriptor owned here and closed
        // before returning; `addr` is zeroed and then filled in bounds.
        unsafe {
            #[cfg(target_os = "linux")]
            let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
            #[cfg(not(target_os = "linux"))]
            let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
            assert!(fd >= 0, "{}", std::io::Error::last_os_error());
            #[cfg(not(target_os = "linux"))]
            assert_eq!(libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC), 0);
            let mut addr: libc::sockaddr_un = std::mem::zeroed();
            addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
            let bytes = path.as_os_str().as_bytes();
            assert!(bytes.len() < addr.sun_path.len(), "socket path too long");
            for (dst, src) in addr.sun_path.iter_mut().zip(bytes) {
                *dst = *src as libc::c_char;
            }
            let rc = libc::bind(
                fd,
                (&raw const addr).cast::<libc::sockaddr>(),
                std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
            );
            let err = std::io::Error::last_os_error();
            libc::close(fd);
            assert_eq!(rc, 0, "{err}");
        }
    }

    #[test]
    fn a_second_lock_is_refused_while_the_first_is_held() {
        let d = tempfile::tempdir().unwrap();
        let first = lock_daemon(d.path()).unwrap();
        let e = format!("{:#}", lock_daemon(d.path()).err().expect("refused"));
        assert!(e.contains("another acd is running"), "{e}");
        drop(first);
        // Another test thread can spawn a process while `first` is open, and
        // the child holds a copy of the descriptor (and so the flock) until
        // its exec closes it. Wait for the release.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            match lock_daemon(d.path()) {
                Ok(_) => break,
                Err(e) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "lock not released: {e:#}"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }

    #[test]
    fn a_new_socket_is_private() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.sock");
        let (_l, _owned) = bind_as(&p, euid()).unwrap();
        let m = std::fs::symlink_metadata(&p).unwrap();
        assert!(m.file_type().is_socket());
        assert_eq!(m.mode() & 0o777, 0o600);
    }

    #[test]
    fn a_stale_socket_is_replaced() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.sock");
        leave_stale_socket(&p);
        // The old socket refuses connections; the inode number may be
        // reused (it is on Linux), so prove the replacement by serving.
        assert_eq!(
            UnixStream::connect(&p).err().and_then(|e| e.raw_os_error()),
            Some(libc::ECONNREFUSED)
        );
        let (l, _owned) = bind_as(&p, euid()).unwrap();
        drop(UnixStream::connect(&p).unwrap());
        assert!(l.accept().is_ok());
    }

    #[test]
    fn a_live_socket_is_refused_and_kept() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.sock");
        let _live = UnixListener::bind(&p).unwrap();
        let before = ino(&p);
        let e = format!("{:#}", bind_as(&p, euid()).err().expect("refused"));
        assert!(e.contains("in use"), "{e}");
        assert_eq!(ino(&p), before);
    }

    #[test]
    fn another_owner_is_refused_and_kept() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.sock");
        drop(UnixListener::bind(&p).unwrap());
        let before = ino(&p);
        let e = format!("{:#}", bind_as(&p, euid() + 1).err().expect("refused"));
        assert!(e.contains("owned by another user"), "{e}");
        assert_eq!(ino(&p), before);
    }

    #[test]
    fn other_file_types_are_refused_and_kept() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("stale.sock");
        drop(UnixListener::bind(&target).unwrap());
        let file = d.path().join("file.sock");
        std::fs::write(&file, b"x").unwrap();
        let dir = d.path().join("dir.sock");
        std::fs::create_dir(&dir).unwrap();
        let link = d.path().join("link.sock");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        for p in [&file, &dir, &link] {
            let e = format!("{:#}", bind_as(p, euid()).err().expect("refused"));
            assert!(e.contains("not a socket"), "{e}");
            assert!(
                std::fs::symlink_metadata(p).is_ok(),
                "{} removed",
                p.display()
            );
        }
        assert!(std::fs::symlink_metadata(&target).is_ok());
    }

    #[test]
    fn an_unreachable_socket_is_refused_and_kept() {
        // A socket without write permission fails with EACCES, which says
        // nothing about whether it is live.
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.sock");
        drop(UnixListener::bind(&p).unwrap());
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();
        if UnixStream::connect(&p).err().and_then(|e| e.raw_os_error()) != Some(libc::EACCES) {
            return; // running as root: permission bits do not apply
        }
        let before = ino(&p);
        let e = format!("{:#}", bind_as(&p, euid()).err().expect("refused"));
        assert!(e.contains("cannot tell whether"), "{e}");
        assert_eq!(ino(&p), before);
    }

    #[test]
    fn cleanup_removes_its_own_socket_but_not_a_successor() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.sock");
        let (_l, owned) = bind_as(&p, euid()).unwrap();
        drop(owned);
        assert!(std::fs::symlink_metadata(&p).is_err(), "own socket removed");

        let (_l, owned) = bind_as(&p, euid()).unwrap();
        std::fs::remove_file(&p).unwrap();
        let _successor = UnixListener::bind(&p).unwrap();
        let successor = ino(&p);
        drop(owned);
        assert_eq!(ino(&p), successor, "the successor's socket is kept");
    }
}

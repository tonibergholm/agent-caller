//! Socket startup (omarchy spec §4.4) and the peer check (§5.1).
use std::fs::{File, OpenOptions};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    #[error("{0}: the runtime directory must be a directory you own, mode 0700, and not a symlink")]
    RuntimeDir(PathBuf),
    #[error("another acd-inbox is running ({0} is locked)")]
    AlreadyRunning(PathBuf),
    #[error("{0} exists and is not a socket; refusing to remove it")]
    NotASocket(PathBuf),
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> StartupError + '_ {
    move |e| StartupError::Io(path.to_path_buf(), e)
}

/// Owned by the euid, mode within 0700, not a symlink (plan D23).
pub fn check_runtime_dir(dir: &Path) -> Result<(), StartupError> {
    acd_securefs::check_private_dir(dir, 0o700).map_err(|_| StartupError::RuntimeDir(dir.to_path_buf()))
}

/// Held for the service's whole life; released when the process exits.
pub struct LockFile(#[allow(dead_code)] File);

/// `O_NOFOLLOW`, mode 0600, exclusive `flock` without waiting.
pub fn take_lock(path: &Path) -> Result<LockFile, StartupError> {
    let f = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(io(path))?;
    validate_lock(&f, path)?;
    // SAFETY: a valid open descriptor; flock has no other preconditions.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::EWOULDBLOCK) {
            return Err(StartupError::AlreadyRunning(path.to_path_buf()));
        }
        return Err(StartupError::Io(path.to_path_buf(), e));
    }
    Ok(LockFile(f))
}

/// Checks the opened descriptor, not the path, as `crates/acd/src/writer_lock.rs`
/// does: a regular file owned by the euid. A looser mode is repaired.
fn validate_lock(f: &File, path: &Path) -> Result<(), StartupError> {
    let m = f.metadata().map_err(io(path))?;
    if !m.file_type().is_file() || m.uid() != acd_securefs::euid() {
        return Err(StartupError::Io(
            path.to_path_buf(),
            std::io::Error::other("the lock file must be a regular file you own"),
        ));
    }
    if m.mode() & 0o7777 != 0o600 {
        f.set_permissions(std::fs::Permissions::from_mode(0o600)).map_err(io(path))?;
    }
    Ok(())
}

/// With the lock held: a socket at `path` is removed. Any other file type,
/// or a symlink, is left alone and refused.
pub fn clear_socket_path(_lock: &LockFile, path: &Path) -> Result<(), StartupError> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(StartupError::Io(path.to_path_buf(), e)),
        Ok(m) if m.file_type().is_socket() => std::fs::remove_file(path).map_err(io(path)),
        Ok(_) => Err(StartupError::NotASocket(path.to_path_buf())),
    }
}

/// The socket this process bound. Dropping it removes the path only while
/// it still names the same inode.
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

pub struct Listening {
    pub listener: tokio::net::UnixListener,
    pub socket: OwnedSocket,
    pub lock: LockFile,
}

/// §4.4 in order: check the runtime dir, take the lock, clear a stale
/// socket, bind, then set mode 0600. Call inside a tokio runtime.
pub fn listen(runtime_dir: &Path, socket: &Path, lock: &Path) -> Result<Listening, StartupError> {
    check_runtime_dir(runtime_dir)?;
    let lock = take_lock(lock)?;
    clear_socket_path(&lock, socket)?;
    let std_listener = std::os::unix::net::UnixListener::bind(socket).map_err(io(socket))?;
    let m = std::fs::symlink_metadata(socket).map_err(io(socket))?;
    let owned = OwnedSocket { path: socket.to_path_buf(), dev: m.dev(), ino: m.ino() };
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600)).map_err(io(socket))?;
    std_listener.set_nonblocking(true).map_err(io(socket))?;
    let listener = tokio::net::UnixListener::from_std(std_listener).map_err(io(socket))?;
    Ok(Listening { listener, socket: owned, lock })
}

pub fn peer_allowed(peer_uid: u32, own_uid: u32) -> bool {
    peer_uid == own_uid
}

/// The peer runs as this user. Fails closed when its credentials cannot be
/// read (plan D24).
pub fn peer_ok(stream: &tokio::net::UnixStream) -> bool {
    stream.peer_cred().is_ok_and(|c| peer_allowed(c.uid(), acd_securefs::euid()))
}

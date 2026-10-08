use std::{
    fmt,
    fs::File,
    io::{Read, Take},
    os::fd::FromRawFd,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub enum SecureFsError {
    Missing(PathBuf),
    Symlink(PathBuf),
    NotRegular(PathBuf),
    Owner(PathBuf),
    Mode { path: PathBuf, mode: u32, want: u32 },
    TooLarge(PathBuf),
    Io(PathBuf, std::io::Error),
}

impl fmt::Display for SecureFsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(p) => write!(f, "{} is missing", p.display()),
            Self::Symlink(p) => write!(f, "{} is a symlink", p.display()),
            Self::NotRegular(p) => write!(f, "{} is not the expected file type", p.display()),
            Self::Owner(p) => write!(f, "{} has the wrong owner", p.display()),
            Self::Mode { path, mode, want } => write!(
                f,
                "{} has mode {:o}; expected a subset of {:o}",
                path.display(),
                mode,
                want
            ),
            Self::TooLarge(p) => write!(f, "{} is too large", p.display()),
            Self::Io(p, e) => write!(f, "{}: {e}", p.display()),
        }
    }
}
impl std::error::Error for SecureFsError {}

pub fn euid() -> u32 {
    unsafe { libc::geteuid() }
}
pub fn owner_ok(file_uid: u32, expected_uid: u32) -> bool {
    file_uid == expected_uid
}

fn open_path(path: &Path, directory: bool) -> Result<File, SecureFsError> {
    use std::os::unix::ffi::OsStrExt;
    let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        SecureFsError::Io(
            path.into(),
            std::io::Error::from(std::io::ErrorKind::InvalidInput),
        )
    })?;
    let mut flags = libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;
    if directory {
        flags |= libc::O_DIRECTORY;
    }
    // SAFETY: cpath is NUL terminated and flags request a descriptor only.
    let fd = unsafe { libc::open(cpath.as_ptr(), flags) };
    if fd < 0 {
        let e = std::io::Error::last_os_error();
        return Err(match e.raw_os_error() {
            Some(libc::ELOOP) => SecureFsError::Symlink(path.into()),
            Some(libc::ENOENT) => SecureFsError::Missing(path.into()),
            Some(libc::ENOTDIR) => SecureFsError::NotRegular(path.into()),
            _ => SecureFsError::Io(path.into(), e),
        });
    }
    // SAFETY: fd is newly returned and ownership transfers to File.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn verify(file: &File, path: &Path, directory: bool, max_mode: u32) -> Result<(), SecureFsError> {
    let m = file
        .metadata()
        .map_err(|e| SecureFsError::Io(path.into(), e))?;
    if (directory && !m.is_dir()) || (!directory && !m.is_file()) {
        return Err(SecureFsError::NotRegular(path.into()));
    }
    use std::os::unix::fs::MetadataExt;
    if !owner_ok(m.uid(), euid()) {
        return Err(SecureFsError::Owner(path.into()));
    }
    let mode = m.mode() & 0o7777;
    if mode & !max_mode != 0 {
        return Err(SecureFsError::Mode {
            path: path.into(),
            mode,
            want: max_mode,
        });
    }
    Ok(())
}

pub fn check_private_dir(dir: &Path, max_mode: u32) -> Result<(), SecureFsError> {
    let file = open_path(dir, true)?;
    verify(&file, dir, true, max_mode)
}

pub fn read_private(path: &Path, max_mode: u32, max_len: u64) -> Result<Vec<u8>, SecureFsError> {
    let mut file = open_path(path, false)?;
    verify(&file, path, false, max_mode)?;
    let cap = max_len.saturating_add(1);
    let mut reader: Take<&mut File> = (&mut file).take(cap);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|e| SecureFsError::Io(path.into(), e))?;
    if bytes.len() as u64 > max_len {
        return Err(SecureFsError::TooLarge(path.into()));
    }
    Ok(bytes)
}

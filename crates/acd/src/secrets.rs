use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use acd_securefs::SecureFsError;
use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// 32 random bytes, base64url without padding.
pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("OS randomness is available");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Accepts only a well-formed token: 43 base64url characters that decode to
/// 32 bytes.
pub fn parse_token(s: &str) -> Result<String> {
    let s = s.trim();
    if s.len() != 43 {
        bail!("token must be 43 base64url characters");
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| anyhow::anyhow!("token is not valid base64url"))?;
    if bytes.len() != 32 {
        bail!("token must decode to 32 bytes");
    }
    Ok(s.to_string())
}

pub fn ensure_private_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn explain(e: SecureFsError) -> anyhow::Error {
    match e {
        SecureFsError::Mode { path, mode, want } => anyhow::anyhow!(
            "{} has mode {:o}, which lets other users read it. Fix with: chmod {:o} {}",
            path.display(),
            mode & 0o7777,
            want,
            path.display()
        ),
        SecureFsError::Symlink(p) => {
            anyhow::anyhow!(
                "{} is a symlink; replace it with a regular file",
                p.display()
            )
        }
        SecureFsError::Owner(p) => anyhow::anyhow!("{} is not owned by you", p.display()),
        SecureFsError::NotRegular(p) => {
            anyhow::anyhow!("{} is not a regular file", p.display())
        }
        other => anyhow::anyhow!("{other}"),
    }
}

/// F1: maximum size for structured install records, journals and token stores.
pub const STRUCTURED_MAX: usize = 256 * 1024;

pub fn read_structured(path: &Path) -> Result<String> {
    let bytes = acd_securefs::read_private(path, 0o600, STRUCTURED_MAX as u64).map_err(explain)?;
    String::from_utf8(bytes).with_context(|| format!("{} is not UTF-8", path.display()))
}

/// Refuses, before writing, anything `read_structured` could not read back.
pub fn write_structured(path: &Path, text: &str) -> Result<()> {
    if text.len() + 1 > STRUCTURED_MAX {
        bail!(
            "{} would be too large ({} bytes, at most {STRUCTURED_MAX})",
            path.display(),
            text.len() + 1
        );
    }
    write_secret(path, text)
}

/// Writes `value` atomically with mode 0600: a unique temp file in the
/// same directory, fsync, rename, fsync of the directory. The temp file is
/// removed on any failure.
pub fn write_secret(path: &Path, value: &str) -> Result<()> {
    write_atomic(path, format!("{value}\n").as_bytes(), 0o600)
}

/// A held descriptor for a directory. Every operation is relative to the
/// descriptor (`openat`, `renameat`, `unlinkat`), so renaming or replacing
/// the directory's path afterwards cannot redirect a write.
pub struct DirFd {
    dir: fs::File,
    path: PathBuf,
}

fn cname(name: &std::ffi::OsStr) -> Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(name.as_bytes()).context("name contains a NUL byte")
}

fn cvt(r: i32) -> std::io::Result<i32> {
    if r < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(r)
    }
}

impl DirFd {
    /// Opens `path` once. `strict` refuses a symlink as the last component
    /// (the caller passes a canonical path) and requires a directory owned
    /// by the effective user, checked on the descriptor.
    pub fn open(path: &Path, strict: bool) -> Result<DirFd> {
        use std::os::unix::fs::MetadataExt;
        let mut flags = libc::O_DIRECTORY | libc::O_CLOEXEC;
        if strict {
            flags |= libc::O_NOFOLLOW;
        }
        let dir = OpenOptions::new()
            .read(true)
            .custom_flags(flags)
            .open(path)
            .with_context(|| format!("opening directory {}", path.display()))?;
        if strict {
            let m = dir.metadata()?;
            if !m.is_dir() || m.uid() != acd_securefs::euid() {
                bail!(
                    "refusing to use {}: not a directory owned by you",
                    path.display()
                );
            }
        }
        Ok(DirFd {
            dir,
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Opens an existing file for reading without following a symlink.
    /// `Ok(None)` when it does not exist; an error with `ELOOP` for a symlink.
    pub fn open_nofollow(&self, name: &std::ffi::OsStr) -> std::io::Result<Option<fs::File>> {
        use std::os::fd::{AsRawFd, FromRawFd};
        let c = cname(name).map_err(|e| std::io::Error::other(e.to_string()))?;
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
        // SAFETY: valid directory fd and NUL-terminated name.
        match cvt(unsafe { libc::openat(self.dir.as_raw_fd(), c.as_ptr(), flags) }) {
            // SAFETY: openat returned a fresh descriptor that we own.
            Ok(fd) => Ok(Some(unsafe { fs::File::from_raw_fd(fd) })),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Removes a file relative to the descriptor. A missing file is fine.
    pub fn unlink(&self, name: &std::ffi::OsStr) -> Result<()> {
        use std::os::fd::AsRawFd;
        let c = cname(name)?;
        // SAFETY: valid directory fd and NUL-terminated name.
        match cvt(unsafe { libc::unlinkat(self.dir.as_raw_fd(), c.as_ptr(), 0) }) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(e).with_context(|| format!("removing {}", self.path.join(name).display()))
            }
            _ => Ok(()),
        }
    }

    pub fn sync(&self) -> std::io::Result<()> {
        self.dir.sync_all()
    }

    /// A unique temporary file in this directory, created `O_EXCL|O_NOFOLLOW`
    /// with the exact mode, written and fsynced. Nothing named `name` changes.
    ///
    /// The staged file holds the final contents, token included, until the
    /// rename; if the directory sits in a Git work tree it could be added
    /// concurrently for those milliseconds, exactly as the settings file can
    /// be while untracked. That exposure is accepted.
    pub fn stage(&self, name: &std::ffi::OsStr, bytes: &[u8], mode: u32) -> Result<Staged> {
        use std::os::fd::{AsRawFd, FromRawFd};
        let mut rnd = [0u8; 8];
        getrandom::fill(&mut rnd).expect("OS randomness is available");
        let mut tmp = name.to_os_string();
        tmp.push(format!(
            ".{}.{}.tmp",
            std::process::id(),
            rnd.iter().map(|b| format!("{b:02x}")).collect::<String>()
        ));
        let ctmp = cname(&tmp)?;
        let flags =
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: valid directory fd and NUL-terminated name.
        let fd = cvt(unsafe {
            libc::openat(
                self.dir.as_raw_fd(),
                ctmp.as_ptr(),
                flags,
                mode as libc::c_uint,
            )
        })
        .with_context(|| format!("creating {}", self.path.join(&tmp).display()))?;
        // SAFETY: openat returned a fresh descriptor that we own.
        let mut f = unsafe { fs::File::from_raw_fd(fd) };
        let staged = Staged {
            dir: self.dir.try_clone()?,
            tmp: ctmp,
            target: cname(name)?,
            shown: self.path.join(name),
            live: true,
        };
        // `mode` above is masked by the umask; set it exactly on the fd.
        f.set_permissions(fs::Permissions::from_mode(mode))?;
        f.write_all(bytes)?;
        f.sync_all()?;
        Ok(staged)
    }
}

/// A fully written, fsynced temporary file waiting to replace its target.
/// Dropping it without `commit` removes the temporary file.
pub struct Staged {
    dir: fs::File,
    tmp: std::ffi::CString,
    target: std::ffi::CString,
    shown: PathBuf,
    live: bool,
}

/// A failed `Staged::commit`. `renamed` says whether the target may already
/// hold the new bytes: false means it is untouched, true means the rename
/// happened (or may have) and only a later step, such as the directory
/// fsync, failed.
pub struct CommitError {
    pub renamed: bool,
    pub error: anyhow::Error,
}

impl CommitError {
    pub fn not_renamed(error: anyhow::Error) -> Self {
        Self {
            renamed: false,
            error,
        }
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if self.live {
            use std::os::fd::AsRawFd;
            // SAFETY: valid directory fd and NUL-terminated name.
            unsafe { libc::unlinkat(self.dir.as_raw_fd(), self.tmp.as_ptr(), 0) };
        }
    }
}

/// First half of `write_atomic`: stages next to `path`, opening the parent
/// directory once. Nothing at `path` changes.
pub fn stage_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<Staged> {
    let dir = DirFd::open(path.parent().context("path has no parent")?, false)?;
    dir.stage(
        path.file_name().context("path has no file name")?,
        bytes,
        mode,
    )
}

impl Staged {
    /// `renameat` over the target, then fsyncs the directory.
    pub fn commit(mut self) -> std::result::Result<(), CommitError> {
        use std::os::fd::AsRawFd;
        let fd = self.dir.as_raw_fd();
        // SAFETY: valid directory fd and NUL-terminated names.
        if let Err(e) =
            cvt(unsafe { libc::renameat(fd, self.tmp.as_ptr(), fd, self.target.as_ptr()) })
        {
            return Err(CommitError::not_renamed(
                anyhow::Error::new(e).context(format!("replacing {}", self.shown.display())),
            ));
        }
        self.live = false;
        // The rename already happened: the new value is in place. A failure
        // here only means durability is unconfirmed, so say so.
        self.dir.sync_all().map_err(|e| CommitError {
            renamed: true,
            error: anyhow::Error::new(e).context(format!(
                "{} was written with the new value, but syncing its directory failed; durability is unconfirmed",
                self.shown.display(),
            )),
        })
    }
}

/// Writes bytes atomically with the requested mode using a unique same-directory
/// temporary file, fsync, rename, and directory fsync.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    stage_atomic(path, bytes, mode)?
        .commit()
        .map_err(|e| e.error)
}

/// Fails if `path` is a symlink, is not owned by the effective user, or
/// grants any permission bit outside `max_mode`.
pub fn check_mode(path: &Path, max_mode: u32) -> Result<()> {
    let meta = fs::symlink_metadata(path).with_context(|| format!("reading {}", path.display()))?;
    let r = if meta.is_dir() {
        acd_securefs::check_private_dir(path, max_mode)
    } else {
        acd_securefs::check_private_file(path, max_mode)
    };
    r.map_err(explain)
}

pub fn read_secret(path: &Path) -> Result<String> {
    let bytes = acd_securefs::read_private(path, 0o600, 4096).map_err(explain)?;
    Ok(String::from_utf8(bytes)
        .context("secret is not UTF-8")?
        .trim()
        .to_string())
}

/// For `acd init`: tightens a loose mode on a regular file the user owns.
/// Works on a no-follow descriptor, so symlinks, FIFOs, directories and
/// foreign owners are refused and nothing can be swapped after the check.
/// Returns whether it changed.
pub fn repair_mode(path: &Path, mode: u32) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let file = acd_securefs::open_owned_file(path).map_err(explain)?;
    let meta = file
        .metadata()
        .with_context(|| format!("reading {}", path.display()))?;
    if meta.mode() & 0o7777 & !mode == 0 {
        return Ok(false);
    }
    file.set_permissions(fs::Permissions::from_mode(mode))
        .with_context(|| format!("setting mode on {}", path.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_unique_and_url_safe() {
        let (a, b) = (new_token(), new_token());
        assert_ne!(a, b);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
    }

    #[test]
    fn parse_token_accepts_only_well_formed_tokens() {
        let t = new_token();
        assert_eq!(parse_token(&t).unwrap(), t);
        assert!(parse_token("").is_err());
        assert!(parse_token("short").is_err());
        assert!(parse_token(&format!("{t}A")).is_err());
        assert!(parse_token(&"!".repeat(43)).is_err());
    }

    #[test]
    fn write_atomic_uses_the_requested_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        write_atomic(&path, b"x", 0o644).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert_eq!(fs::read(&path).unwrap(), b"x");
    }

    const UMASK_CHILD: &str = "ACD_TEST_UMASK_CHILD";

    /// Runs only in the child process spawned below, so the process-wide
    /// umask change never touches other tests.
    #[test]
    #[ignore = "run by write_atomic_mode_ignores_the_process_umask in a child"]
    fn umask_child_checks_write_atomic_mode() {
        if std::env::var_os(UMASK_CHILD).is_none() {
            return;
        }
        unsafe { libc::umask(0o077) };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        write_atomic(&path, b"x", 0o644).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[test]
    fn write_atomic_mode_ignores_the_process_umask() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "secrets::tests::umask_child_checks_write_atomic_mode",
                "--nocapture",
            ])
            .env(UMASK_CHILD, "1")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && text.contains("1 passed"),
            "{text}{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn check_mode_rejects_group_and_world_bits() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t");
        write_secret(&p, "x").unwrap();
        check_mode(&p, 0o600).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o640)).unwrap();
        let err = check_mode(&p, 0o600).unwrap_err().to_string();
        assert!(err.contains("chmod 600"), "{err}");
    }

    #[test]
    fn check_mode_refuses_a_symlink_even_to_a_private_file() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        write_secret(&real, "x").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let err = check_mode(&link, 0o600).unwrap_err().to_string();
        assert!(err.contains("symlink"), "{err}");
        assert!(read_secret(&link).is_err());
    }

    #[test]
    fn write_secret_leaves_no_temp_file_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("occupied");
        std::fs::create_dir(&target).unwrap(); // rename onto a directory fails
        assert!(write_secret(&target, "x").is_err());
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn repair_mode_tightens_but_refuses_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t");
        write_secret(&p, "x").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(repair_mode(&p, 0o600).unwrap());
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(!repair_mode(&p, 0o600).unwrap(), "already private");
        let link = dir.path().join("l");
        std::os::unix::fs::symlink(&p, &link).unwrap();
        assert!(repair_mode(&link, 0o600).is_err());
    }

    #[test]
    fn repair_mode_refuses_directories_and_fifos() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("d");
        fs::create_dir(&sub).unwrap();
        let err = repair_mode(&sub, 0o600).unwrap_err().to_string();
        assert!(err.contains("not a regular file"), "{err}");
        let fifo = dir.path().join("f");
        let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0);
        let err = repair_mode(&fifo, 0o600).unwrap_err().to_string();
        assert!(err.contains("not a regular file"), "{err}");
    }

    #[test]
    fn repair_mode_clears_special_bits_and_reports_them() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t");
        write_secret(&p, "x").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o4600)).unwrap();
        let err = check_mode(&p, 0o600).unwrap_err().to_string();
        assert!(err.contains("4600"), "{err}");
        assert!(repair_mode(&p, 0o600).unwrap());
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o7777,
            0o600
        );
        check_mode(&p, 0o600).unwrap();
    }
}

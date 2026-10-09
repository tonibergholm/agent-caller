use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

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

/// Writes `value` atomically with mode 0600: a unique temp file in the
/// same directory, fsync, rename, fsync of the directory. The temp file is
/// removed on any failure.
pub fn write_secret(path: &Path, value: &str) -> Result<()> {
    let dir = path.parent().context("secret path has no parent")?;
    let mut rnd = [0u8; 8];
    getrandom::fill(&mut rnd).expect("OS randomness is available");
    let name = path
        .file_name()
        .context("secret path has no file name")?
        .to_string_lossy();
    let tmp = dir.join(format!(
        "{name}.{}.{}.tmp",
        std::process::id(),
        rnd.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ));
    let result = (|| -> Result<()> {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(value.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
        return result;
    }
    // The rename already happened: the new value is in place. A failure here
    // only means durability is unconfirmed, so say so.
    fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .with_context(|| {
            format!(
                "{} was written with the new value, but syncing {} failed; durability is unconfirmed",
                path.display(),
                dir.display()
            )
        })
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

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

/// 32 random bytes, base64url without padding.
pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("OS randomness is available");
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn ensure_private_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Writes `value` atomically with mode 0600.
pub fn write_secret(path: &Path, value: &str) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let _ = fs::remove_file(&tmp);
    let mut f = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)
        .with_context(|| format!("creating {}", tmp.display()))?;
    f.write_all(value.as_bytes())?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Fails if `path` grants any permission bit outside `max_mode`.
pub fn check_mode(path: &Path, max_mode: u32) -> Result<()> {
    let mode = fs::metadata(path).with_context(|| format!("reading {}", path.display()))?.permissions().mode() & 0o777;
    if mode & !max_mode != 0 {
        bail!(
            "{} has mode {:o}, which lets other users read it. Fix with: chmod {:o} {}",
            path.display(), mode, max_mode, path.display()
        );
    }
    Ok(())
}

#[allow(dead_code)] // used by `acd run` in Task 10
pub fn read_secret(path: &Path) -> Result<String> {
    check_mode(path, 0o600)?;
    Ok(fs::read_to_string(path)?.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_unique_and_url_safe() {
        let (a, b) = (new_token(), new_token());
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
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
}

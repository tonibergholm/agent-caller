use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use acd_core::tls::{KeyFiles, check_pair, server_config};
use anyhow::{Context, Result, bail};
use tokio::sync::watch;

const MAX_BUNDLE: u64 = 16 * 1024;

pub fn load(dir: &Path) -> Result<(KeyFiles, String)> {
    let path = dir.join("tls.pem");
    let bytes = acd_securefs::read_private(&path, 0o600, MAX_BUNDLE)
        .with_context(|| format!("reading TLS bundle {}", path.display()))?;
    let pem = std::str::from_utf8(&bytes).context("TLS bundle is not UTF-8")?;
    let files = KeyFiles::from_bundle(pem).context("parsing TLS bundle")?;
    let pin = check_pair(&files).context("validating TLS key and certificate")?;
    Ok((files, pin))
}

pub fn publish(dir: &Path, files: &KeyFiles) -> Result<()> {
    let lock_path = dir.join("tls.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&lock_path)
        .with_context(|| format!("opening {}", lock_path.display()))?;
    // SAFETY: lock is a live descriptor and flock does not retain the pointer.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error()).context("locking TLS bundle");
    }

    let mut random = [0u8; 8];
    getrandom::fill(&mut random).context("generating TLS temporary name")?;
    let suffix = random
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let tmp = dir.join(format!("tls.pem.{}.{}.tmp", std::process::id(), suffix));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&tmp)
        .with_context(|| format!("creating {}", tmp.display()))?;
    file.write_all(files.to_bundle().as_bytes())?;
    file.sync_all()
        .with_context(|| format!("syncing {}", tmp.display()))?;

    #[cfg(debug_assertions)]
    if std::env::var("ACD_TEST_FAILPOINT").as_deref() == Ok("publish_before_rename") {
        bail!("TLS publication failpoint before rename");
    }

    fs::rename(&tmp, dir.join("tls.pem")).context("publishing TLS bundle")?;
    File::open(dir)?
        .sync_all()
        .context("syncing config directory")?;
    drop(lock);
    Ok(())
}

pub fn ensure(dir: &Path) -> Result<()> {
    remove_old_temps(dir)?;
    if load(dir).is_ok() {
        return Ok(());
    }
    publish(dir, &acd_core::tls::generate("acd"))
}

pub fn rotate(dir: &Path) -> Result<()> {
    publish(dir, &acd_core::tls::generate("acd"))
}

fn remove_old_temps(dir: &Path) -> Result<()> {
    let now = SystemTime::now();
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("tls.pem.") || !name.ends_with(".tmp") {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if now.duration_since(metadata.modified()?).unwrap_or_default() > Duration::from_secs(60)
            && metadata.file_type().is_file()
        {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

pub async fn watch(
    dir: std::path::PathBuf,
    mut pin: String,
    tx: watch::Sender<Arc<rustls::ServerConfig>>,
) {
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        match load(&dir) {
            Ok((files, next_pin)) if pin != next_pin => match server_config(&files) {
                Ok(config) => {
                    tx.send_replace(config);
                    pin = next_pin;
                    tracing::info!("TLS key rotated; new connections use the new certificate");
                }
                Err(e) => tracing::error!(error = %e, "cannot configure reloaded TLS key"),
            },
            Ok(_) => {}
            Err(e) => {
                tracing::error!(error = %e, "cannot reload TLS bundle; keeping current certificate")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn watcher_applies_rotation_that_precedes_its_first_poll() {
        let dir = tempfile::tempdir().unwrap();
        publish(dir.path(), &acd_core::tls::generate("generation A")).unwrap();
        let (installed, installed_pin) = load(dir.path()).unwrap();
        let (tx, mut rx) = watch::channel(server_config(&installed).unwrap());
        publish(dir.path(), &acd_core::tls::generate("generation B")).unwrap();
        let watcher = tokio::spawn(watch(dir.path().to_path_buf(), installed_pin.clone(), tx));
        tokio::time::timeout(Duration::from_secs(10), rx.changed())
            .await
            .expect("watcher should publish rotation within 10 seconds")
            .expect("watch channel remains open");
        let (_, latest_pin) = load(dir.path()).unwrap();
        assert_ne!(latest_pin, installed_pin);
        let _current = rx.borrow_and_update().clone();
        watcher.abort();
    }
}

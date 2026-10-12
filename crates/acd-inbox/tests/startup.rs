use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};

use acd_inbox::startup::{self, StartupError};

/// A private runtime dir. Canonical, because on macOS /var is a symlink.
fn runtime() -> (tempfile::TempDir, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let p = std::fs::canonicalize(d.path()).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
    (d, p)
}

fn listen(dir: &Path) -> Result<startup::Listening, StartupError> {
    startup::listen(dir, &dir.join("acd-inbox.sock"), &dir.join("acd-inbox.lock"))
}

#[tokio::test]
async fn an_unsafe_runtime_dir_is_refused() {
    let (_d, dir) = runtime();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(listen(&dir), Err(StartupError::RuntimeDir(_))));
    let (_d2, real) = runtime();
    let (_d3, holder) = runtime();
    let link = holder.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert!(matches!(startup::check_runtime_dir(&link), Err(StartupError::RuntimeDir(_))));
    assert!(startup::check_runtime_dir(&real).is_ok());
}

#[tokio::test]
async fn a_second_instance_exits() {
    let (_d, dir) = runtime();
    let _first = listen(&dir).unwrap();
    assert!(matches!(listen(&dir), Err(StartupError::AlreadyRunning(_))));
}

#[tokio::test]
async fn a_regular_file_or_a_symlink_at_the_socket_path_is_never_deleted() {
    let (_d, dir) = runtime();
    let path = dir.join("acd-inbox.sock");
    std::fs::write(&path, "keep me").unwrap();
    assert!(matches!(listen(&dir), Err(StartupError::NotASocket(_))));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep me");
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(dir.join("elsewhere"), &path).unwrap();
    assert!(matches!(listen(&dir), Err(StartupError::NotASocket(_))));
    assert!(std::fs::symlink_metadata(&path).unwrap().file_type().is_symlink());
}

#[tokio::test]
async fn a_stale_socket_is_replaced_and_the_new_one_is_0600() {
    let (_d, dir) = runtime();
    let path = dir.join("acd-inbox.sock");
    drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
    assert!(std::fs::symlink_metadata(&path).unwrap().file_type().is_socket());
    let l = listen(&dir).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    let client = tokio::net::UnixStream::connect(&path).await.unwrap();
    let (server, _) = l.listener.accept().await.unwrap();
    assert!(startup::peer_ok(&server), "the same user passes");
    drop(client);
    drop(l);
    assert!(std::fs::symlink_metadata(&path).is_err(), "dropping the listener removes its socket");
}

#[test]
fn another_uid_is_refused() {
    assert!(startup::peer_allowed(1000, 1000));
    assert!(!startup::peer_allowed(0, 1000));
    assert!(!startup::peer_allowed(1001, 1000));
}

#[tokio::test]
async fn a_loose_lock_file_is_repaired_and_a_directory_refused() {
    let (_d, dir) = runtime();
    let lock = dir.join("acd-inbox.lock");
    std::fs::write(&lock, "").unwrap();
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o666)).unwrap();
    let held = startup::take_lock(&lock).unwrap();
    assert_eq!(std::fs::metadata(&lock).unwrap().permissions().mode() & 0o7777, 0o600);
    drop(held);
    let (_d2, dir2) = runtime();
    std::fs::create_dir(dir2.join("acd-inbox.lock")).unwrap();
    assert!(matches!(listen(&dir2), Err(StartupError::Io(..))));
}

#[tokio::test]
async fn a_symlinked_lock_path_is_refused() {
    let (_d, dir) = runtime();
    std::os::unix::fs::symlink(dir.join("target"), dir.join("acd-inbox.lock")).unwrap();
    assert!(matches!(listen(&dir), Err(StartupError::Io(..))));
}

#[tokio::test]
async fn dropping_leaves_a_socket_or_file_that_replaced_ours() {
    let (_d, dir) = runtime();
    let path = dir.join("acd-inbox.sock");
    let l = listen(&dir).unwrap();
    std::fs::remove_file(&path).unwrap();
    let other = std::os::unix::net::UnixListener::bind(&path).unwrap();
    drop(l);
    assert!(std::fs::symlink_metadata(&path).unwrap().file_type().is_socket(), "a different socket stays");
    drop(other);
    std::fs::remove_file(&path).unwrap();
    let l = listen(&dir).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, "keep me").unwrap();
    drop(l);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep me");
}

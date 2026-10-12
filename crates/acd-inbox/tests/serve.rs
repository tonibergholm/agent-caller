mod common;

use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use acd_client::settings::MemorySecrets;
use acd_inbox::{prefs::Paths, serve::start_service, server::NoKeyring};
use common::*;

#[tokio::test]
async fn start_service_listens_privately_and_names_the_local_machine() {
    let acd = tempfile::tempdir().unwrap();
    // SAFETY: set before any other thread of this test binary reads the environment.
    unsafe { std::env::set_var("ACD_CONFIG_DIR", acd.path()) };
    let config = tempfile::tempdir().unwrap();
    let run = tempfile::tempdir().unwrap();
    let runtime = std::fs::canonicalize(run.path()).unwrap();
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
    let paths = Paths::new(config.path(), &runtime);
    let prepared = Arc::new(Mutex::new(None));
    let p = prepared.clone();
    let running = start_service(&paths, Arc::new(MemorySecrets::default()), Arc::new(NoKeyring), fast(), "omarchy", move |keys| {
        *p.lock().unwrap() = Some(keys);
    })
    .await
    .unwrap();
    assert_eq!(*prepared.lock().unwrap(), Some(vec![]), "a fresh install has no remote machines");
    assert_eq!(std::fs::metadata(&paths.socket).unwrap().permissions().mode() & 0o777, 0o600);
    let mut v = View::connect(&paths.socket).await;
    let (_, settings, _) = v.greeting().await;
    assert_eq!(settings["machines"][0]["name"], "omarchy");
    let second = start_service(&paths, Arc::new(MemorySecrets::default()), Arc::new(NoKeyring), fast(), "omarchy", |_| {}).await;
    assert!(second.err().unwrap().to_string().contains("another acd-inbox is running"));
    running.server.abort();
    drop(running);
    assert!(std::fs::symlink_metadata(&paths.socket).is_err(), "the socket goes with the service");
}

mod common;

use std::sync::Arc;
use std::time::Duration;

use acd_client::fake::FakeServer;
use acd_client::inbox::{Inbox, InboxOptions, MachineError};
use acd_client::settings::{SecretErrorKind, SecretStatus, SecretStore, Settings};
use acd_inbox::prefs::Prefs;
use acd_inbox::secrets::fake::{FakeBackend, Hang, UnlockBehaviour};
use acd_inbox::secrets::{SecretWorker, WorkerStore, WorkerTimeouts};
use acd_inbox::server::{SecretControl, ServerOptions, Service};
use common::*;
use serde_json::json;

const QUICK: WorkerTimeouts = WorkerTimeouts { op: Duration::from_millis(200), unlock: Duration::from_millis(300) };

fn store(backend: &FakeBackend, timeouts: WorkerTimeouts) -> Arc<WorkerStore> {
    Arc::new(WorkerStore::new(SecretWorker::spawn(backend.clone(), timeouts)))
}

fn calls(b: &FakeBackend) -> usize {
    b.0.lock().unwrap().calls.len()
}

#[test]
fn reads_are_cached_and_writes_go_through() {
    let backend = FakeBackend::default();
    backend.0.lock().unwrap().items.insert("k".into(), "v".into());
    let s = store(&backend, WorkerTimeouts::LIVE);
    s.prefill(&["k".into()]);
    for _ in 0..3 {
        assert_eq!(s.get("k").unwrap().as_deref(), Some("v"));
    }
    s.set("k", "w").unwrap();
    assert_eq!(s.get("k").unwrap().as_deref(), Some("w"));
    s.delete("k").unwrap();
    assert_eq!(s.get("k").unwrap(), None);
    assert_eq!(backend.0.lock().unwrap().calls, ["read k", "write k", "delete k"]);
}

#[test]
fn errors_are_cached_until_refreshed() {
    for (setup, kind) in [
        ((true, false), SecretErrorKind::Locked),
        ((false, true), SecretErrorKind::Other),
    ] {
        let backend = FakeBackend::default();
        {
            let mut k = backend.0.lock().unwrap();
            k.items.insert("k".into(), "v".into());
            (k.locked, k.unavailable) = setup;
        }
        let s = store(&backend, WorkerTimeouts::LIVE);
        s.prefill(&["k".into()]);
        let before = calls(&backend);
        assert_eq!(s.get("k").unwrap_err().kind(), kind);
        let _ = s.status("k");
        assert_eq!(calls(&backend), before, "a cached error does not go back to the keyring");
        {
            let mut k = backend.0.lock().unwrap();
            (k.locked, k.unavailable) = (false, false);
        }
        assert_eq!(s.get("k").unwrap_err().kind(), kind, "still cached");
        s.refresh(&["k".into()]);
        assert_eq!(s.get("k").unwrap().as_deref(), Some("v"));
    }
}

#[test]
fn a_locked_keyring_at_start_is_noted_and_never_prompted() {
    let backend = FakeBackend::default();
    {
        let mut k = backend.0.lock().unwrap();
        k.items.insert("k".into(), "v".into());
        k.locked = true;
    }
    let s = store(&backend, WorkerTimeouts::LIVE);
    s.prefill(&["k".into()]);
    assert_eq!(s.status("k"), SecretStatus::Locked);
    assert!(!backend.0.lock().unwrap().calls.iter().any(|c| c == "unlock"));
}

/// Lets an `UntilReleased` change finish, on drop too.
struct Release(FakeBackend);

impl Drop for Release {
    fn drop(&mut self) {
        self.0.0.lock().unwrap().released = true;
    }
}

#[test]
fn a_timed_out_write_leaves_the_key_unknown_until_a_read_after_it_settles() {
    let backend = FakeBackend::default();
    backend.0.lock().unwrap().hang_writes = Hang::UntilReleased;
    let release = Release(backend.clone());
    let s = store(&backend, WorkerTimeouts { op: Duration::from_millis(300), ..QUICK });
    assert_eq!(s.set("k", "v").unwrap_err().kind(), SecretErrorKind::Unknown);
    assert_eq!(s.status("k"), SecretStatus::Unknown);
    let retrying = {
        let s = s.clone();
        std::thread::spawn(move || s.refresh(&["k".into()]))
    };
    while s.pending_for_test() == 0 {
        std::thread::yield_now();
    }
    drop(release);
    retrying.join().unwrap();
    assert_eq!(s.get("k").unwrap().as_deref(), Some("v"), "the retry read the settled write");
    // "v" is not a machine secret, so the status is Unreadable; what matters
    // is that Unknown is gone.
    assert_eq!(s.status("k"), SecretStatus::Unreadable);
}

#[test]
fn a_failed_read_does_not_hide_an_unconfirmed_change() {
    let backend = FakeBackend::default();
    backend.0.lock().unwrap().hang_writes = Hang::UntilReleased;
    let release = Release(backend.clone());
    let s = store(&backend, WorkerTimeouts { op: Duration::from_millis(200), ..QUICK });
    assert_eq!(s.set("k", "v").unwrap_err().kind(), SecretErrorKind::Unknown);
    // The retry queues behind the running write and times out.
    s.refresh(&["k".into()]);
    assert_eq!(s.get("k").unwrap_err().kind(), SecretErrorKind::Unknown, "re-pairing needs Unknown");
    assert_eq!(s.status("k"), SecretStatus::Unknown);
    drop(release);
    s.refresh(&["k".into()]);
    assert_eq!(s.get("k").unwrap().as_deref(), Some("v"), "a successful read clears it");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hung_write_fails_everything_closed_and_nothing_reconnects() {
    let f = FakeServer::start(TOKEN, acd_client::fake::Identity::Paired).await;
    let backend = FakeBackend::default();
    backend.0.lock().unwrap().items.insert("m1".into(), box_secret(&f));
    let s = store(&backend, QUICK);
    let prefill = s.clone();
    tokio::task::spawn_blocking(move || prefill.prefill(&["m1".into()])).await.unwrap();
    let inbox = Inbox::new(
        box_settings(&f),
        s.clone(),
        InboxOptions { timing: fast(), settings_path: None, client_name: "t".into(), client_version: "0".into() },
    );
    inbox.start();
    f.next_connection().await;
    backend.0.lock().unwrap().hang_writes = Hang::Forever;
    let line = format!("acd://127.0.0.1:{}?pin={}#{}", f.port(), f.pin(), TOKEN);
    let ib = inbox.clone();
    let result = tokio::task::spawn_blocking(move || ib.pair_machine(&line)).await.unwrap();
    assert!(matches!(result, Err(MachineError::SecretWrite(ref e)) if e.kind() == SecretErrorKind::Unknown));
    assert!(inbox.conn_stamp("m1").is_none(), "fail closed");
    let (s2, ib) = (s.clone(), inbox.clone());
    let retried = tokio::task::spawn_blocking(move || {
        s2.refresh(&["m1".into()]);
        ib.reconnect_machine("m1")
    })
    .await
    .unwrap();
    assert!(retried.is_err(), "the retry's read cannot run behind the hung write");
    assert!(inbox.conn_stamp("m1").is_none());
    assert_eq!(inbox.machines()[0].secret, SecretStatus::Unknown);
}

#[test]
fn mark_unknown_makes_the_key_unknown() {
    let backend = FakeBackend::default();
    backend.0.lock().unwrap().items.insert("k".into(), "v".into());
    let s = store(&backend, QUICK);
    s.prefill(&["k".into()]);
    s.mark_unknown("k");
    assert_eq!(s.status("k"), SecretStatus::Unknown);
    s.refresh(&["k".into()]);
    assert_eq!(s.status("k"), SecretStatus::Unreadable, "\"v\" is not a machine secret, but it was read");
}

#[test]
fn a_refresh_that_read_old_credentials_never_overwrites_a_re_pairing() {
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    let backend = FakeBackend::default();
    backend.0.lock().unwrap().items.insert("m1".into(), "old".into());
    let s = store(&backend, WorkerTimeouts::LIVE);
    s.prefill(&["m1".into()]);
    let (inside, released) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
    let (i, r) = (inside.clone(), released.clone());
    s.set_read_hook(Arc::new(move || {
        i.store(true, SeqCst);
        while !r.load(SeqCst) {
            std::thread::yield_now();
        }
    }));
    struct Release(Arc<AtomicBool>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.store(true, SeqCst);
        }
    }
    let release = Release(released);
    let refreshing = {
        let s = s.clone();
        std::thread::spawn(move || s.refresh(&["m1".into()]))
    };
    while !inside.load(SeqCst) {
        std::thread::yield_now();
    }
    s.set("m1", "new").unwrap();
    drop(release);
    refreshing.join().unwrap();
    assert_eq!(s.get("m1").unwrap().as_deref(), Some("new"), "the refresh's old value was dropped");
}

#[test]
fn a_change_that_needs_a_prompt_is_refused_as_locked() {
    let backend = FakeBackend::default();
    backend.0.lock().unwrap().prompts = true;
    let s = store(&backend, QUICK);
    assert_eq!(s.set("k", "v").unwrap_err().kind(), SecretErrorKind::Locked);
    assert!(backend.0.lock().unwrap().items.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unconfirmed_rollback_is_reported_and_never_guessed() {
    let f = FakeServer::start(TOKEN, acd_client::fake::Identity::Paired).await;
    let d = tempfile::tempdir().unwrap();
    let blocker = d.path().join("file");
    std::fs::write(&blocker, "block").unwrap();
    // The write succeeds, the settings save fails, the rollback delete hangs.
    let backend = FakeBackend::default();
    backend.0.lock().unwrap().hang_deletes = Hang::Forever;
    let s = store(&backend, QUICK);
    let inbox = Inbox::new(
        Settings { machines: vec![], ..Settings::fresh() },
        s,
        InboxOptions { timing: fast(), settings_path: Some(blocker.join("settings.json")), client_name: "t".into(), client_version: "0".into() },
    );
    let line = format!("acd://127.0.0.1:{}?pin={}#{}", f.port(), f.pin(), TOKEN);
    let result = tokio::task::spawn_blocking(move || inbox.pair_machine(&line)).await.unwrap();
    assert!(matches!(result, Err(MachineError::PairRecoveryFailed)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_after_prefill_never_waits_on_the_keyring() {
    let backend = FakeBackend::default();
    backend.0.lock().unwrap().unavailable = true;
    let s = store(&backend, WorkerTimeouts::LIVE);
    let f = FakeServer::start(TOKEN, acd_client::fake::Identity::Paired).await;
    let keys = vec!["m1".to_owned()];
    let prefill = s.clone();
    tokio::task::spawn_blocking(move || prefill.prefill(&keys)).await.unwrap();
    let before = calls(&backend);
    let inbox = Inbox::new(
        box_settings(&f),
        s.clone(),
        InboxOptions { timing: fast(), settings_path: None, client_name: "t".into(), client_version: "0".into() },
    );
    inbox.start();
    let svc = Service::new(inbox.clone(), Prefs::default(), None, s.clone(), ServerOptions::default());
    svc.publish_settings().await;
    assert_eq!(inbox.machines()[0].secret, SecretStatus::Unreadable);
    assert_eq!(calls(&backend), before, "start and the settings snapshot used the cache only");
}

/// A rig whose inbox reads a `WorkerStore` over a locked fake keyring that
/// holds m1's secret.
async fn locked_rig(behaviour: UnlockBehaviour, timeouts: WorkerTimeouts) -> (Rig, FakeBackend) {
    let backend = FakeBackend::default();
    let b = backend.clone();
    let rig = rig_with(RigOptions {
        timing: held(),
        connect: false,
        secrets: Some(Box::new(move |f: &FakeServer| {
            {
                let mut k = b.0.lock().unwrap();
                k.items.insert("m1".into(), box_secret(f));
                k.locked = true;
                k.unlock = behaviour;
            }
            let s = store(&b, timeouts);
            s.prefill(&["m1".into()]);
            (s.clone() as Arc<dyn SecretStore>, s as Arc<dyn SecretControl>)
        })),
        ..Default::default()
    })
    .await;
    (rig, backend)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_locked_keyring_at_start_unlocks_and_reconnects_without_restart() {
    let (rig, _backend) = locked_rig(UnlockBehaviour::Succeed, WorkerTimeouts::LIVE).await;
    let mut v = View::connect(&rig.socket).await;
    let (_, settings, _) = v.greeting().await;
    assert_eq!(settings["machines"][0]["secret"], "locked");
    assert!(rig.inbox.conn_stamp("m1").is_none(), "no connection without the secret");
    assert_eq!(v.cmd(json!({"type":"unlock_secrets"})).await["type"], "ok");
    let c = rig.f.next_connection().await;
    c.send(json!({"type":"snapshot","epoch":"e1","seq":1,"agents":[pending_agent("a1","r1")]})).await;
    v.recv_until(|m| m["type"] == "settings" && m["machines"][0]["secret"] == "ok").await;
    v.recv_until(|m| m["type"] == "view" && m["view"]["cards"][0]["answerable"] == true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dismissed_or_slow_prompt_changes_nothing() {
    let (rig, _b) = locked_rig(UnlockBehaviour::Dismiss, QUICK).await;
    let mut v = View::connect(&rig.socket).await;
    v.greeting().await;
    assert_eq!(v.cmd(json!({"type":"unlock_secrets"})).await["code"], "secret_store_locked");
    let (rig2, _b2) = locked_rig(UnlockBehaviour::Hang, QUICK).await;
    let mut w = View::connect(&rig2.socket).await;
    w.greeting().await;
    assert_eq!(w.cmd(json!({"type":"unlock_secrets"})).await["code"], "secret_store_timeout");
    assert!(rig.inbox.conn_stamp("m1").is_none() && rig2.inbox.conn_stamp("m1").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancel_is_answered_while_an_unlock_prompt_waits() {
    let backend = FakeBackend::default();
    let b = backend.clone();
    let rig = rig_with(RigOptions {
        timing: held(),
        secrets: Some(Box::new(move |f: &FakeServer| {
            {
                let mut k = b.0.lock().unwrap();
                k.items.insert("m1".into(), box_secret(f));
                k.unlock = UnlockBehaviour::Hang;
            }
            // A short unlock limit: runtime teardown waits for the blocked thread.
            let s = store(&b, WorkerTimeouts { op: Duration::from_secs(5), unlock: Duration::from_secs(2) });
            s.prefill(&["m1".into()]);
            (s.clone() as Arc<dyn SecretStore>, s as Arc<dyn SecretControl>)
        })),
        ..Default::default()
    })
    .await;
    let mut v = View::connect(&rig.socket).await;
    v.greeting().await;
    let r = v.cmd(json!({"type":"pick","card":"m1/a1","request":"r1","option":"allow","revision":1})).await;
    let id = r["result"].as_str().unwrap().to_owned();
    v.send(&json!({"type":"unlock_secrets","id":"u1"})).await;
    let cancel = tokio::time::timeout(Duration::from_secs(1), v.cmd(json!({"type":"cancel","action":id})))
        .await
        .expect("cancel waited for the keyring prompt");
    assert_eq!(cancel["result"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retry_reads_a_cached_failure_again() {
    let backend = FakeBackend::default();
    let b = backend.clone();
    let rig = rig_with(RigOptions {
        timing: held(),
        connect: false,
        secrets: Some(Box::new(move |f: &FakeServer| {
            {
                let mut k = b.0.lock().unwrap();
                k.items.insert("m1".into(), box_secret(f));
                k.unavailable = true;
            }
            let s = store(&b, QUICK);
            s.prefill(&["m1".into()]);
            (s.clone() as Arc<dyn SecretStore>, s as Arc<dyn SecretControl>)
        })),
        ..Default::default()
    })
    .await;
    backend.0.lock().unwrap().unavailable = false;
    let mut v = View::connect(&rig.socket).await;
    v.greeting().await;
    assert_eq!(v.cmd(json!({"type":"retry_machine","machine":"m1"})).await["type"], "ok");
    rig.f.next_connection().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retry_of_an_unknown_machine_reads_nothing() {
    let backend = FakeBackend::default();
    let b = backend.clone();
    let rig = rig_with(RigOptions {
        timing: held(),
        connect: false,
        secrets: Some(Box::new(move |_: &FakeServer| {
            let s = store(&b, QUICK);
            (s.clone() as Arc<dyn SecretStore>, s as Arc<dyn SecretControl>)
        })),
        ..Default::default()
    })
    .await;
    let mut v = View::connect(&rig.socket).await;
    v.greeting().await;
    let before = calls(&backend);
    let r = v.cmd(json!({"type":"retry_machine","machine":"junk"})).await;
    assert_eq!(r["code"], "unknown_machine");
    assert_eq!(calls(&backend), before, "a junk key never reaches the keyring");
}

#[test]
fn a_dead_worker_is_unavailable_for_reads_and_ambiguous_for_changes() {
    use acd_inbox::secrets::{BackendError, WorkerError};
    // A change whose worker dies mid-job: it may have run.
    let backend = FakeBackend::default();
    let worker = SecretWorker::spawn(backend.clone(), QUICK);
    backend.0.lock().unwrap().panic_next = true;
    assert_eq!(worker.write("k", "w"), Err(WorkerError::Ambiguous));
    // Later jobs never reach a worker.
    assert_eq!(worker.read("k"), Err(WorkerError::Backend(BackendError::Unavailable)));
    // A read whose worker dies: it never produced anything.
    let backend = FakeBackend::default();
    let worker = SecretWorker::spawn(backend.clone(), QUICK);
    backend.0.lock().unwrap().panic_next = true;
    assert_eq!(worker.read("k"), Err(WorkerError::Backend(BackendError::Unavailable)));
}

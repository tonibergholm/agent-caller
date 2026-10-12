mod common;

use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;

use acd_client::voice::ActionOutcome;
use acd_inbox::commands;
use acd_inbox::prefs::Prefs;
use acd_inbox::server::{CloseReason, NoKeyring, ServerOptions, Service};
use common::*;

async fn service(timing: acd_client::Timing) -> (acd_client::fake::FakeServer, Arc<acd_client::inbox::Inbox>, Arc<Service>) {
    let (f, _c, inbox) = inbox_rig(Some(vec![pending_agent("a1", "r1")]), timing, None).await;
    let svc = Service::new(inbox.clone(), Prefs::default(), None, Arc::new(NoKeyring), ServerOptions::default());
    (f, inbox, svc)
}

const PICK: &[u8] = br#"{"type":"pick","id":"p1","card":"m1/a1","request":"r1","option":"allow","revision":1}"#;

/// The hook parks every admitted command inside the admission lock until
/// the `Release` guard is dropped, panics included, so a failed assertion
/// never leaves a thread parked.
fn park(svc: &Service) -> (Arc<AtomicBool>, Release) {
    let inside = Arc::new(AtomicBool::new(false));
    let released = Arc::new(AtomicBool::new(false));
    let (i, r) = (inside.clone(), released.clone());
    svc.set_admission_hook(Arc::new(move || {
        i.store(true, SeqCst);
        while !r.load(SeqCst) {
            std::thread::yield_now();
        }
    }));
    (inside, Release(released))
}

struct Release(Arc<AtomicBool>);

impl Drop for Release {
    fn drop(&mut self) {
        self.0.store(true, SeqCst);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_close_during_a_pick_cancels_the_new_action() {
    let (f, inbox, svc) = service(held()).await;
    let mut rx = inbox.action_outcomes();
    let conn = svc.connect_for_test();
    let (inside, release) = park(&svc);
    let picking = {
        let (svc, conn) = (svc.clone(), conn.clone());
        tokio::task::spawn_blocking(move || commands::handle(&svc, &conn, PICK).unwrap())
    };
    until("the pick inside the admission lock", || inside.load(SeqCst)).await;
    let closing = {
        let (svc, conn) = (svc.clone(), conn.clone());
        tokio::task::spawn_blocking(move || svc.close(&conn, CloseReason::Eof))
    };
    until("the close to wait for the admission lock", || conn.gate_waiters_for_test() == 1).await;
    drop(release);
    picking.await.unwrap();
    closing.await.unwrap();
    let event = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();
    assert_eq!(event.outcome, ActionOutcome::Cancelled, "the action created before the close was cancelled by it");
    inbox.expire_countdowns_for_test();
    view_until(&inbox, |v| v.actions.is_empty()).await;
    assert_eq!(answers(&f), 0);
}

#[tokio::test]
async fn a_command_after_close_is_refused() {
    let (_f, inbox, svc) = service(held()).await;
    let conn = svc.connect_for_test();
    svc.close(&conn, CloseReason::Eof);
    commands::handle(&svc, &conn, PICK).unwrap();
    commands::handle(&svc, &conn, br#"{"type":"panel","id":"p2","open":true}"#).unwrap();
    assert!(inbox.stamped_view().view.actions.is_empty() && !*svc.panel_open().borrow());
    view_until(&inbox, |v| v.actions.is_empty()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_late_panel_open_cannot_stick_after_close() {
    let (_f, _inbox, svc) = service(held()).await;
    let conn = svc.connect_for_test();
    let (inside, release) = park(&svc);
    let opening = {
        let (svc, conn) = (svc.clone(), conn.clone());
        tokio::task::spawn_blocking(move || {
            commands::handle(&svc, &conn, br#"{"type":"panel","id":"o1","open":true}"#).unwrap()
        })
    };
    until("panel(open) inside the admission lock", || inside.load(SeqCst)).await;
    let closing = {
        let (svc, conn) = (svc.clone(), conn.clone());
        tokio::task::spawn_blocking(move || svc.close(&conn, CloseReason::Eof))
    };
    until("the close to wait", || conn.gate_waiters_for_test() == 1).await;
    drop(release);
    opening.await.unwrap();
    closing.await.unwrap();
    assert!(!*svc.panel_open().borrow(), "the panel flag of a closed view was left set");
}

#[tokio::test]
async fn ids_are_remembered_up_to_a_bound() {
    let (_f, inbox, _svc) = service(held()).await;
    let svc = Service::new(
        inbox.clone(),
        Prefs::default(),
        None,
        Arc::new(NoKeyring),
        ServerOptions { remembered_ids: 2, ..Default::default() },
    );
    let conn = svc.connect_for_test();
    for id in ["a", "b", "a", "c", "a"] {
        commands::handle(&svc, &conn, format!(r#"{{"type":"escape","id":"{id}"}}"#).as_bytes()).unwrap();
    }
    let codes: Vec<String> = std::iter::from_fn(|| conn.outbox.try_next_for_test())
        .skip(3) // hello, settings, view
        .map(|l| serde_json::from_str::<serde_json::Value>(&l).unwrap()["code"].as_str().unwrap_or("").to_owned())
        .collect();
    assert_eq!(codes, ["", "", "duplicate_id", "", ""],
        "a repeat within the last 2 ids is refused; an older one is forgotten");
}

#[tokio::test]
async fn connection_owners_start_at_one_and_a_close_spares_unowned_actions() {
    use acd_client::actions::ActionOwner;
    let (_f, inbox, svc) = service(held()).await;
    let conn = svc.connect_for_test();
    let other = svc.connect_for_test();
    assert_eq!((conn.owner, other.owner), (ActionOwner(1), ActionOwner(2)));
    assert_ne!(conn.owner, ActionOwner::DEFAULT, "owner 0 is shared with the Mac app and voice");
    // A voice or app action carries the default owner.
    inbox.pick_owned(ActionOwner::DEFAULT, "m1/a1", "r1", "allow", 1).unwrap();
    view_until(&inbox, |v| v.actions.len() == 1).await;
    svc.close(&conn, CloseReason::Eof);
    svc.close(&other, CloseReason::Eof);
    assert_eq!(inbox.stamped_view().view.actions.len(), 1, "a disconnect cancelled an unowned action");
    inbox.expire_countdowns_for_test();
    view_until(&inbox, |v| v.actions.is_empty()).await;
}

#[tokio::test]
async fn default_options_remember_1024_ids() {
    let (_f, _inbox, svc) = service(held()).await;
    assert_eq!(svc.opts.remembered_ids, 1024);
    let conn = svc.connect_for_test();
    // hello, settings and view come first; the reply queue holds 1024.
    for _ in 0..3 {
        conn.outbox.try_next_for_test().unwrap();
    }
    let send = |id: String| commands::handle(&svc, &conn, format!(r#"{{"type":"escape","id":"{id}"}}"#).as_bytes()).unwrap();
    for i in 0..1025 {
        send(format!("i{i}"));
        conn.outbox.try_next_for_test().unwrap();
    }
    send("i1".into()); // among the last 1024
    send("i0".into()); // forgotten
    let code = |l: String| serde_json::from_str::<serde_json::Value>(&l).unwrap()["code"].as_str().unwrap_or("").to_owned();
    let codes: Vec<String> = std::iter::from_fn(|| conn.outbox.try_next_for_test()).map(code).collect();
    assert_eq!(codes, ["duplicate_id", ""]);
}

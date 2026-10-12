mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::time::Duration;

use acd_client::settings::SecretError;
use acd_inbox::server::SecretControl;
use common::*;
use serde_json::{Value, json};

fn code(r: Value) -> String {
    assert_eq!(r["type"], "error", "{r}");
    r["code"].as_str().unwrap().to_owned()
}

fn action_id(r: Value) -> String {
    assert_eq!(r["type"], "ok", "{r}");
    r["result"].as_str().unwrap().to_owned()
}

async fn greeted(rig: &Rig) -> View {
    let mut v = View::connect(&rig.socket).await;
    v.greeting().await;
    v
}

fn pick(card: &str, request: &str, option: &str) -> Value {
    json!({"type":"pick","card":card,"request":request,"option":option,"revision":1})
}

#[tokio::test]
async fn pick_returns_the_action_id_and_the_countdown_fires() {
    let rig = rig(vec![pending_agent("a1", "r1")], fast()).await;
    let mut v = greeted(&rig).await;
    let id = action_id(v.cmd(pick("m1/a1", "r1", "allow")).await);
    let fired = v.recv_until(|m| m["type"] == "outcome" && m["action"] == id.as_str()).await;
    assert_eq!(fired["card"], "m1/a1");
    assert_eq!(fired["result"], json!({"kind":"fired"}));
    frame_of_type(&rig.f, "request.answer").await;
}

#[tokio::test]
async fn stale_unknown_and_bad_input_get_their_codes() {
    let rig = rig(vec![pending_agent("a1", "r1")], held()).await;
    let mut v = greeted(&rig).await;
    let stale = json!({"type":"pick","card":"m1/a1","request":"r1","option":"allow","revision":2});
    assert_eq!(code(v.cmd(stale).await), "stale");
    assert_eq!(code(v.cmd(pick("m1/zz", "r1", "allow")).await), "unknown_card");
    assert_eq!(code(v.cmd(pick("m1/a1", "r1", "nope")).await), "unknown_option");
    assert_eq!(code(v.cmd(json!({"type":"pick","card":"m1/a1"})).await), "bad_request");
    let blank = json!({"type":"answer_text","card":"m1/a1","request":"r1","revision":1,"text":"   "});
    assert_eq!(code(v.cmd(blank).await), "bad_text");
    assert_eq!(code(v.cmd(json!({"type":"reply","card":"m1/a1","text":""})).await), "bad_text");
    assert_eq!(code(v.cmd(json!({"type":"mark_seen","card":"m1/a1"})).await), "cannot_mark_seen");
    assert_eq!(code(v.cmd(json!({"type":"rename_machine","machine":"nope","name":"x"})).await), "unknown_machine");
    assert_eq!(code(v.cmd(json!({"type":"rename_machine","machine":"m1","name":"  "})).await), "bad_name");
    assert_eq!(code(v.cmd(json!({"type":"set_show","what":"everything","on":true})).await), "bad_request");
    assert_eq!(code(v.cmd(json!({"type":"retry_machine","machine":"nope"})).await), "unknown_machine");
    assert_eq!(v.cmd(json!({"type":"escape"})).await["result"], false, "nothing was counting");
}

#[tokio::test]
async fn a_repeated_id_does_nothing() {
    let rig = rig(vec![pending_agent("a1", "r1")], held()).await;
    let mut v = greeted(&rig).await;
    v.send(&json!({"type":"pick","id":"same","card":"m1/a1","request":"r1","option":"allow","revision":1})).await;
    let first = v.recv_until(|m| m["id"] == "same").await;
    let id = action_id(first);
    v.send(&json!({"type":"cancel","id":"same","action":id})).await;
    let second = v.recv_until(|m| m["id"] == "same").await;
    assert_eq!(second["code"], "duplicate_id");
    view_until(&rig.inbox, |view| view.actions.len() == 1 && view.actions[0].id == id).await;
}

#[tokio::test]
async fn esc_is_per_view_and_the_cancel_control_cancels_any_views_action() {
    let rig = rig(vec![pending_agent("a1", "r1"), pending_agent("a2", "r2")], held()).await;
    let mut a = greeted(&rig).await;
    let mut b = greeted(&rig).await;
    let ida = action_id(a.cmd(pick("m1/a1", "r1", "allow")).await);
    let idb = action_id(b.cmd(pick("m1/a2", "r2", "allow")).await);
    assert_eq!(a.cmd(json!({"type":"escape"})).await["result"], true);
    let o = b.recv_until(|m| m["type"] == "outcome" && m["action"] == ida.as_str()).await;
    assert_eq!(o["result"]["kind"], "cancelled");
    assert_eq!(a.cmd(json!({"type":"escape"})).await["result"], false, "b's countdown is not a's");
    assert_eq!(a.cmd(json!({"type":"cancel","action":idb})).await["result"], true);
    let o = b.recv_until(|m| m["type"] == "outcome" && m["action"] == idb.as_str()).await;
    assert_eq!(o["result"]["kind"], "cancelled");
}

#[tokio::test]
async fn a_second_view_cannot_pick_a_card_that_is_counting() {
    let rig = rig(vec![pending_agent("a1", "r1")], held()).await;
    let mut a = greeted(&rig).await;
    let mut b = greeted(&rig).await;
    let ida = action_id(a.cmd(pick("m1/a1", "r1", "allow")).await);
    assert_eq!(code(b.cmd(pick("m1/a1", "r1", "deny")).await), "already_counting");
    view_until(&rig.inbox, |view| view.actions.len() == 1 && view.actions[0].id == ida).await;
}

#[tokio::test]
async fn the_panel_flag_follows_the_view_and_clears_on_disconnect() {
    let rig = rig(vec![pending_agent("a1", "r1")], fast()).await;
    let mut open = rig.svc.panel_open();
    let mut a = greeted(&rig).await;
    assert_eq!(a.cmd(json!({"type":"panel","open":true})).await["type"], "ok");
    tokio::time::timeout(Duration::from_secs(2), open.wait_for(|o| *o)).await.unwrap().unwrap();
    drop(a);
    tokio::time::timeout(Duration::from_secs(2), open.wait_for(|o| !*o)).await.unwrap().unwrap();
}

#[tokio::test]
async fn settings_follow_show_toggles_and_the_notification_switch() {
    let rig = rig(vec![pending_agent("a1", "r1")], fast()).await;
    let mut v = greeted(&rig).await;
    assert_eq!(v.cmd(json!({"type":"set_show","what":"subagents","on":true})).await["type"], "ok");
    v.recv_until(|m| m["type"] == "settings" && m["show_subagents"] == true).await;
    assert_eq!(v.cmd(json!({"type":"set_notifications","on":false})).await["type"], "ok");
    v.recv_until(|m| m["type"] == "settings" && m["notifications"] == false).await;
    assert!(!rig.svc.prefs().borrow().notifications);
}

#[tokio::test]
async fn a_bad_pairing_string_is_never_echoed() {
    let rig = rig(vec![pending_agent("a1", "r1")], fast()).await;
    let mut v = greeted(&rig).await;
    let secretish = format!("acd://host:0?pin=sha256:XYZ#{TOKEN}");
    let r = v.cmd(json!({"type":"pair","pairing":secretish})).await;
    assert_eq!(r["code"], "bad_pairing");
    assert!(!r.to_string().contains(TOKEN), "the reply repeats the pairing string");
}

#[tokio::test]
async fn a_literal_lan_address_is_refused_with_bad_pairing_and_saved_nothing() {
    let rig = rig(vec![pending_agent("a1", "r1")], fast()).await;
    let mut v = greeted(&rig).await;
    let before = rig.inbox.settings().machines.len();
    let lan = format!("acd://192.168.1.10:47800?pin=sha256:{}#{TOKEN}", "A".repeat(43));
    let r = v.cmd(json!({"type":"pair","pairing":lan})).await;
    // The wire code stays `bad_pairing`: a new code is a protocol change.
    assert_eq!(r["code"], "bad_pairing");
    assert!(!r.to_string().contains("192.168.1.10"), "the reply names the address");
    assert!(!r.to_string().contains(TOKEN), "the reply repeats the pairing string");
    assert_eq!(rig.inbox.settings().machines.len(), before, "a refused line saved a machine");
}

/// An unlock that waits until released, as a keyring prompt does.
#[derive(Default)]
struct SlowUnlock {
    entered: AtomicBool,
    release: AtomicBool,
}

impl SecretControl for SlowUnlock {
    fn unlock(&self) -> Result<(), SecretError> {
        self.entered.store(true, SeqCst);
        while !self.release.load(SeqCst) {
            std::thread::yield_now();
        }
        Ok(())
    }
    fn refresh(&self, _: &[String]) {}
}

/// Releases the unlock on drop, panics included.
struct Unblock(Arc<SlowUnlock>);

impl Drop for Unblock {
    fn drop(&mut self) {
        self.0.release.store(true, SeqCst);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_disconnect_during_a_slow_command_still_cancels_countdowns() {
    let control = Arc::new(SlowUnlock::default());
    let unblock = Unblock(control.clone());
    let rig = rig_with(RigOptions { timing: held(), control: control.clone(), ..Default::default() }).await;
    let mut a = greeted(&rig).await;
    let mut watcher = greeted(&rig).await;
    let id = action_id(a.cmd(pick("m1/a1", "r1", "allow")).await);
    a.send(&json!({"type":"unlock_secrets","id":"u1"})).await;
    until("the unlock to start", || control.entered.load(SeqCst)).await;
    drop(a);
    let o = watcher.recv_until(|m| m["type"] == "outcome" && m["action"] == id.as_str()).await;
    assert_eq!(o["result"]["kind"], "cancelled");
    drop(unblock);
    rig.inbox.expire_countdowns_for_test();
    view_until(&rig.inbox, |v| v.actions.is_empty()).await;
    assert_eq!(answers(&rig.f), 0);
}

#[tokio::test]
async fn pair_rename_and_remove_publish_settings() {
    let rig = rig(vec![pending_agent("a1", "r1")], fast()).await;
    let mut v = greeted(&rig).await;
    // m1's key is not a UUID, so pair a second machine, which gets one.
    let line = format!("acd://localhost:{}?pin={}#{}", rig.f.port(), rig.f.pin(), TOKEN);
    let paired = v.cmd(json!({"type":"pair","pairing":line})).await;
    assert_eq!(paired["type"], "ok", "{paired}");
    let key = paired["result"]["machine"].as_str().unwrap().to_owned();
    assert!(!paired.to_string().contains(TOKEN));
    v.recv_until(|m| m["type"] == "settings" && m["machines"].as_array().unwrap().len() == 2).await;
    let r = v.cmd(json!({"type":"rename_machine","machine":key,"name":"Dev box"})).await;
    assert_eq!(r["type"], "ok", "{r}");
    v.recv_until(|m| m["type"] == "settings" && m["machines"].as_array().unwrap().iter().any(|e| e["name"] == "Dev box")).await;
    assert_eq!(code(v.cmd(json!({"type":"rename_machine","machine":key,"name":"  "})).await), "bad_name");
    assert_eq!(v.cmd(json!({"type":"remove_machine","machine":key})).await["type"], "ok");
    v.recv_until(|m| m["type"] == "settings" && m["machines"].as_array().unwrap().len() == 1).await;
}

#[tokio::test]
async fn owned_commands_use_the_connections_owner_not_the_default() {
    let rig = rig(vec![pending_agent("a1", "r1")], held()).await;
    let mut a = greeted(&rig).await;
    let id = action_id(a.cmd(pick("m1/a1", "r1", "allow")).await);
    // `escape` and the disconnect cleanup act on this connection's owner.
    assert_eq!(a.cmd(json!({"type":"escape"})).await["result"], true);
    view_until(&rig.inbox, |v| v.actions.is_empty()).await;
    let id2 = action_id(a.cmd(pick("m1/a1", "r1", "allow")).await);
    assert_ne!(id, id2);
    drop(a);
    view_until(&rig.inbox, |v| v.actions.is_empty()).await;
}

#[tokio::test]
async fn no_reply_carries_secrets_or_message_text() {
    let rig = rig(vec![pending_agent("a1", "r1")], held()).await;
    let mut v = greeted(&rig).await;
    let secretish = format!("acd://host:0?pin=sha256:XYZ#{TOKEN}");
    let replies = [
        v.cmd(json!({"type":"pair","pairing":secretish})).await,
        v.cmd(json!({"type":"retry_machine","machine":secretish})).await,
        v.cmd(json!({"type":"remove_machine","machine":secretish})).await,
        v.cmd(json!({"type":"rename_machine","machine":secretish,"name":secretish})).await,
        v.cmd(json!({"type":"pick","card":secretish,"request":secretish,"option":secretish,"revision":1})).await,
        v.cmd(json!({"type":"reply","card":secretish,"text":secretish})).await,
        v.cmd(json!({"type":"set_show","what":secretish,"on":true})).await,
    ];
    for r in replies {
        assert_eq!(r["type"], "error", "{}", r["type"]);
        let text = r.to_string();
        assert!(!text.contains(TOKEN) && !text.contains("sha256") && r.get("message").is_none(), "{text}");
    }
}

/// The service queues a reply and the settings it caused apart, so the
/// settings may be written first. `cmd` must not lose them (CI flake).
#[tokio::test]
async fn cmd_keeps_settings_that_arrive_before_the_reply() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 256];
        let n = s.read(&mut buf).await.unwrap();
        assert!(n > 0);
        s.write_all(b"{\"type\":\"settings\",\"machines\":[]}\n{\"type\":\"ok\",\"id\":\"c1\"}\n").await.unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;
    });
    let mut v = View::connect(&path).await;
    assert_eq!(v.cmd(json!({"type":"escape"})).await["type"], "ok");
    let m = v.recv_until(|m| m["type"] == "settings").await;
    assert!(m["machines"].as_array().unwrap().is_empty());
    server.abort();
}

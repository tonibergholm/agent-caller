//! Tests added from a cargo-mutants run (docs/research/2026-11-04-mutation-testing.md).
//! A child module of `commands`, which keeps `Commands` crate-private.
use super::*;
use crate::session::EpochChange;
use std::time::Duration;

const C1: [u8; 32] = [1; 32];

fn t() -> crate::Timing {
    crate::Timing::default()
}
fn caps(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}
fn answer() -> Command {
    Command::Answer { request_id: "r".into(), revision: 7, option_id: "o".into() }
}
fn not_ready(id: &str) -> CommandResult {
    CommandResult::err(id, ErrorCode::RequestNotReady, "m")
}
fn id_of(m: &ClientMessage) -> String {
    m.command_id().unwrap().to_owned()
}

/// A command sent without the revision capability (so the frame needs none),
/// answered `request_not_ready`, so it waits to be retried.
fn waiting(t0: Instant, caps_at_start: &[&str]) -> (Commands, ClientMessage, oneshot::Receiver<Outcome>) {
    let mut c = Commands::default();
    let (tx, rx) = oneshot::channel();
    let m = c.begin(answer(), tx, &caps(caps_at_start), C1, t0).unwrap();
    c.on_result(&not_ready(&id_of(&m)), t0, &t());
    (c, m, rx)
}

/// `due` `>` -> `>=` (line 198): at exactly the window the retry still goes
/// out; a moment later it has failed with `request_not_ready`.
#[test]
fn a_retry_due_exactly_at_the_window_is_still_sent() {
    let t0 = Instant::now();
    let (mut c, m, mut rx) = waiting(t0, &["answer"]);
    assert_eq!(c.due(t0 + t().not_ready_window, &caps(&["answer"]), &t()), vec![m]);
    assert!(rx.try_recv().is_err());

    let (mut c, _m, mut rx) = waiting(t0, &["answer"]);
    let late = t0 + t().not_ready_window + Duration::from_nanos(1);
    assert!(c.due(late, &caps(&["answer"]), &t()).is_empty());
    assert!(matches!(rx.try_recv(), Ok(Outcome::Failed { code: ErrorCode::RequestNotReady, .. })));
}

/// `due` `==` -> `!=` (line 201): the capability check compares the command's capability.
#[test]
fn a_retry_needs_the_capability_it_was_sent_with() {
    let t0 = Instant::now();
    let (mut c, _m, mut rx) = waiting(t0, &["answer"]);
    assert!(c.due(t0 + Duration::from_secs(1), &caps(&["send"]), &t()).is_empty());
    assert_eq!(rx.try_recv().unwrap(), Outcome::Uncertain);
    let (mut c, m, mut rx) = waiting(t0, &["answer"]);
    assert_eq!(c.due(t0 + Duration::from_secs(1), &caps(&["answer"]), &t()), vec![m]);
    assert!(rx.try_recv().is_err());
}

/// `on_welcome` `>` -> `==` / `>=` (line 248): an in-flight command is resent
/// until it is older than `stale_inflight`, and no longer after.
#[test]
fn a_reconnect_resends_until_stale_inflight_and_no_longer() {
    let t0 = Instant::now();
    let stale = t().stale_inflight;
    let mut c = Commands::default();
    let (tx, mut rx) = oneshot::channel();
    let m = c.begin(answer(), tx, &caps(&["answer"]), C1, t0).unwrap();
    assert_eq!(c.on_welcome(EpochChange::Same, &caps(&["answer"]), t0 + stale, &t()), vec![m.clone()]);
    assert!(rx.try_recv().is_err());
    assert!(c.on_welcome(EpochChange::Same, &caps(&["answer"]), t0 + stale + Duration::from_nanos(1), &t()).is_empty());
    assert_eq!(rx.try_recv().unwrap(), Outcome::Uncertain);
}

/// A new epoch means the daemon restarted: nothing in flight is resent (safe for idempotency).
#[test]
fn a_new_epoch_revokes_everything_in_flight() {
    let t0 = Instant::now();
    let mut c = Commands::default();
    let (tx, mut rx) = oneshot::channel();
    c.begin(answer(), tx, &caps(&["answer"]), C1, t0).unwrap();
    assert!(c.on_welcome(EpochChange::New, &caps(&["answer"]), t0, &t()).is_empty());
    assert_eq!(rx.try_recv().unwrap(), Outcome::Uncertain);
}

/// `on_welcome` `>` -> `>=` (line 266): a not-ready retry at exactly the window is still resent.
#[test]
fn a_not_ready_retry_at_the_window_is_resent_on_reconnect() {
    let t0 = Instant::now();
    let (mut c, m, mut rx) = waiting(t0, &["answer"]);
    assert_eq!(c.on_welcome(EpochChange::Same, &caps(&["answer"]), t0 + t().not_ready_window, &t()), vec![m]);
    assert!(rx.try_recv().is_err());
    let (mut c, _m, mut rx) = waiting(t0, &["answer"]);
    let late = t0 + t().not_ready_window + Duration::from_nanos(1);
    assert!(c.on_welcome(EpochChange::Same, &caps(&["answer"]), late, &t()).is_empty());
    assert!(matches!(rx.try_recv(), Ok(Outcome::Failed { code: ErrorCode::RequestNotReady, .. })));
}

/// `on_welcome` `==` -> `!=` (line 275): the capability check on a not-ready retry.
#[test]
fn a_not_ready_retry_needs_its_capability_on_reconnect() {
    let t0 = Instant::now();
    let (mut c, _m, mut rx) = waiting(t0, &["answer"]);
    assert!(c.on_welcome(EpochChange::Same, &caps(&["send"]), t0 + Duration::from_secs(1), &t()).is_empty());
    assert_eq!(rx.try_recv().unwrap(), Outcome::Uncertain);
    let (mut c, m, mut rx) = waiting(t0, &["answer"]);
    assert_eq!(c.on_welcome(EpochChange::Same, &caps(&["answer"]), t0 + Duration::from_secs(1), &t()), vec![m]);
    assert!(rx.try_recv().is_err());
}

/// `Debug` for `Command` and `Outcome`: lengths only for typed text and daemon messages.
#[test]
fn debug_output_never_shows_typed_text_or_daemon_messages() {
    let shown = [
        format!("{:?}", Command::AnswerText { request_id: "r".into(), revision: 1, text: "SECRET-TEXT".into() }),
        format!("{:?}", Command::Send { agent_id: "a".into(), text: "SECRET-TEXT".into() }),
        format!("{:?}", Outcome::Failed { code: ErrorCode::BadRequest, message: "SECRET-MESSAGE".into() }),
    ];
    for s in &shown {
        assert!(!s.contains("SECRET-"), "{s}");
    }
    assert!(shown[0].contains("AnswerText") && shown[0].contains("text_bytes"));
    assert!(shown[1].contains("Send") && shown[1].contains("text_bytes"));
    assert!(shown[2].contains("Failed") && shown[2].contains("message_bytes"));
    assert!(format!("{:?}", answer()).contains("Answer") && format!("{:?}", answer()).contains("option_id"));
    assert!(format!("{:?}", Command::MarkSeen { agent_id: "a".into() }).contains("MarkSeen"));
    assert_eq!(format!("{:?}", Outcome::Delivered), "Delivered");
    assert_eq!(format!("{:?}", Outcome::Done), "Done");
    assert_eq!(format!("{:?}", Outcome::Uncertain), "Uncertain");
    assert!(format!("{:?}", Outcome::NotSent(NotSent::Offline)).contains("NotSent"));
}

mod common;

use acd_client::voice::{ActionOutcome, NotSentReason as ClientReason};
use acd_voice::plan::{Enqueue, EnqueueError, Guard, InboxPort, NotSentReason, VoiceOutcome};
use acd_voice::text::VoiceText;
use acd_voice_client::{AcdPort, freeze, voice_outcome};
use common::*;

#[tokio::test]
async fn the_port_enqueues_only_on_the_state_the_step_was_planned_on() {
    let r = inbox_rig(vec![pending_agent()]).await;
    let port = AcdPort::new(r.inbox.clone());
    let snap = freeze(&r.inbox.stamped_view(), shown(Some("m1/a1"), &["m1/a1"]), true);
    let card = snap.card("m1/a1").unwrap();
    let guard = Guard { conn: card.conn.unwrap(), agent_id: "a1".into(), request: Some(("r1".into(), 1)) };
    let stale = Guard { request: Some(("r1".into(), 9)), ..guard.clone() };
    let pick = |guard| Enqueue::Pick { card: "m1/a1".into(), option_id: "allow".into(), guard };
    assert_eq!(port.enqueue(pick(stale)), Err(EnqueueError::Stale));
    let id = port.enqueue(pick(guard)).unwrap();
    assert!(port.cancel(&id));
    assert!(!port.cancel(&id));
}

/// D7: a named send reaches an idle agent the panel hides; its card
/// shows while the countdown runs.
#[tokio::test]
async fn a_send_reaches_an_idle_agent_the_panel_hides() {
    let r = inbox_rig(vec![pending_agent(), idle_agent()]).await;
    let snap = freeze(&r.inbox.stamped_view(), shown(None, &["m1/a1"]), false);
    let idle = snap.card("m1/a3").expect("frozen although hidden");
    assert!(idle.idle_hidden);
    let guard = Guard { conn: idle.conn.unwrap(), agent_id: "a3".into(), request: None };
    let port = AcdPort::new(r.inbox.clone());
    port.enqueue(Enqueue::Reply { card: "m1/a3".into(), text: VoiceText::new("run the tests"), guard }).unwrap();
    let sent = frame(&r.f, "agent.send").await;
    assert_eq!((sent["agent_id"].as_str(), sent["text"].as_str()), (Some("a3"), Some("run the tests")));
}

#[test]
fn outcomes_map_one_to_one() {
    use acd_protocol::ErrorCode;
    for (client, voice) in [
        (ActionOutcome::Fired, VoiceOutcome::Fired),
        (ActionOutcome::Delivered, VoiceOutcome::Delivered),
        (ActionOutcome::Rejected { code: ErrorCode::RequestGone }, VoiceOutcome::Rejected { code: ErrorCode::RequestGone }),
        (ActionOutcome::Uncertain, VoiceOutcome::Uncertain),
        (ActionOutcome::Cancelled, VoiceOutcome::Cancelled),
        (ActionOutcome::NotSent { reason: ClientReason::Hidden }, VoiceOutcome::NotSent { reason: NotSentReason::Hidden }),
        (ActionOutcome::NotSent { reason: ClientReason::AnsweredElsewhere }, VoiceOutcome::NotSent { reason: NotSentReason::AnsweredElsewhere }),
    ] {
        assert_eq!(voice_outcome(client), voice);
    }
}

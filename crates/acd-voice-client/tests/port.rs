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
    let guard = Guard { conn: card.conn.unwrap(), agent_id: "a1".into(), request: Some(("r1".into(), 1)), named_send_only: false };
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
    let guard = Guard { conn: idle.conn.unwrap(), agent_id: "a3".into(), request: None, named_send_only: true };
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

fn free_text_agent() -> serde_json::Value {
    let mut a = pending_agent();
    a["pending"]["free_text"] = true.into();
    a
}

/// Task 10 review: the text answer is guarded like a pick.
#[tokio::test]
async fn answer_text_is_refused_on_a_stale_revision() {
    let r = inbox_rig(vec![free_text_agent()]).await;
    let port = AcdPort::new(r.inbox.clone());
    let snap = freeze(&r.inbox.stamped_view(), shown(Some("m1/a1"), &["m1/a1"]), true);
    let conn = snap.card("m1/a1").unwrap().conn.unwrap();
    let guard = Guard { conn, agent_id: "a1".into(), request: Some(("r1".into(), 1)), named_send_only: false };
    let answer = |guard| Enqueue::AnswerText { card: "m1/a1".into(), text: VoiceText::new("use main"), guard };
    let stale = Guard { request: Some(("r1".into(), 9)), ..guard.clone() };
    assert_eq!(port.enqueue(answer(stale)), Err(EnqueueError::Stale));
    let id = port.enqueue(answer(guard)).unwrap();
    assert!(port.cancel(&id));
}

/// Task 10 review: a guard stamped on another connection is refused, for
/// every kind of step.
#[tokio::test]
async fn a_guard_from_another_connection_is_refused() {
    let r = inbox_rig(vec![free_text_agent()]).await;
    let port = AcdPort::new(r.inbox.clone());
    let snap = freeze(&r.inbox.stamped_view(), shown(Some("m1/a1"), &["m1/a1"]), true);
    let conn = snap.card("m1/a1").unwrap().conn.unwrap();
    let other_conn = acd_voice::snapshot::ConnStamp { conn_gen: conn.conn_gen + 1, ..conn };
    let guard = Guard { conn: other_conn, agent_id: "a1".into(), request: Some(("r1".into(), 1)), named_send_only: false };
    let card = || "m1/a1".to_owned();
    for op in [
        Enqueue::Pick { card: card(), option_id: "allow".into(), guard: guard.clone() },
        Enqueue::AnswerText { card: card(), text: VoiceText::new("x"), guard: guard.clone() },
        Enqueue::Reply { card: card(), text: VoiceText::new("x"), guard: guard.clone() },
    ] {
        assert_eq!(port.enqueue(op), Err(EnqueueError::ConnectionChanged));
    }
}

/// Final review B2: agent text and voice text never show in `Debug`, from
/// the frozen snapshot through the HUD and the inbox action to the wire.
#[tokio::test]
async fn debug_never_shows_content_from_snapshot_to_wire() {
    use acd_client::actions::{Action, ActionKind, Semantics};
    use acd_client::view::ActionView;
    use acd_protocol::ClientMessage;
    use acd_voice::fast::Locale;
    use acd_voice::plan::Executor;
    use acd_voice::router::{RouteOptions, route};

    let mut a = pending_agent();
    a["title"] = "SECRET-TITLE".into();
    a["project"] = "SECRET-PROJECT".into();
    a["pending"]["title"] = "SECRET-REQUEST".into();
    a["pending"]["detail"] = "SECRET-DETAIL".into();
    a["pending"]["options"][0]["label"] = "SECRET-YES".into();
    a["pending"]["options"][2]["label"] = "SECRET-ALWAYS".into();
    a["pending"]["options"][2]["description"] = "SECRET-RULE".into();
    let r = inbox_rig(vec![a.clone()]).await;
    let snap = freeze(&r.inbox.stamped_view(), shown(Some("m1/a1"), &["m1/a1"]), true);
    let port = AcdPort::new(r.inbox.clone());
    let mut exec = Executor::new();
    let said = VoiceText::new("yes always");
    let (n, thinking) = exec.begin(&port, snap.clone(), said.clone(), Locale::En).unwrap();
    let routed = route(&snap, &said, None, &RouteOptions::default()).await;
    let confirm = exec.routed(n, routed).unwrap();
    let id = confirm.hud.awaiting_display().expect("a confirm step");
    exec.shown(&port, id);
    let countdown = exec.confirm(&port, Some(id)).unwrap();

    let req: acd_protocol::Request = serde_json::from_value(a["pending"].clone()).unwrap();
    let sem = Semantics::of(&req, "always").unwrap();
    let action = Action {
        id: "act".into(),
        owner: acd_client::actions::ActionOwner::DEFAULT,
        card_key: "m1/a1".into(),
        machine_key: "m1".into(),
        agent_id: "a1".into(),
        instance: 1,
        conn_gen: 1,
        kind: ActionKind::Answer(sem.clone()),
        fires_at: std::time::Instant::now(),
        view: ActionView {
            id: "act".into(),
            card_key: "m1/a1".into(),
            label: sem.option.label.clone(),
            detail: sem.option.description.clone(),
            fires_at_ms: 0.0,
        },
    };
    let wire = [
        ClientMessage::RequestAnswer {
            id: "c1".into(),
            request_id: "r1".into(),
            revision: Some(1),
            option_id: None,
            text: Some("SECRET-ANSWER".into()),
        },
        ClientMessage::AgentSend { id: "c2".into(), agent_id: "a1".into(), text: "SECRET-SEND".into() },
        ClientMessage::Hello {
            protocol: "acd/1".into(),
            token: "SECRET-TOKEN".into(),
            client: acd_protocol::ClientInfo { id: "c".into(), name: "n".into(), version: "0".into() },
            resume: None,
        },
    ];
    let shown = [
        format!("{snap:?}"),
        format!("{thinking:?}"),
        format!("{confirm:?}"),
        format!("{countdown:?}"),
        format!("{exec:?}"),
        format!("{sem:?}"),
        format!("{action:?}"),
        format!("{wire:?}"),
    ];
    for s in &shown {
        assert!(!s.contains("SECRET"), "{s}");
    }
    assert!(shown[2].contains("Confirm"), "the HUD is the confirm step: {}", shown[2]);
    assert!(shown[7].contains("request_id: \"r1\""), "ids stay readable: {}", shown[7]);
}

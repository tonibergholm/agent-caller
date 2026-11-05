//! Coverage-driven tests (docs/research/2026-11-05-coverage.md).
use std::time::Instant;

use acd_client::actions::{check, check_reason, notice_for, Action, ActionKind, ActionOwner};
use acd_client::conn::{MachineState, MachineStatus};
use acd_client::session::Session;
use acd_client::view::{ActionView, NoticeTone};
use acd_client::voice::NotSentReason;

#[test]
fn every_not_sent_reason_has_a_notice_that_says_nothing_was_sent_or_why() {
    use NoticeTone::{Info, Warning};
    use NotSentReason::*;
    let cases = [
        (Offline, "Offline. Nothing was sent.", Warning),
        (NotSupported, "Not supported for this agent", Info),
        (AgentGone, "The agent is gone", Info),
        (AnsweredElsewhere, "Answered elsewhere", Info),
        (Changed, "The request changed. Nothing was sent.", Warning),
        (AgentBusy, "The agent is busy. Try again when it is idle.", Warning),
        (Hidden, "Hidden. Nothing was sent.", Info),
    ];
    for (reason, text, tone) in cases {
        let n = notice_for(reason);
        assert_eq!((n.text.as_str(), n.tone), (text, tone), "{reason:?}");
    }
}

fn reply_action(instance: u64, conn_gen: u64) -> Action {
    Action {
        id: "a1".into(),
        owner: ActionOwner::DEFAULT,
        card_key: "c".into(),
        machine_key: "m".into(),
        agent_id: "agent-1".into(),
        instance,
        conn_gen,
        kind: ActionKind::Reply { text: "hello".into() },
        fires_at: Instant::now(),
        view: ActionView {
            id: "a1".into(),
            card_key: "c".into(),
            label: "Reply".into(),
            detail: None,
            fires_at_ms: 0.0,
        },
    }
}

fn state(instance: u64, conn_gen: u64, status: MachineStatus, caps: &[&str]) -> MachineState {
    let mut session = Session::default();
    session.capabilities = caps.iter().map(|c| c.to_string()).collect();
    MachineState { instance, status, session, conn_gen }
}

/// An action armed against one connection must never fire on another, and
/// each missing precondition gets its own reason (fail closed).
#[test]
fn a_reply_is_refused_unless_the_same_connection_is_online_and_capable() {
    let a = reply_action(1, 7);
    // A different instance, a newer connection, or a machine that is not
    // online all read as offline.
    for s in [
        state(2, 7, MachineStatus::Online, &["send"]),
        state(1, 8, MachineStatus::Online, &["send"]),
        state(1, 7, MachineStatus::Syncing, &["send"]),
        state(1, 7, MachineStatus::Connecting, &["send"]),
    ] {
        assert_eq!(check_reason(&a, &s), Err(NotSentReason::Offline));
    }
    // Online without the `send` capability: not supported, even though the
    // `answer` capability is present.
    assert_eq!(
        check_reason(&a, &state(1, 7, MachineStatus::Online, &["answer"])),
        Err(NotSentReason::NotSupported)
    );
    // Capable but the agent is not in the session.
    assert_eq!(
        check_reason(&a, &state(1, 7, MachineStatus::Online, &["send"])),
        Err(NotSentReason::AgentGone)
    );
    // `check` carries the same reason as the card notice.
    let n = check(&a, &state(1, 7, MachineStatus::Online, &["send"])).unwrap_err();
    assert_eq!(n.text, "The agent is gone");
}

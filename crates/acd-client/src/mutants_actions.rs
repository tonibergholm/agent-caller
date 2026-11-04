//! Tests added from a cargo-mutants run (docs/research/2026-11-04-mutation-testing.md).
//! A child module of `actions`, for its private test helpers.
use super::*;
use crate::session::Session;
use acd_protocol::*;

fn req() -> Request {
    serde_json::from_value(serde_json::json!({"id":"r","agent_id":"a","kind":"permission","status":"open","answer_path":"hook","title":"SECRET-TITLE","detail":"SECRET-DETAIL","options":[{"id":"allow","label":"Yes","description":null,"effect":"allow"},{"id":"always","label":"Always","description":"Bash(npm test:*)","effect":"allow_always"}],"questions":null,"free_text":true,"raw_screen":null,"opened_at":"2026-10-08T00:00:00Z"})).unwrap()
}

fn state_with(req: Request, can_send: bool) -> MachineState {
    let a: Agent = serde_json::from_value(serde_json::json!({"id":"a","machine_id":"m","source":"herdr","kind":"claude","title":"agent","cwd":null,"project":null,"state":"needs_you","state_since":"2026-10-08T00:00:00Z","failed":false,"last_message":null,"can_send":can_send,"pending":serde_json::to_value(req).unwrap()})).unwrap();
    let mut s = Session::default();
    let machine: Machine = serde_json::from_value(serde_json::json!({"id":"m","name":"M","acd_version":"0","sources":{"hooks":{"state":"ok"},"herdr":{"state":"ok"},"t3":{"state":"ok"}}})).unwrap();
    s.welcome(machine, ["answer", "send"].into_iter().map(str::to_owned).collect(), "e", 1);
    s.apply(&ServerMessage::Snapshot { epoch: "e".into(), seq: 1, agents: vec![a] });
    MachineState { instance: 1, status: MachineStatus::Online, session: s, conn_gen: 1 }
}

fn action(kind: ActionKind) -> Action {
    let now = Instant::now();
    Action {
        id: "x".into(),
        owner: ActionOwner::DEFAULT,
        card_key: "m/a".into(),
        machine_key: "m".into(),
        agent_id: "a".into(),
        instance: 1,
        conn_gen: 1,
        kind,
        fires_at: now,
        view: ActionView { id: "x".into(), card_key: "m/a".into(), label: "l".into(), detail: None, fires_at_ms: 0. },
    }
}

/// `check_reason` `||` -> `&&` (line 282): each of the three conditions alone
/// is enough to refuse. The action is built from the same request the state
/// holds, so the other conditions cannot trigger.
#[test]
fn an_unanswerable_request_is_refused_even_when_it_is_unchanged() {
    let mut r = req();
    r.answer_path = AnswerPath::None;
    let a = action(ActionKind::Answer(Semantics::of(&r, "allow").unwrap()));
    assert_eq!(check_reason(&a, &state_with(r, true)), Err(NotSentReason::Changed));
}

#[test]
fn an_unknown_effect_is_refused_even_when_it_is_unchanged() {
    let mut r = req();
    r.options[0].effect = Effect::Unknown;
    let a = action(ActionKind::Answer(Semantics::of(&r, "allow").unwrap()));
    assert_eq!(check_reason(&a, &state_with(r, true)), Err(NotSentReason::Changed));
}

/// The typed-answer rules: free text must be on, the request answerable and unchanged.
#[test]
fn a_typed_answer_needs_free_text_an_answerable_request_and_the_same_text() {
    let r = req();
    let a = action(ActionKind::AnswerText(TextSemantics::of(&r, "go")));
    assert_eq!(check_reason(&a, &state_with(r.clone(), true)), Ok(()));
    let mut no_free = r.clone();
    no_free.free_text = false;
    let a = action(ActionKind::AnswerText(TextSemantics::of(&no_free, "go")));
    assert_eq!(check_reason(&a, &state_with(no_free, true)), Err(NotSentReason::Changed));
    let mut none = r.clone();
    none.answer_path = AnswerPath::None;
    let a = action(ActionKind::AnswerText(TextSemantics::of(&none, "go")));
    assert_eq!(check_reason(&a, &state_with(none, true)), Err(NotSentReason::Changed));
    let a = action(ActionKind::AnswerText(TextSemantics::of(&r, "go")));
    let mut other = r;
    other.detail = "different".into();
    assert_eq!(check_reason(&a, &state_with(other, true)), Err(NotSentReason::Changed));
}

/// `check_reason` match guard `!agent.can_send` (line 289): a reply to an agent that cannot take one is refused.
#[test]
fn a_reply_needs_an_agent_that_can_take_it() {
    let a = action(ActionKind::Reply { text: "hi".into() });
    assert_eq!(check_reason(&a, &state_with(req(), true)), Ok(()));
    assert_eq!(check_reason(&a, &state_with(req(), false)), Err(NotSentReason::AgentBusy));
}

/// `Debug` for `Semantics`, `TextSemantics`, `ActionKind`: lengths, never the
/// agent's or the user's text (voice spec section 3.7).
#[test]
fn debug_output_never_shows_request_or_typed_text() {
    let r = req();
    let sem = Semantics::of(&r, "allow").unwrap();
    let text = TextSemantics::of(&r, "SECRET-TYPED");
    for shown in [
        format!("{sem:?}"),
        format!("{text:?}"),
        format!("{:?}", ActionKind::Answer(sem.clone())),
        format!("{:?}", ActionKind::AnswerText(text.clone())),
        format!("{:?}", ActionKind::Reply { text: "SECRET-TYPED".into() }),
    ] {
        assert!(!shown.contains("SECRET-"), "{shown}");
        assert!(!shown.is_empty());
    }
    assert!(format!("{sem:?}").contains("Semantics") && format!("{sem:?}").contains("title_bytes"));
    assert!(format!("{text:?}").contains("TextSemantics") && format!("{text:?}").contains("text_bytes"));
    assert!(format!("{:?}", ActionKind::Answer(sem)).starts_with("Answer("));
    assert!(format!("{:?}", ActionKind::AnswerText(text)).starts_with("AnswerText("));
    assert!(format!("{:?}", ActionKind::Reply { text: "x".into() }).contains("text_bytes"));
}

/// `check_reason` `||` -> `&&` on the typed-answer branch (line 282): another
/// request, one already claimed, or none at all means it was answered elsewhere.
#[test]
fn a_typed_answer_to_another_or_a_claimed_request_was_answered_elsewhere() {
    let r = req();
    let a = action(ActionKind::AnswerText(TextSemantics::of(&r, "go")));
    let mut other = r.clone();
    other.id = "r-other".into();
    assert_eq!(check_reason(&a, &state_with(other, true)), Err(NotSentReason::AnsweredElsewhere));
    let mut claimed = r.clone();
    claimed.status = RequestStatus::Answering;
    assert_eq!(check_reason(&a, &state_with(claimed, true)), Err(NotSentReason::AnsweredElsewhere));
    let mut gone = state_with(r, true);
    gone.session.agents.get_mut("a").unwrap().pending = None;
    assert_eq!(check_reason(&a, &gone), Err(NotSentReason::AnsweredElsewhere));
}

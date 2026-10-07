use acd_core::*;
use acd_protocol::*;

fn agent(id: &str, can_send: bool) -> AgentObs {
    AgentObs {
        id: id.into(), source: Source::Herdr, kind: AgentKind::Claude, title: "t".into(), cwd: None,
        project: None, state: AgentState::Done, failed: false, last_message: None, can_send,
    }
}

fn perm(key: &str) -> RequestObs {
    RequestObs {
        agent_id: "a1".into(), key: CorrelationKey(key.into()), upgrade_from: None,
        kind: RequestKind::Permission, answer_path: AnswerPath::Hook, title: "Bash".into(), detail: "ls".into(),
        options: vec![RequestOption { id: "allow".into(), label: "Yes".into(), description: None, effect: Effect::Allow }],
        questions: None, free_text: false, raw_screen: None, ready: true,
    }
}

fn setup(obs: RequestObs) -> (Registry, String) {
    let mut r = Registry::new("m".into());
    r.apply(Observation::AgentUpsert(agent("a1", true)));
    let ch = r.apply(Observation::RequestSeen(obs));
    let id = ch.iter().find_map(|c| match c { Change::RequestOpened(q) => Some(q.id.clone()), _ => None }).unwrap();
    (r, id)
}

#[test]
fn claim_moves_to_answering_and_second_claim_is_busy() {
    let (mut r, id) = setup(perm("k"));
    let (req, ch) = r.claim(&id, Answer::Option("allow".into())).unwrap();
    assert_eq!(req.status, RequestStatus::Answering);
    assert!(matches!(&ch[0], Change::AgentUpdated(a) if a.pending.as_ref().unwrap().status == RequestStatus::Answering));
    assert_eq!(r.claim(&id, Answer::Option("allow".into())).unwrap_err(), ErrorCode::RequestBusy);
}

#[test]
fn claim_error_cases() {
    let (mut r, id) = setup(perm("k"));
    assert_eq!(r.claim("nope", Answer::Option("allow".into())).unwrap_err(), ErrorCode::RequestGone);
    assert_eq!(r.claim(&id, Answer::Option("maybe".into())).unwrap_err(), ErrorCode::BadRequest);
    assert_eq!(r.claim(&id, Answer::Text("hi".into())).unwrap_err(), ErrorCode::BadRequest);

    let (mut r2, id2) = setup(RequestObs { answer_path: AnswerPath::None, ..perm("k") });
    assert_eq!(r2.claim(&id2, Answer::Option("allow".into())).unwrap_err(), ErrorCode::NotAnswerable);

    let (mut r3, id3) = setup(RequestObs { ready: false, ..perm("k") });
    assert_eq!(r3.claim(&id3, Answer::Option("allow".into())).unwrap_err(), ErrorCode::RequestNotReady);
}

#[test]
fn free_text_answers_reject_control_characters() {
    let q = Question { id: "q".into(), header: None, text: "Name?".into(), multi_select: false, options: vec![] };
    let obs = RequestObs { kind: RequestKind::Question, questions: Some(vec![q]), free_text: true, options: vec![], answer_path: AnswerPath::T3, ..perm("k") };
    let (mut r, id) = setup(obs);
    assert_eq!(r.claim(&id, Answer::Text("a\u{1b}[2J".into())).unwrap_err(), ErrorCode::BadRequest);
    assert_eq!(r.claim(&id, Answer::Text("   ".into())).unwrap_err(), ErrorCode::BadRequest);
    assert!(r.claim(&id, Answer::Text("Ada\nLovelace".into())).is_ok());
}

#[test]
fn release_returns_to_open() {
    let (mut r, id) = setup(perm("k"));
    r.claim(&id, Answer::Option("allow".into())).unwrap();
    assert_eq!(r.release(&id).len(), 1);
    assert_eq!(r.request(&id).unwrap().status, RequestStatus::Open);
    assert!(r.release(&id).is_empty(), "releasing an open request is a no-op");
}

#[test]
fn terminal_end_after_claim_is_attributed_to_inbox() {
    let (mut r, id) = setup(perm("k"));
    r.claim(&id, Answer::Option("allow".into())).unwrap();
    let ch = r.apply(Observation::RequestEnded { key: CorrelationKey("k".into()), by: ResolvedBy::Terminal });
    assert!(ch.contains(&Change::RequestResolved {
        request_id: id.clone(), agent_id: "a1".into(), by: ResolvedBy::Inbox, option_id: Some("allow".into()),
    }));
}

#[test]
fn check_send_rules() {
    let mut r = Registry::new("m".into());
    r.apply(Observation::AgentUpsert(agent("a1", true)));
    r.apply(Observation::AgentUpsert(agent("hooks-only", false)));
    assert!(r.check_send("a1", "run the tests").is_ok());
    assert_eq!(r.check_send("ghost", "x").unwrap_err(), ErrorCode::AgentGone);
    assert_eq!(r.check_send("hooks-only", "x").unwrap_err(), ErrorCode::NotSupported);
    assert_eq!(r.check_send("a1", "").unwrap_err(), ErrorCode::BadRequest);
    assert_eq!(r.check_send("a1", "rm -rf /\r").unwrap_err(), ErrorCode::BadRequest);
    r.apply(Observation::RequestSeen(perm("k")));
    assert_eq!(r.check_send("a1", "x").unwrap_err(), ErrorCode::AgentBusy);
}

#[test]
fn mark_seen_moves_done_to_idle_only() {
    let mut r = Registry::new("m".into());
    r.apply(Observation::AgentUpsert(agent("a1", true)));
    let ch = r.mark_seen("a1").unwrap();
    assert!(matches!(&ch[0], Change::AgentUpdated(a) if a.state == AgentState::Idle));
    assert!(r.mark_seen("a1").unwrap().is_empty());
    assert_eq!(r.mark_seen("ghost").unwrap_err(), ErrorCode::AgentGone);
}

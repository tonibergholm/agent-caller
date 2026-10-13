use acd_core::*;
use acd_protocol::*;

fn agent(id: &str, can_send: bool) -> AgentObs {
    AgentObs {
        id: id.into(),
        source: Source::Herdr,
        kind: AgentKind::Claude,
        title: "t".into(),
        cwd: None,
        project: None,
        state: AgentState::Done,
        failed: false,
        last_message: None,
        can_send,
        role: AgentRole::Main,
        parent_title: None,
    }
}

fn perm(key: &str) -> RequestObs {
    RequestObs {
        agent_id: "a1".into(),
        key: CorrelationKey(key.into()),
        upgrade_from: None,
        kind: RequestKind::Permission,
        answer_path: AnswerPath::Hook,
        title: "Bash".into(),
        detail: "ls".into(),
        options: vec![RequestOption {
            id: "allow".into(),
            label: "Yes".into(),
            description: None,
            effect: Effect::Allow,
            confirm: false,
        }],
        questions: None,
        free_text: false,
        raw_screen: None,
        ready: true,
        reusable_key: true,
    }
}

fn setup(obs: RequestObs) -> (Registry, String) {
    let mut r = Registry::new("m".into());
    r.apply(Observation::AgentUpsert(agent("a1", true)));
    let ch = r.apply(Observation::RequestSeen(obs));
    let id = ch
        .iter()
        .find_map(|c| match c {
            Change::RequestOpened(q) => Some(q.id.clone()),
            _ => None,
        })
        .unwrap();
    (r, id)
}

#[test]
fn claim_moves_to_answering_and_second_claim_is_busy() {
    let (mut r, id) = setup(perm("k"));
    let (req, ch) = r.claim(&id, Answer::Option("allow".into()), None).unwrap();
    assert_eq!(req.status, RequestStatus::Answering);
    assert!(
        matches!(&ch[0], Change::AgentUpdated(a) if a.pending.as_ref().unwrap().status == RequestStatus::Answering)
    );
    assert_eq!(
        r.claim(&id, Answer::Option("allow".into()), None)
            .unwrap_err(),
        ErrorCode::RequestBusy
    );
}

#[test]
fn claim_checks_revision_after_lifecycle_before_answer_validation() {
    let (mut r, id) = setup(perm("k"));
    assert_eq!(r.request(&id).unwrap().revision, 1);
    assert_eq!(
        r.claim(&id, Answer::Option("removed-option".into()), Some(0))
            .unwrap_err(),
        ErrorCode::RequestChanged
    );
    assert_eq!(r.request(&id).unwrap().status, RequestStatus::Open);
    assert_eq!(r.request(&id).unwrap().revision, 1);
    assert_eq!(
        r.claim(&id, Answer::Option("allow".into()), Some(1))
            .unwrap()
            .0
            .status,
        RequestStatus::Answering
    );

    let (mut legacy, legacy_id) = setup(perm("legacy"));
    assert_eq!(
        legacy
            .claim(&legacy_id, Answer::Option("allow".into()), None)
            .unwrap()
            .0
            .status,
        RequestStatus::Answering
    );
}

#[test]
fn claim_lifecycle_errors_precede_revision_mismatch() {
    let (mut r, id) = setup(perm("k"));
    r.claim(&id, Answer::Option("allow".into()), None).unwrap();
    assert_eq!(
        r.claim(&id, Answer::Option("allow".into()), Some(999))
            .unwrap_err(),
        ErrorCode::RequestBusy
    );

    let (mut not_ready, not_ready_id) = setup(RequestObs {
        ready: false,
        ..perm("not-ready")
    });
    assert_eq!(
        not_ready
            .claim(&not_ready_id, Answer::Option("removed".into()), Some(999))
            .unwrap_err(),
        ErrorCode::RequestNotReady
    );

    let (mut blocked, blocked_id) = setup(RequestObs {
        answer_path: AnswerPath::None,
        ..perm("blocked")
    });
    assert_eq!(
        blocked
            .claim(&blocked_id, Answer::Option("removed".into()), Some(999))
            .unwrap_err(),
        ErrorCode::NotAnswerable
    );
}

#[test]
fn claim_error_cases() {
    let (mut r, id) = setup(perm("k"));
    assert_eq!(
        r.claim("nope", Answer::Option("allow".into()), None)
            .unwrap_err(),
        ErrorCode::RequestGone
    );
    assert_eq!(
        r.claim(&id, Answer::Option("maybe".into()), None)
            .unwrap_err(),
        ErrorCode::BadRequest
    );
    assert_eq!(
        r.claim(&id, Answer::Text("hi".into()), None).unwrap_err(),
        ErrorCode::BadRequest
    );

    let (mut r2, id2) = setup(RequestObs {
        answer_path: AnswerPath::None,
        ..perm("k")
    });
    assert_eq!(
        r2.claim(&id2, Answer::Option("allow".into()), None)
            .unwrap_err(),
        ErrorCode::NotAnswerable
    );

    let (mut r3, id3) = setup(RequestObs {
        ready: false,
        ..perm("k")
    });
    assert_eq!(
        r3.claim(&id3, Answer::Option("allow".into()), None)
            .unwrap_err(),
        ErrorCode::RequestNotReady
    );
}

#[test]
fn free_text_answers_reject_control_characters() {
    let q = Question {
        id: "q".into(),
        header: None,
        text: "Name?".into(),
        multi_select: false,
        options: vec![],
    };
    let obs = RequestObs {
        kind: RequestKind::Question,
        questions: Some(vec![q]),
        free_text: true,
        options: vec![],
        answer_path: AnswerPath::T3,
        ..perm("k")
    };
    let (mut r, id) = setup(obs);
    assert_eq!(
        r.claim(&id, Answer::Text("a\u{1b}[2J".into()), None)
            .unwrap_err(),
        ErrorCode::BadRequest
    );
    assert_eq!(
        r.claim(&id, Answer::Text("   ".into()), None).unwrap_err(),
        ErrorCode::BadRequest
    );
    assert!(
        r.claim(&id, Answer::Text("Ada\nLovelace".into()), None)
            .is_ok()
    );
}

#[test]
fn release_returns_to_open() {
    let (mut r, id) = setup(perm("k"));
    r.claim(&id, Answer::Option("allow".into()), None).unwrap();
    assert_eq!(r.release(&id).len(), 1);
    assert_eq!(r.request(&id).unwrap().status, RequestStatus::Open);
    assert!(
        r.release(&id).is_empty(),
        "releasing an open request is a no-op"
    );
}

#[test]
fn inbox_end_after_claim_carries_the_claimed_option() {
    let (mut r, id) = setup(perm("k"));
    r.claim(&id, Answer::Option("allow".into()), None).unwrap();
    let ch = r.apply(Observation::RequestEnded {
        agent_id: "a1".into(),
        key: CorrelationKey("k".into()),
        by: ResolvedBy::Inbox,
    });
    assert!(ch.contains(&Change::RequestResolved {
        request_id: id.clone(),
        agent_id: "a1".into(),
        by: ResolvedBy::Inbox,
        option_id: Some("allow".into()),
        reason: None,
        message: None,
    }));
}

#[test]
fn terminal_end_after_claim_stays_terminal() {
    let (mut r, id) = setup(perm("k"));
    r.claim(&id, Answer::Option("allow".into()), None).unwrap();
    let ch = r.apply(Observation::RequestEnded {
        agent_id: "a1".into(),
        key: CorrelationKey("k".into()),
        by: ResolvedBy::Terminal,
    });
    assert!(ch.contains(&Change::RequestResolved {
        request_id: id.clone(),
        agent_id: "a1".into(),
        by: ResolvedBy::Terminal,
        option_id: None,
        reason: None,
        message: None,
    }));
}

#[test]
fn check_send_rules() {
    let mut r = Registry::new("m".into());
    r.apply(Observation::AgentUpsert(agent("a1", true)));
    r.apply(Observation::AgentUpsert(agent("hooks-only", false)));
    assert!(r.check_send("a1", "run the tests").is_ok());
    assert_eq!(
        r.check_send("ghost", "x").unwrap_err(),
        ErrorCode::AgentGone
    );
    assert_eq!(
        r.check_send("hooks-only", "x").unwrap_err(),
        ErrorCode::NotSupported
    );
    assert_eq!(r.check_send("a1", "").unwrap_err(), ErrorCode::BadRequest);
    assert_eq!(
        r.check_send("a1", "rm -rf /\r").unwrap_err(),
        ErrorCode::BadRequest
    );
    r.apply(Observation::RequestSeen(perm("k")));
    assert_eq!(r.check_send("a1", "x").unwrap_err(), ErrorCode::AgentBusy);
}

#[test]
fn check_send_refuses_needs_you_agent_without_pending_request() {
    let mut r = Registry::new("m".into());
    r.apply(Observation::AgentUpsert(AgentObs {
        state: AgentState::NeedsYou,
        ..agent("a1", true)
    }));
    assert!(r.agent("a1").unwrap().pending.is_none());
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

#[test]
fn seen_marker_persists_until_a_new_completion() {
    let mut r = Registry::new("m".into());
    r.apply(Observation::AgentUpsert(agent("a1", true)));
    r.mark_seen("a1").unwrap();
    assert!(
        r.apply(Observation::AgentUpsert(agent("a1", true)))
            .is_empty()
    );
    assert_eq!(r.agent("a1").unwrap().state, AgentState::Idle);
    r.apply(Observation::AgentUpsert(AgentObs {
        state: AgentState::Working,
        ..agent("a1", true)
    }));
    r.apply(Observation::AgentUpsert(agent("a1", true)));
    assert_eq!(r.agent("a1").unwrap().state, AgentState::Done);
}

#[test]
fn new_request_clears_the_seen_marker() {
    let mut r = Registry::new("m".into());
    r.apply(Observation::AgentUpsert(agent("a1", true)));
    r.mark_seen("a1").unwrap();
    r.apply(Observation::RequestSeen(perm("k")));
    r.apply(Observation::RequestEnded {
        agent_id: "a1".into(),
        key: CorrelationKey("k".into()),
        by: ResolvedBy::Terminal,
    });
    r.apply(Observation::AgentUpsert(agent("a1", true)));
    assert_eq!(
        r.agent("a1").unwrap().state,
        AgentState::Done,
        "a new completion after a request is unseen"
    );
}

#[test]
fn request_content_is_frozen_while_answering() {
    let (mut r, id) = setup(perm("k"));
    r.claim(&id, Answer::Option("allow".into()), None).unwrap();
    let evil = RequestObs {
        detail: "rm -rf /".into(),
        options: vec![RequestOption {
            id: "other".into(),
            label: "x".into(),
            description: None,
            effect: Effect::Allow,
            confirm: false,
        }],
        ..perm("k")
    };
    assert!(r.apply(Observation::RequestSeen(evil.clone())).is_empty());
    let q = r.request(&id).unwrap();
    assert_eq!(q.detail, "ls");
    assert_eq!(q.status, RequestStatus::Answering);
    assert_eq!(q.options[0].id, "allow");
    r.release(&id);
    assert!(!r.apply(Observation::RequestSeen(evil)).is_empty());
    assert_eq!(r.request(&id).unwrap().detail, "rm -rf /");
}

#[test]
fn claim_of_resolved_request_is_gone() {
    let (mut r, id) = setup(perm("k"));
    r.apply(Observation::RequestEnded {
        agent_id: "a1".into(),
        key: CorrelationKey("k".into()),
        by: ResolvedBy::Terminal,
    });
    assert_eq!(
        r.claim(&id, Answer::Option("allow".into()), None)
            .unwrap_err(),
        ErrorCode::RequestGone
    );
}

#[test]
fn release_clears_the_claim() {
    let (mut r, id) = setup(perm("k"));
    r.claim(&id, Answer::Option("allow".into()), None).unwrap();
    r.release(&id);
    let ch = r.apply(Observation::RequestEnded {
        agent_id: "a1".into(),
        key: CorrelationKey("k".into()),
        by: ResolvedBy::Terminal,
    });
    assert!(ch.contains(&Change::RequestResolved {
        request_id: id,
        agent_id: "a1".into(),
        by: ResolvedBy::Terminal,
        option_id: None,
        reason: None,
        message: None,
    }));
}

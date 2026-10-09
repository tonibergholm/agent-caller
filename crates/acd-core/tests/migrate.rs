use acd_core::testing::{agent_obs, permission_obs};
use acd_core::*;
use acd_protocol::*;

fn hooks_agent(id: &str) -> AgentObs {
    AgentObs {
        source: Source::Hooks,
        can_send: false,
        ..agent_obs(id)
    }
}

fn reg() -> Registry {
    let mut r = Registry::new("m".into());
    r.apply(Observation::AgentUpsert(hooks_agent("claude:s1")));
    r.apply(Observation::AgentUpsert(agent_obs("herdr:h:w1:p1")));
    r
}

#[test]
fn the_open_request_moves_and_keeps_its_id() {
    let mut r = reg();
    r.apply(Observation::RequestSeen(permission_obs("claude:s1", "k")));
    let rid = r
        .agent("claude:s1")
        .unwrap()
        .pending
        .as_ref()
        .unwrap()
        .id
        .clone();
    let changes = r.apply(Observation::AgentMigrated {
        from: "claude:s1".into(),
        to: "herdr:h:w1:p1".into(),
    });
    assert!(
        matches!(&changes[0], Change::AgentUpdated(a) if a.id == "herdr:h:w1:p1"
        && a.pending.as_ref().map(|p| p.id.as_str()) == Some(rid.as_str())
        && a.state == AgentState::NeedsYou)
    );
    assert_eq!(
        changes.last(),
        Some(&Change::AgentRemoved("claude:s1".into()))
    );
    assert!(
        !changes
            .iter()
            .any(|c| matches!(c, Change::RequestResolved { .. } | Change::RequestOpened(_)))
    );
    assert!(r.agent("claude:s1").is_none());
    assert_eq!(r.request(&rid).unwrap().agent_id, "herdr:h:w1:p1");
    // The key moved too: a replay on the new agent updates in place.
    r.apply(Observation::RequestSeen(permission_obs(
        "herdr:h:w1:p1",
        "k",
    )));
    assert_eq!(
        r.agent("herdr:h:w1:p1")
            .unwrap()
            .pending
            .as_ref()
            .unwrap()
            .id,
        rid
    );
    let ended = r.apply(Observation::RequestEnded {
        agent_id: "herdr:h:w1:p1".into(),
        key: CorrelationKey("k".into()),
        by: ResolvedBy::Terminal,
    });
    assert!(
        ended
            .iter()
            .any(|c| matches!(c, Change::RequestResolved { request_id, .. } if *request_id == rid))
    );
}

#[test]
fn a_claimed_request_moves_with_its_claim() {
    let mut r = reg();
    r.apply(Observation::RequestSeen(permission_obs("claude:s1", "k")));
    let rid = r
        .agent("claude:s1")
        .unwrap()
        .pending
        .as_ref()
        .unwrap()
        .id
        .clone();
    r.claim(&rid, Answer::Option("allow".into()), None).unwrap();
    r.apply(Observation::AgentMigrated {
        from: "claude:s1".into(),
        to: "herdr:h:w1:p1".into(),
    });
    assert_eq!(r.request(&rid).unwrap().status, RequestStatus::Answering);
    let done = r.apply(Observation::RequestEndedById {
        request_id: rid.clone(),
        by: ResolvedBy::Inbox,
    });
    assert!(
        done.iter().any(
            |c| matches!(c, Change::RequestResolved { option_id: Some(o), .. } if o == "allow")
        )
    );
}

#[test]
fn a_fallback_on_the_target_is_superseded() {
    let mut r = reg();
    let mut fb = permission_obs("herdr:h:w1:p1", "herdr:h:w1:p1:fp");
    fb.kind = RequestKind::BlockedUnparsed;
    fb.answer_path = AnswerPath::None;
    r.apply(Observation::FallbackSeen(fb));
    let fb_id = r
        .agent("herdr:h:w1:p1")
        .unwrap()
        .pending
        .as_ref()
        .unwrap()
        .id
        .clone();
    r.apply(Observation::RequestSeen(permission_obs("claude:s1", "k")));
    let changes = r.apply(Observation::AgentMigrated {
        from: "claude:s1".into(),
        to: "herdr:h:w1:p1".into(),
    });
    assert!(changes.iter().any(|c| matches!(c, Change::RequestResolved { request_id, by: ResolvedBy::Superseded, .. } if *request_id == fb_id)));
    assert_ne!(
        r.agent("herdr:h:w1:p1")
            .unwrap()
            .pending
            .as_ref()
            .unwrap()
            .id,
        fb_id
    );
}

#[test]
fn a_structured_request_on_the_target_wins() {
    let mut r = reg();
    r.apply(Observation::RequestSeen(permission_obs(
        "herdr:h:w1:p1",
        "other",
    )));
    let kept = r
        .agent("herdr:h:w1:p1")
        .unwrap()
        .pending
        .as_ref()
        .unwrap()
        .id
        .clone();
    r.apply(Observation::RequestSeen(permission_obs("claude:s1", "k")));
    let moving = r
        .agent("claude:s1")
        .unwrap()
        .pending
        .as_ref()
        .unwrap()
        .id
        .clone();
    let changes = r.apply(Observation::AgentMigrated {
        from: "claude:s1".into(),
        to: "herdr:h:w1:p1".into(),
    });
    assert!(changes.iter().any(|c| matches!(c, Change::RequestResolved { request_id, by: ResolvedBy::Lost, .. } if *request_id == moving)));
    assert_eq!(
        r.agent("herdr:h:w1:p1")
            .unwrap()
            .pending
            .as_ref()
            .unwrap()
            .id,
        kept
    );
    assert!(r.agent("claude:s1").is_none());
}

#[test]
fn migration_without_a_request_only_removes_the_source() {
    let mut r = reg();
    let changes = r.apply(Observation::AgentMigrated {
        from: "claude:s1".into(),
        to: "herdr:h:w1:p1".into(),
    });
    assert_eq!(changes, vec![Change::AgentRemoved("claude:s1".into())]);
}

#[test]
fn migration_to_an_unknown_agent_is_a_no_op() {
    let mut r = reg();
    r.apply(Observation::RequestSeen(permission_obs("claude:s1", "k")));
    assert!(
        r.apply(Observation::AgentMigrated {
            from: "claude:s1".into(),
            to: "herdr:h:nope".into()
        })
        .is_empty()
    );
    assert!(r.agent("claude:s1").unwrap().pending.is_some());
}

#[test]
fn pending_key_names_the_primary_key() {
    let mut r = reg();
    assert!(r.pending_key("herdr:h:w1:p1").is_none());
    let mut fb = permission_obs("herdr:h:w1:p1", "herdr:h:w1:p1:fp");
    fb.kind = RequestKind::BlockedUnparsed;
    fb.answer_path = AnswerPath::None;
    r.apply(Observation::FallbackSeen(fb));
    let (rid, key, kind) = r.pending_key("herdr:h:w1:p1").unwrap();
    assert_eq!(key, CorrelationKey("herdr:h:w1:p1:fp".into()));
    assert_eq!(kind, RequestKind::BlockedUnparsed);
    assert_eq!(
        r.agent("herdr:h:w1:p1")
            .unwrap()
            .pending
            .as_ref()
            .unwrap()
            .id,
        rid
    );
}

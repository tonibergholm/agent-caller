use acd_core::*;
use acd_protocol::*;

fn agent(id: &str) -> AgentObs {
    AgentObs {
        id: id.into(), source: Source::Herdr, kind: AgentKind::Claude, title: "sp1".into(),
        cwd: Some("/src/app".into()), project: Some("app".into()), state: AgentState::Working,
        failed: false, last_message: None, can_send: true,
    }
}

fn perm(agent_id: &str, key: &str, detail: &str) -> RequestObs {
    RequestObs {
        agent_id: agent_id.into(), key: CorrelationKey(key.into()), upgrade_from: None,
        kind: RequestKind::Permission, answer_path: AnswerPath::Hook, title: "Bash".into(),
        detail: detail.into(),
        options: vec![
            RequestOption { id: "allow".into(), label: "Yes".into(), description: None, effect: Effect::Allow },
            RequestOption { id: "deny".into(), label: "No".into(), description: None, effect: Effect::Deny },
        ],
        questions: None, free_text: false, raw_screen: None, ready: true,
    }
}

fn opened(changes: &[Change]) -> Vec<Request> {
    changes.iter().filter_map(|c| match c { Change::RequestOpened(r) => Some(r.clone()), _ => None }).collect()
}

fn resolved(changes: &[Change]) -> Vec<(String, ResolvedBy, Option<String>)> {
    changes.iter().filter_map(|c| match c {
        Change::RequestResolved { request_id, by, option_id, .. } => Some((request_id.clone(), *by, option_id.clone())),
        _ => None,
    }).collect()
}

fn reg_with_agent() -> Registry {
    let mut r = Registry::new("m1".into());
    r.apply(Observation::AgentUpsert(agent("a1")));
    r
}

#[test]
fn identical_agent_upsert_emits_nothing() {
    let mut r = Registry::new("m1".into());
    assert_eq!(r.apply(Observation::AgentUpsert(agent("a1"))).len(), 1);
    assert!(r.apply(Observation::AgentUpsert(agent("a1"))).is_empty());
    assert_eq!(r.agent("a1").unwrap().machine_id, "m1");
}

#[test]
fn observations_for_unknown_agents_are_ignored() {
    let mut r = Registry::new("m1".into());
    assert!(r.apply(Observation::RequestSeen(perm("ghost", "k", "ls"))).is_empty());
    assert!(r.apply(Observation::AgentGone { agent_id: "ghost".into() }).is_empty());
    assert!(r.apply(Observation::RequestEnded { key: CorrelationKey("k".into()), by: ResolvedBy::Terminal }).is_empty());
    assert!(r.agents().is_empty());
}

#[test]
fn opening_a_request_sets_pending_and_needs_you() {
    let mut r = reg_with_agent();
    let ch = r.apply(Observation::RequestSeen(perm("a1", "k1", "ls")));
    let req = &opened(&ch)[0];
    let a = r.agent("a1").unwrap();
    assert_eq!(a.state, AgentState::NeedsYou);
    assert_eq!(a.pending.as_ref().unwrap().id, req.id);
    assert_eq!(req.status, RequestStatus::Open);
}

#[test]
fn same_key_updates_in_place() {
    let mut r = reg_with_agent();
    let id = opened(&r.apply(Observation::RequestSeen(perm("a1", "k1", "ls"))))[0].id.clone();
    let ch = r.apply(Observation::RequestSeen(perm("a1", "k1", "ls -la")));
    assert!(opened(&ch).is_empty());
    assert_eq!(r.agent("a1").unwrap().pending.as_ref().unwrap().id, id);
    assert_eq!(r.request(&id).unwrap().detail, "ls -la");
}

#[test]
fn different_key_supersedes() {
    let mut r = reg_with_agent();
    let first = opened(&r.apply(Observation::RequestSeen(perm("a1", "k1", "ls"))))[0].id.clone();
    let ch = r.apply(Observation::RequestSeen(perm("a1", "k2", "rm x")));
    assert_eq!(resolved(&ch), vec![(first, ResolvedBy::Superseded, None)]);
    assert_eq!(opened(&ch).len(), 1);
}

#[test]
fn replay_of_resolved_key_is_ignored_until_dialog_gone_then_new_occurrence() {
    let mut r = reg_with_agent();
    let first = opened(&r.apply(Observation::RequestSeen(perm("a1", "k1", "ls"))))[0].id.clone();
    r.apply(Observation::RequestEnded { key: CorrelationKey("k1".into()), by: ResolvedBy::Uncertain });
    assert!(r.apply(Observation::RequestSeen(perm("a1", "k1", "ls"))).is_empty(), "replay must not reopen");
    r.apply(Observation::DialogGone { key: CorrelationKey("k1".into()) });
    let second = opened(&r.apply(Observation::RequestSeen(perm("a1", "k1", "ls"))));
    assert_eq!(second.len(), 1);
    assert_ne!(second[0].id, first);
}

#[test]
fn terminal_end_confirms_gone_so_same_dialog_can_reopen() {
    let mut r = reg_with_agent();
    r.apply(Observation::RequestSeen(perm("a1", "k1", "ls")));
    let ch = r.apply(Observation::RequestEnded { key: CorrelationKey("k1".into()), by: ResolvedBy::Terminal });
    assert_eq!(resolved(&ch)[0].1, ResolvedBy::Terminal);
    assert_eq!(opened(&r.apply(Observation::RequestSeen(perm("a1", "k1", "ls")))).len(), 1);
}

#[test]
fn upgrade_from_fallback_keeps_request_id() {
    let mut r = reg_with_agent();
    let fallback = RequestObs {
        kind: RequestKind::BlockedUnparsed, answer_path: AnswerPath::None,
        raw_screen: Some("Do you want to proceed?".into()), options: vec![], ..perm("a1", "fp:1", "")
    };
    let id = opened(&r.apply(Observation::RequestSeen(fallback)))[0].id.clone();
    let structured = RequestObs { upgrade_from: Some(CorrelationKey("fp:1".into())), ..perm("a1", "prompt:9", "ls") };
    let ch = r.apply(Observation::RequestSeen(structured));
    assert!(opened(&ch).is_empty());
    assert!(resolved(&ch).is_empty());
    let req = r.request(&id).unwrap();
    assert_eq!(req.kind, RequestKind::Permission);
    assert_eq!(req.answer_path, AnswerPath::Hook);
}

#[test]
fn agent_gone_resolves_pending_as_lost_and_removes_agent() {
    let mut r = reg_with_agent();
    let id = opened(&r.apply(Observation::RequestSeen(perm("a1", "k1", "ls"))))[0].id.clone();
    let ch = r.apply(Observation::AgentGone { agent_id: "a1".into() });
    assert_eq!(resolved(&ch), vec![(id.clone(), ResolvedBy::Lost, None)]);
    assert!(ch.contains(&Change::AgentRemoved("a1".into())));
    assert!(r.request(&id).is_none());
    assert!(r.agent("a1").is_none());
}

#[test]
fn path_and_readiness_updates_apply_to_open_requests() {
    let mut r = reg_with_agent();
    let id = opened(&r.apply(Observation::RequestSeen(RequestObs { ready: false, ..perm("a1", "k1", "ls") })))[0].id.clone();
    r.apply(Observation::RequestPath { key: CorrelationKey("k1".into()), answer_path: AnswerPath::Keys });
    assert_eq!(r.request(&id).unwrap().answer_path, AnswerPath::Keys);
    r.apply(Observation::RequestReady { key: CorrelationKey("k1".into()), ready: true });
    assert!(r.is_ready(&id));
}

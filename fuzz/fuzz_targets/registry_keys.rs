//! The registry's request and key handling (acd-core/src/registry.rs) under
//! random observations for a few agents and keys: keys are kept after their
//! request resolves, so a late replay of a resolved dialog does not reopen it.
//! Invariants: an agent has at most one unresolved request and it is the
//! agent's `pending`; a request opens once and resolves once, and only while
//! its agent exists; a resolved request id is never found again; a request
//! with a key unique per occurrence (not reusable) is never reopened by a
//! replay of that key while the agent lives; a reusable key reopens only after
//! the dialog was confirmed gone; removing an agent leaves nothing of it.
#![no_main]
use acd_core::{AgentObs, Change, CorrelationKey, Observation, Registry, RequestObs};
use acd_protocol::{AgentKind, AgentRole, AgentState, AnswerPath, RequestKind, RequestStatus, ResolvedBy, Source};
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use std::collections::{HashMap, HashSet};

#[derive(Arbitrary, Debug)]
enum Op {
    Upsert { agent: u8 },
    Gone { agent: u8 },
    Seen { agent: u8, key: u8, reusable: bool },
    EndedBy { agent: u8, key: u8, by: u8 },
    DialogGone { agent: u8, key: u8 },
    EndById { pick: u8, by: u8 },
    EndIfOpen { pick: u8 },
    DialogGoneById { pick: u8 },
}

fn by(b: u8) -> ResolvedBy {
    [ResolvedBy::Inbox, ResolvedBy::Terminal, ResolvedBy::Agent, ResolvedBy::Superseded, ResolvedBy::Uncertain, ResolvedBy::Lost][b as usize % 6]
}

fn agent(id: &str) -> AgentObs {
    AgentObs {
        id: id.into(),
        source: Source::Herdr,
        kind: AgentKind::Claude,
        title: "agent".into(),
        cwd: None,
        project: None,
        state: AgentState::Working,
        failed: false,
        last_message: None,
        can_send: true,
        role: AgentRole::Main,
        parent_title: None,
    }
}

fn req(agent: &str, key: &str, reusable: bool) -> RequestObs {
    RequestObs {
        agent_id: agent.into(),
        key: CorrelationKey(key.into()),
        upgrade_from: None,
        kind: RequestKind::Permission,
        answer_path: AnswerPath::None,
        title: "t".into(),
        detail: "d".into(),
        options: vec![],
        questions: None,
        free_text: false,
        raw_screen: None,
        ready: false,
        reusable_key: reusable,
    }
}

fuzz_target!(|ops: Vec<Op>| {
    let mut r = Registry::new("m".into());
    let a = |n: u8| format!("a{}", n % 3);
    let k = |n: u8| format!("k{}", n % 4);
    let mut live: HashSet<String> = HashSet::new();
    let mut open: HashMap<String, String> = HashMap::new(); // agent -> request id
    let mut opened_by: HashMap<String, (String, String, bool)> = HashMap::new(); // rid -> (agent, key, reusable)
    let mut ever: HashSet<String> = HashSet::new();
    let mut resolved: HashSet<String> = HashSet::new();
    // (agent, key) that must not reopen: a key unique per occurrence once its
    // request resolved (`permanent`), a reusable key until its dialog is
    // confirmed gone (`unconfirmed`).
    let mut permanent: HashSet<(String, String)> = HashSet::new();
    let mut unconfirmed: HashSet<(String, String)> = HashSet::new();
    let mut all_ids: Vec<String> = vec![];
    for op in ops.iter().take(300) {
        let mut seen_req: Option<(String, String)> = None;
        let changes = match op {
            Op::Upsert { agent: x } => {
                live.insert(a(*x));
                r.apply(Observation::AgentUpsert(agent(&a(*x))))
            }
            Op::Gone { agent: x } => {
                permanent.retain(|(ag, _)| *ag != a(*x));
                unconfirmed.retain(|(ag, _)| *ag != a(*x));
                r.apply(Observation::AgentGone { agent_id: a(*x) })
            }
            Op::Seen { agent: x, key, reusable } => {
                seen_req = Some((a(*x), k(*key)));
                r.apply(Observation::RequestSeen(req(&a(*x), &k(*key), *reusable)))
            }
            Op::EndedBy { agent: x, key, by: b } => {
                r.apply(Observation::RequestEnded { agent_id: a(*x), key: CorrelationKey(k(*key)), by: by(*b) })
            }
            Op::DialogGone { agent: x, key } => {
                r.apply(Observation::DialogGone { agent_id: a(*x), key: CorrelationKey(k(*key)) })
            }
            Op::EndById { pick, by: b } => {
                let id = all_ids.get(*pick as usize % all_ids.len().max(1)).cloned().unwrap_or_default();
                r.apply(Observation::RequestEndedById { request_id: id, by: by(*b) })
            }
            Op::EndIfOpen { pick } => {
                let id = all_ids.get(*pick as usize % all_ids.len().max(1)).cloned().unwrap_or_default();
                r.apply(Observation::RequestEndedIfOpen { request_id: id, by: ResolvedBy::Superseded })
            }
            Op::DialogGoneById { pick } => {
                let id = all_ids.get(*pick as usize % all_ids.len().max(1)).cloned().unwrap_or_default();
                r.apply(Observation::DialogGoneById { request_id: id })
            }
        };
        let mut opened_now = false;
        for c in &changes {
            match c {
                Change::RequestResolved { request_id, agent_id, .. } => {
                    assert!(resolved.insert(request_id.clone()), "{request_id} resolved twice");
                    assert_eq!(open.remove(agent_id).as_deref(), Some(request_id.as_str()), "resolved a request that was not the agent's open one");
                    let (ag, key, reusable) = opened_by[request_id].clone();
                    if reusable {
                        unconfirmed.insert((ag, key));
                    } else {
                        permanent.insert((ag, key));
                    }
                }
                Change::RequestOpened(q) => {
                    opened_now = true;
                    assert!(ever.insert(q.id.clone()), "request id {} reused", q.id);
                    assert!(live.contains(&q.agent_id), "opened for an agent that does not exist");
                    assert!(open.insert(q.agent_id.clone(), q.id.clone()).is_none(), "two open requests on {}", q.agent_id);
                    all_ids.push(q.id.clone());
                    if let Some((ag, key)) = &seen_req {
                        let reusable = matches!(op, Op::Seen { reusable: true, .. });
                        let pair = (ag.clone(), key.clone());
                        assert!(!permanent.contains(&pair), "a resolved one-shot key {key} was reopened by a replay");
                        assert!(!unconfirmed.contains(&pair), "a resolved dialog {key} was reopened before it was confirmed gone");
                        opened_by.insert(q.id.clone(), (ag.clone(), key.clone(), reusable));
                    }
                }
                Change::AgentRemoved(id) => {
                    live.remove(id);
                    assert!(!open.contains_key(id), "agent removed with an open request");
                }
                _ => {}
            }
        }
        let _ = opened_now;
        // Confirmations unblock a reusable key (after the resolution they came with).
        match op {
            Op::EndedBy { agent: x, key, by: b } if matches!(by(*b), ResolvedBy::Inbox | ResolvedBy::Terminal | ResolvedBy::Agent) => {
                unconfirmed.remove(&(a(*x), k(*key)));
            }
            Op::DialogGone { agent: x, key } => {
                unconfirmed.remove(&(a(*x), k(*key)));
            }
            Op::EndById { pick, by: b } if matches!(by(*b), ResolvedBy::Inbox | ResolvedBy::Terminal | ResolvedBy::Agent) => {
                if let Some(id) = all_ids.get(*pick as usize % all_ids.len().max(1)) {
                    if let Some((ag, key, _)) = opened_by.get(id) {
                        unconfirmed.remove(&(ag.clone(), key.clone()));
                    }
                }
            }
            Op::DialogGoneById { pick } => {
                if let Some(id) = all_ids.get(*pick as usize % all_ids.len().max(1)) {
                    if let Some((ag, key, _)) = opened_by.get(id) {
                        unconfirmed.remove(&(ag.clone(), key.clone()));
                    }
                }
            }
            _ => {}
        }
        for id in &resolved {
            assert!(r.request(id).is_none(), "resolved request {id} is still there");
        }
        for ag in r.agents() {
            match (&ag.pending, open.get(&ag.id)) {
                (Some(p), Some(rid)) => {
                    assert_eq!(&p.id, rid);
                    assert!(matches!(p.status, RequestStatus::Open | RequestStatus::Answering));
                    assert!(r.request(rid).is_some());
                }
                (None, None) => {}
                (p, o) => panic!("pending {:?} but open {o:?}", p.as_ref().map(|p| &p.id)),
            }
        }
        for ag in open.keys() {
            assert!(live.contains(ag) && r.agent(ag).is_some(), "an open request of a missing agent");
        }
    }
});

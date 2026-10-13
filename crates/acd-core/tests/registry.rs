use acd_core::*;
use acd_protocol::*;

fn agent(id: &str) -> AgentObs {
    AgentObs {
        id: id.into(),
        source: Source::Herdr,
        kind: AgentKind::Claude,
        title: "sp1".into(),
        cwd: Some("/src/app".into()),
        project: Some("app".into()),
        state: AgentState::Working,
        failed: false,
        last_message: None,
        can_send: true,
        role: acd_protocol::AgentRole::Main,
        parent_title: None,
    }
}

fn perm(agent_id: &str, key: &str, detail: &str) -> RequestObs {
    RequestObs {
        agent_id: agent_id.into(),
        key: CorrelationKey(key.into()),
        upgrade_from: None,
        kind: RequestKind::Permission,
        answer_path: AnswerPath::Hook,
        title: "Bash".into(),
        detail: detail.into(),
        options: vec![
            RequestOption {
                id: "allow".into(),
                label: "Yes".into(),
                description: None,
                effect: Effect::Allow,
                confirm: false,
            },
            RequestOption {
                id: "deny".into(),
                label: "No".into(),
                description: None,
                effect: Effect::Deny,
                confirm: false,
            },
        ],
        questions: None,
        free_text: false,
        raw_screen: None,
        ready: true,
        reusable_key: true,
    }
}

fn opened(changes: &[Change]) -> Vec<Request> {
    changes
        .iter()
        .filter_map(|c| match c {
            Change::RequestOpened(r) => Some(r.clone()),
            _ => None,
        })
        .collect()
}

fn resolved(changes: &[Change]) -> Vec<(String, ResolvedBy, Option<String>)> {
    changes
        .iter()
        .filter_map(|c| match c {
            Change::RequestResolved {
                request_id,
                by,
                option_id,
                ..
            } => Some((request_id.clone(), *by, option_id.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn a_reason_reaches_the_resolution() {
    let mut r = Registry::new("m".into());
    r.apply(Observation::AgentUpsert(acd_core::testing::agent_obs("a")));
    r.apply(Observation::RequestSeen(acd_core::testing::permission_obs(
        "a", "k",
    )));
    let rid = r.agent("a").unwrap().pending.as_ref().unwrap().id.clone();
    let ch = r.apply(Observation::RequestEndedWithReason {
        request_id: rid.clone(),
        by: ResolvedBy::Uncertain,
        reason: Some(ResolveReason::StrayKeystroke),
        message: None,
    });
    assert!(ch.iter().any(|c| matches!(c, Change::RequestResolved {
        request_id, by: ResolvedBy::Uncertain, reason: Some(ResolveReason::StrayKeystroke), .. } if *request_id == rid)));
    assert!(
        r.apply(Observation::RequestEndedWithReason {
            request_id: rid,
            by: ResolvedBy::Uncertain,
            reason: Some(ResolveReason::StrayKeystroke),
            message: None,
        })
        .is_empty(),
        "a second end is a no-op"
    );
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
    assert!(
        r.apply(Observation::RequestSeen(perm("ghost", "k", "ls")))
            .is_empty()
    );
    assert!(
        r.apply(Observation::AgentGone {
            agent_id: "ghost".into()
        })
        .is_empty()
    );
    assert!(
        r.apply(Observation::RequestEnded {
            agent_id: "a1".into(),
            key: CorrelationKey("k".into()),
            by: ResolvedBy::Terminal
        })
        .is_empty()
    );
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
    let id = opened(&r.apply(Observation::RequestSeen(perm("a1", "k1", "ls"))))[0]
        .id
        .clone();
    let ch = r.apply(Observation::RequestSeen(perm("a1", "k1", "ls -la")));
    assert!(opened(&ch).is_empty());
    assert_eq!(r.agent("a1").unwrap().pending.as_ref().unwrap().id, id);
    assert_eq!(r.request(&id).unwrap().detail, "ls -la");
}

#[test]
fn semantic_updates_bump_revision_but_raw_screen_and_readiness_do_not() {
    let mut r = reg_with_agent();
    let mut original = perm("a1", "k1", "ls");
    original.raw_screen = Some("initial screen".into());
    let id = opened(&r.apply(Observation::RequestSeen(original.clone())))[0]
        .id
        .clone();
    assert_eq!(r.request(&id).unwrap().revision, 1);

    let mut raw_only = original.clone();
    raw_only.raw_screen = Some("updated screen".into());
    raw_only.ready = false;
    r.apply(Observation::RequestSeen(raw_only));
    assert_eq!(r.request(&id).unwrap().revision, 1);

    let mut changed = original;
    changed.detail = "updated detail".into();
    r.apply(Observation::RequestSeen(changed));
    assert_eq!(r.request(&id).unwrap().revision, 2);
}

#[test]
fn option_and_question_order_are_semantic() {
    let mut r = reg_with_agent();
    let mut obs = perm("a1", "ordered", "detail");
    obs.kind = RequestKind::Question;
    obs.answer_path = AnswerPath::T3;
    obs.options = vec![
        RequestOption {
            id: "one".into(),
            label: "One".into(),
            description: None,
            effect: Effect::Choose,
            confirm: false,
        },
        RequestOption {
            id: "two".into(),
            label: "Two".into(),
            description: None,
            effect: Effect::Choose,
            confirm: false,
        },
    ];
    obs.questions = Some(vec![Question {
        id: "q1".into(),
        header: None,
        text: "Pick".into(),
        multi_select: false,
        options: obs.options.clone(),
    }]);
    let id = opened(&r.apply(Observation::RequestSeen(obs.clone())))[0]
        .id
        .clone();
    // Each reordering on its own must bump, so neither comparison can hide
    // behind the other.
    obs.options.reverse();
    r.apply(Observation::RequestSeen(obs.clone()));
    assert_eq!(r.request(&id).unwrap().revision, 2);
    obs.questions.as_mut().unwrap()[0].options.reverse();
    r.apply(Observation::RequestSeen(obs.clone()));
    assert_eq!(r.request(&id).unwrap().revision, 3);
    let mut second = obs.questions.as_ref().unwrap()[0].clone();
    second.id = "q2".into();
    obs.questions.as_mut().unwrap().push(second);
    r.apply(Observation::RequestSeen(obs.clone()));
    assert_eq!(r.request(&id).unwrap().revision, 4);
    obs.questions.as_mut().unwrap().reverse();
    r.apply(Observation::RequestSeen(obs));
    assert_eq!(r.request(&id).unwrap().revision, 5);
}

#[test]
fn request_path_bumps_revision_and_invalidates_previous_answer() {
    let mut r = reg_with_agent();
    let id = opened(&r.apply(Observation::RequestSeen(perm("a1", "k1", "ls"))))[0]
        .id
        .clone();
    let stale = r.request(&id).unwrap().revision;
    r.apply(Observation::RequestPath {
        agent_id: "a1".into(),
        key: CorrelationKey("k1".into()),
        answer_path: AnswerPath::Keys,
    });
    let updated = r.request(&id).unwrap().revision;
    assert_eq!(updated, stale + 1);
    assert_eq!(
        r.claim(&id, Answer::Option("allow".into()), Some(stale))
            .unwrap_err(),
        ErrorCode::RequestChanged
    );
    assert_eq!(
        r.claim(&id, Answer::Option("allow".into()), Some(updated))
            .unwrap()
            .0
            .status,
        RequestStatus::Answering
    );
}

#[test]
fn different_key_supersedes() {
    let mut r = reg_with_agent();
    let first = opened(&r.apply(Observation::RequestSeen(perm("a1", "k1", "ls"))))[0]
        .id
        .clone();
    let ch = r.apply(Observation::RequestSeen(perm("a1", "k2", "rm x")));
    assert_eq!(resolved(&ch), vec![(first, ResolvedBy::Superseded, None)]);
    assert_eq!(opened(&ch).len(), 1);
}

#[test]
fn replay_of_resolved_key_is_ignored_until_dialog_gone_then_new_occurrence() {
    let mut r = reg_with_agent();
    let first = opened(&r.apply(Observation::RequestSeen(perm("a1", "k1", "ls"))))[0]
        .id
        .clone();
    r.apply(Observation::RequestEnded {
        agent_id: "a1".into(),
        key: CorrelationKey("k1".into()),
        by: ResolvedBy::Uncertain,
    });
    assert!(
        r.apply(Observation::RequestSeen(perm("a1", "k1", "ls")))
            .is_empty(),
        "replay must not reopen"
    );
    r.apply(Observation::DialogGone {
        agent_id: "a1".into(),
        key: CorrelationKey("k1".into()),
    });
    let second = opened(&r.apply(Observation::RequestSeen(perm("a1", "k1", "ls"))));
    assert_eq!(second.len(), 1);
    assert_ne!(second[0].id, first);
}

#[test]
fn terminal_end_confirms_gone_so_same_dialog_can_reopen() {
    let mut r = reg_with_agent();
    r.apply(Observation::RequestSeen(perm("a1", "k1", "ls")));
    let ch = r.apply(Observation::RequestEnded {
        agent_id: "a1".into(),
        key: CorrelationKey("k1".into()),
        by: ResolvedBy::Terminal,
    });
    assert_eq!(resolved(&ch)[0].1, ResolvedBy::Terminal);
    assert_eq!(
        opened(&r.apply(Observation::RequestSeen(perm("a1", "k1", "ls")))).len(),
        1
    );
}

#[test]
fn upgrade_from_fallback_keeps_request_id() {
    let mut r = reg_with_agent();
    let fallback = RequestObs {
        kind: RequestKind::BlockedUnparsed,
        answer_path: AnswerPath::None,
        raw_screen: Some("Do you want to proceed?".into()),
        options: vec![],
        ..perm("a1", "fp:1", "")
    };
    let id = opened(&r.apply(Observation::RequestSeen(fallback)))[0]
        .id
        .clone();
    let structured = RequestObs {
        upgrade_from: Some(CorrelationKey("fp:1".into())),
        ..perm("a1", "prompt:9", "ls")
    };
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
    let id = opened(&r.apply(Observation::RequestSeen(perm("a1", "k1", "ls"))))[0]
        .id
        .clone();
    let ch = r.apply(Observation::AgentGone {
        agent_id: "a1".into(),
    });
    assert_eq!(resolved(&ch), vec![(id.clone(), ResolvedBy::Lost, None)]);
    assert!(ch.contains(&Change::AgentRemoved("a1".into())));
    assert!(r.request(&id).is_none());
    assert!(r.agent("a1").is_none());
}

#[test]
fn path_and_readiness_updates_apply_to_open_requests() {
    let mut r = reg_with_agent();
    let id = opened(&r.apply(Observation::RequestSeen(RequestObs {
        ready: false,
        ..perm("a1", "k1", "ls")
    })))[0]
        .id
        .clone();
    r.apply(Observation::RequestPath {
        agent_id: "a1".into(),
        key: CorrelationKey("k1".into()),
        answer_path: AnswerPath::Keys,
    });
    assert_eq!(r.request(&id).unwrap().answer_path, AnswerPath::Keys);
    r.apply(Observation::RequestReady {
        agent_id: "a1".into(),
        key: CorrelationKey("k1".into()),
        ready: true,
    });
    assert!(r.is_ready(&id));
}

#[test]
fn replay_of_superseded_key_is_ignored() {
    let mut r = reg_with_agent();
    r.apply(Observation::RequestSeen(perm("a1", "k1", "ls")));
    let k2 = opened(&r.apply(Observation::RequestSeen(perm("a1", "k2", "rm x"))))[0]
        .id
        .clone();
    assert!(
        r.apply(Observation::RequestSeen(perm("a1", "k1", "ls")))
            .is_empty()
    );
    assert_eq!(r.agent("a1").unwrap().pending.as_ref().unwrap().id, k2);
}

#[test]
fn upgrade_aliases_fallback_key_and_cleans_up() {
    let mut r = reg_with_agent();
    let fallback = RequestObs {
        kind: RequestKind::BlockedUnparsed,
        answer_path: AnswerPath::None,
        raw_screen: Some("Do you want to proceed?".into()),
        options: vec![],
        ..perm("a1", "fp:1", "")
    };
    let id = opened(&r.apply(Observation::RequestSeen(fallback.clone())))[0]
        .id
        .clone();
    r.apply(Observation::RequestSeen(RequestObs {
        upgrade_from: Some(CorrelationKey("fp:1".into())),
        ..perm("a1", "prompt:9", "ls")
    }));
    assert!(
        r.apply(Observation::RequestSeen(fallback.clone()))
            .is_empty()
    );
    assert_eq!(r.request(&id).unwrap().kind, RequestKind::Permission);
    let ch = r.apply(Observation::RequestEnded {
        agent_id: "a1".into(),
        key: CorrelationKey("prompt:9".into()),
        by: ResolvedBy::Terminal,
    });
    assert_eq!(resolved(&ch)[0].0, id);
    assert!(r.request(&id).is_none());
    r.apply(Observation::DialogGone {
        agent_id: "a1".into(),
        key: CorrelationKey("fp:1".into()),
    });
    let again = opened(&r.apply(Observation::RequestSeen(fallback)));
    assert_eq!(again.len(), 1);
    assert_ne!(again[0].id, id);
}

#[test]
fn same_key_from_another_agent_is_independent() {
    let mut r = reg_with_agent();
    r.apply(Observation::AgentUpsert(agent("a2")));
    let a1req = opened(&r.apply(Observation::RequestSeen(perm("a1", "k1", "ls"))))[0]
        .id
        .clone();
    let ch = r.apply(Observation::RequestSeen(perm("a2", "k1", "pwd")));
    assert_eq!(opened(&ch).len(), 1);
    assert!(resolved(&ch).is_empty());
    assert_eq!(r.request(&a1req).unwrap().detail, "ls");
    assert_eq!(r.agent("a1").unwrap().pending.as_ref().unwrap().id, a1req);
}

#[test]
fn confirming_end_via_alias_key_resolves_upgraded_request() {
    let mut r = reg_with_agent();
    let fallback = RequestObs {
        kind: RequestKind::BlockedUnparsed,
        answer_path: AnswerPath::None,
        raw_screen: Some("Do you want to proceed?".into()),
        options: vec![],
        ..perm("a1", "fp:1", "")
    };
    let id = opened(&r.apply(Observation::RequestSeen(fallback.clone())))[0]
        .id
        .clone();
    r.apply(Observation::RequestSeen(RequestObs {
        upgrade_from: Some(CorrelationKey("fp:1".into())),
        ..perm("a1", "prompt:9", "ls")
    }));
    let ch = r.apply(Observation::RequestEnded {
        agent_id: "a1".into(),
        key: CorrelationKey("fp:1".into()),
        by: ResolvedBy::Terminal,
    });
    assert_eq!(
        resolved(&ch),
        vec![(id.clone(), ResolvedBy::Terminal, None)]
    );
    assert!(r.request(&id).is_none());
    r.apply(Observation::DialogGone {
        agent_id: "a1".into(),
        key: CorrelationKey("fp:1".into()),
    });
    let again = opened(&r.apply(Observation::RequestSeen(fallback)));
    assert_eq!(again.len(), 1);
    assert_ne!(again[0].id, id);
}

#[test]
fn unconfirmed_end_via_alias_key_is_skipped() {
    let mut r = reg_with_agent();
    let fallback = RequestObs {
        kind: RequestKind::BlockedUnparsed,
        answer_path: AnswerPath::None,
        options: vec![],
        ..perm("a1", "fp:1", "")
    };
    let id = opened(&r.apply(Observation::RequestSeen(fallback)))[0]
        .id
        .clone();
    r.apply(Observation::RequestSeen(RequestObs {
        upgrade_from: Some(CorrelationKey("fp:1".into())),
        ..perm("a1", "prompt:9", "ls")
    }));
    assert!(
        r.apply(Observation::RequestEnded {
            agent_id: "a1".into(),
            key: CorrelationKey("fp:1".into()),
            by: ResolvedBy::Uncertain
        })
        .is_empty()
    );
    assert!(r.request(&id).is_some());
}

#[test]
fn key_only_observations_are_scoped_to_their_agent() {
    let mut r = reg_with_agent();
    r.apply(Observation::AgentUpsert(agent("a2")));
    let a1req = opened(&r.apply(Observation::RequestSeen(RequestObs {
        ready: false,
        ..perm("a1", "k1", "ls")
    })))[0]
        .id
        .clone();
    let a2req = opened(&r.apply(Observation::RequestSeen(RequestObs {
        ready: false,
        ..perm("a2", "k1", "pwd")
    })))[0]
        .id
        .clone();

    r.apply(Observation::RequestReady {
        agent_id: "a1".into(),
        key: CorrelationKey("k1".into()),
        ready: true,
    });
    assert!(r.is_ready(&a1req));
    assert!(!r.is_ready(&a2req));

    r.apply(Observation::RequestPath {
        agent_id: "a1".into(),
        key: CorrelationKey("k1".into()),
        answer_path: AnswerPath::Keys,
    });
    assert_eq!(r.request(&a1req).unwrap().answer_path, AnswerPath::Keys);
    assert_eq!(r.request(&a2req).unwrap().answer_path, AnswerPath::Hook);

    let ch = r.apply(Observation::RequestEnded {
        agent_id: "a1".into(),
        key: CorrelationKey("k1".into()),
        by: ResolvedBy::Terminal,
    });
    assert_eq!(
        resolved(&ch),
        vec![(a1req.clone(), ResolvedBy::Terminal, None)]
    );
    assert!(
        r.request(&a2req).is_some(),
        "a2's request with the same key stays open"
    );
    assert_eq!(r.agent("a2").unwrap().pending.as_ref().unwrap().id, a2req);
}

#[test]
fn dialog_gone_is_scoped_to_its_agent() {
    let mut r = reg_with_agent();
    r.apply(Observation::AgentUpsert(agent("a2")));
    for a in ["a1", "a2"] {
        r.apply(Observation::RequestSeen(perm(a, "k1", "ls")));
        r.apply(Observation::RequestEnded {
            agent_id: a.into(),
            key: CorrelationKey("k1".into()),
            by: ResolvedBy::Uncertain,
        });
    }
    r.apply(Observation::DialogGone {
        agent_id: "a1".into(),
        key: CorrelationKey("k1".into()),
    });
    assert_eq!(
        opened(&r.apply(Observation::RequestSeen(perm("a1", "k1", "ls")))).len(),
        1
    );
    assert!(
        r.apply(Observation::RequestSeen(perm("a2", "k1", "ls")))
            .is_empty(),
        "a2's dialog was never confirmed gone"
    );
}

#[test]
fn resolved_non_reusable_key_never_reopens() {
    let mut r = reg_with_agent();
    let once = |key: &str| RequestObs {
        reusable_key: false,
        ..perm("a1", key, "ls")
    };
    r.apply(Observation::RequestSeen(once("K1")));
    r.apply(Observation::RequestEnded {
        agent_id: "a1".into(),
        key: CorrelationKey("K1".into()),
        by: ResolvedBy::Terminal,
    });
    let k2 = opened(&r.apply(Observation::RequestSeen(once("K2"))))[0]
        .id
        .clone();
    assert!(
        r.apply(Observation::RequestSeen(once("K1"))).is_empty(),
        "replay after a confirming end"
    );
    r.apply(Observation::DialogGone {
        agent_id: "a1".into(),
        key: CorrelationKey("K1".into()),
    });
    assert!(
        r.apply(Observation::RequestSeen(once("K1"))).is_empty(),
        "replay after dialog gone"
    );
    assert_eq!(r.agent("a1").unwrap().pending.as_ref().unwrap().id, k2);
    assert_eq!(r.request(&k2).unwrap().status, RequestStatus::Open);
}

#[test]
fn reusable_key_reopens_after_confirmed_end() {
    let mut r = reg_with_agent();
    let first = opened(&r.apply(Observation::RequestSeen(perm("a1", "K1", "ls"))))[0]
        .id
        .clone();
    r.apply(Observation::RequestEnded {
        agent_id: "a1".into(),
        key: CorrelationKey("K1".into()),
        by: ResolvedBy::Terminal,
    });
    let again = opened(&r.apply(Observation::RequestSeen(perm("a1", "K1", "ls"))));
    assert_eq!(again.len(), 1);
    assert_ne!(again[0].id, first);
}

#[test]
fn replayed_lost_end_is_a_no_op() {
    let mut r = reg_with_agent();
    r.apply(Observation::RequestSeen(perm("a1", "k", "ls")));
    let first = r.apply(Observation::RequestEnded {
        agent_id: "a1".into(),
        key: CorrelationKey("k".into()),
        by: ResolvedBy::Lost,
    });
    assert_eq!(resolved(&first).len(), 1);
    let again = r.apply(Observation::RequestEnded {
        agent_id: "a1".into(),
        key: CorrelationKey("k".into()),
        by: ResolvedBy::Lost,
    });
    assert!(
        again.is_empty(),
        "a replayed Lost must not emit anything: {again:?}"
    );
}

#[test]
fn request_path_is_ignored_unless_open() {
    let mut r = reg_with_agent();
    let rid = opened(&r.apply(Observation::RequestSeen(perm("a1", "k", "ls"))))[0]
        .id
        .clone();
    r.claim(&rid, Answer::Option("allow".into()), None).unwrap();
    let changes = r.apply(Observation::RequestPath {
        agent_id: "a1".into(),
        key: CorrelationKey("k".into()),
        answer_path: AnswerPath::Keys,
    });
    assert!(changes.is_empty());
    assert_eq!(r.request(&rid).unwrap().answer_path, AnswerPath::Hook);
}

#[test]
fn removed_then_readded_agent_starts_clean() {
    let mut r = reg_with_agent();
    let rid = opened(&r.apply(Observation::RequestSeen(perm("a1", "k", "ls"))))[0]
        .id
        .clone();
    r.apply(Observation::AgentGone {
        agent_id: "a1".into(),
    });
    assert!(r.request(&rid).is_none());
    r.apply(Observation::AgentUpsert(agent("a1")));
    assert!(r.agent("a1").unwrap().pending.is_none());
    let again = opened(&r.apply(Observation::RequestSeen(perm("a1", "k", "ls"))));
    assert_eq!(again.len(), 1);
    assert_ne!(again[0].id, rid);
}

#[test]
fn end_by_id_touches_only_that_occurrence() {
    let mut r = reg_with_agent();
    let k = CorrelationKey("k".into());
    let first = opened(&r.apply(Observation::RequestSeen(perm("a1", "k", "ls"))))[0]
        .id
        .clone();
    assert_eq!(r.request_id_for("a1", &k), Some(first.clone()));
    let ended = r.apply(Observation::RequestEndedById {
        request_id: first.clone(),
        by: ResolvedBy::Terminal,
    });
    assert_eq!(
        resolved(&ended),
        vec![(first.clone(), ResolvedBy::Terminal, None)]
    );
    // Confirmed gone, so the reusable key opens a second occurrence.
    let second = opened(&r.apply(Observation::RequestSeen(perm("a1", "k", "ls"))))[0]
        .id
        .clone();
    assert_ne!(second, first);
    // A late end for the first occurrence must not resolve the second.
    assert!(
        r.apply(Observation::RequestEndedById {
            request_id: first,
            by: ResolvedBy::Terminal
        })
        .is_empty()
    );
    assert_eq!(r.request(&second).unwrap().status, RequestStatus::Open);
}

#[test]
fn late_dialog_gone_does_not_touch_a_newer_occurrence() {
    let mut r = reg_with_agent();
    let a = opened(&r.apply(Observation::RequestSeen(perm("a1", "k", "ls"))))[0]
        .id
        .clone();
    r.apply(Observation::RequestEndedById {
        request_id: a.clone(),
        by: ResolvedBy::Terminal,
    });
    let b = opened(&r.apply(Observation::RequestSeen(perm("a1", "k", "ls"))))[0]
        .id
        .clone();
    r.apply(Observation::RequestEndedById {
        request_id: b.clone(),
        by: ResolvedBy::Uncertain,
    }); // B not confirmed gone
    r.apply(Observation::DialogGoneById { request_id: a }); // late, for A
    assert!(
        opened(&r.apply(Observation::RequestSeen(perm("a1", "k", "ls")))).is_empty(),
        "B's key must stay unconfirmed, so a replay of B's dialog is not a new occurrence"
    );
    r.apply(Observation::DialogGoneById { request_id: b });
    assert_eq!(
        opened(&r.apply(Observation::RequestSeen(perm("a1", "k", "ls")))).len(),
        1
    );
}

#[test]
fn fallback_never_supersedes_and_never_ends_an_upgraded_request() {
    let mut r = reg_with_agent();
    let mut fb = perm("a1", "fb", "");
    fb.kind = RequestKind::BlockedUnparsed;
    fb.answer_path = AnswerPath::None;
    fb.options = vec![];
    // A structured request is pending: FallbackSeen is a no-op.
    let hook = opened(&r.apply(Observation::RequestSeen(perm("a1", "hook", "ls"))))[0]
        .id
        .clone();
    assert!(r.apply(Observation::FallbackSeen(fb.clone())).is_empty());
    assert_eq!(r.agent("a1").unwrap().pending.as_ref().unwrap().id, hook);
    r.apply(Observation::RequestEndedById {
        request_id: hook,
        by: ResolvedBy::Terminal,
    });
    // Nothing pending: FallbackSeen opens; P3 upgrades it in place.
    let rid = opened(&r.apply(Observation::FallbackSeen(fb)))[0]
        .id
        .clone();
    let mut up = perm("a1", "hook2", "ls");
    up.upgrade_from = Some(CorrelationKey("fb".into()));
    r.apply(Observation::RequestSeen(up));
    assert!(
        resolved(&r.apply(Observation::FallbackEnded {
            agent_id: "a1".into(),
            key: CorrelationKey("fb".into()),
            request_id: rid.clone()
        }))
        .is_empty()
    );
    assert_eq!(r.request(&rid).unwrap().kind, RequestKind::Permission);
    // The structured dialog is still up. If its request resolves uncertain, a replay
    // of it must NOT open a new occurrence: only the fallback key was confirmed gone.
    r.apply(Observation::RequestEndedById {
        request_id: rid,
        by: ResolvedBy::Uncertain,
    });
    assert!(opened(&r.apply(Observation::RequestSeen(perm("a1", "hook2", "ls")))).is_empty());
}

#[test]
fn end_by_id_after_upgrade_uses_the_upgraded_request() {
    let mut r = reg_with_agent();
    let mut fb = perm("a1", "fb", "");
    fb.kind = RequestKind::BlockedUnparsed;
    fb.answer_path = AnswerPath::None;
    fb.options = vec![];
    let rid = opened(&r.apply(Observation::RequestSeen(fb)))[0].id.clone();
    let mut up = perm("a1", "hook", "ls");
    up.upgrade_from = Some(CorrelationKey("fb".into()));
    r.apply(Observation::RequestSeen(up));
    assert_eq!(
        r.request(&rid).unwrap().kind,
        RequestKind::Permission,
        "upgraded in place, same id"
    );
    assert_eq!(
        r.request_id_for("a1", &CorrelationKey("fb".into())),
        None,
        "the alias is not a primary key"
    );
}

#[test]
fn unknown_upsert_keeps_the_seen_marker() {
    let mut r = Registry::new("m1".into());
    let done = AgentObs {
        state: AgentState::Done,
        ..agent("a1")
    };
    r.apply(Observation::AgentUpsert(done.clone()));
    r.mark_seen("a1").unwrap();
    // A source outage reports the agent unknown, then restores it.
    r.apply(Observation::AgentUpsert(AgentObs {
        state: AgentState::Unknown,
        ..agent("a1")
    }));
    assert_eq!(r.agent("a1").unwrap().state, AgentState::Unknown);
    r.apply(Observation::AgentUpsert(done.clone()));
    assert_eq!(
        r.agent("a1").unwrap().state,
        AgentState::Idle,
        "still seen after the outage"
    );
    // A real state change still clears it.
    r.apply(Observation::AgentUpsert(agent("a1")));
    r.apply(Observation::AgentUpsert(done));
    assert_eq!(r.agent("a1").unwrap().state, AgentState::Done);
}

#[test]
fn end_if_open_leaves_a_claimed_request_alone() {
    let mut r = reg_with_agent();
    let rid = opened(&r.apply(Observation::RequestSeen(perm("a1", "k", "ls"))))[0]
        .id
        .clone();
    r.claim(&rid, Answer::Option("allow".into()), None).unwrap();
    let ended = r.apply(Observation::RequestEndedIfOpen {
        request_id: rid.clone(),
        by: ResolvedBy::Terminal,
    });
    assert!(ended.is_empty(), "{ended:?}");
    assert_eq!(r.request(&rid).unwrap().status, RequestStatus::Answering);
    r.release(&rid);
    let ended = r.apply(Observation::RequestEndedIfOpen {
        request_id: rid.clone(),
        by: ResolvedBy::Terminal,
    });
    assert_eq!(
        resolved(&ended),
        vec![(rid.clone(), ResolvedBy::Terminal, None)]
    );
    // Confirmed gone like RequestEndedById, so the reusable key opens a new occurrence.
    assert_eq!(
        opened(&r.apply(Observation::RequestSeen(perm("a1", "k", "ls")))).len(),
        1
    );
    assert!(
        r.apply(Observation::RequestEndedIfOpen {
            request_id: rid,
            by: ResolvedBy::Terminal
        })
        .is_empty()
    );
}

#[test]
fn answering_request_superseded_is_uncertain_but_open_request_is_superseded() {
    let mut r = reg_with_agent();
    let old = opened(&r.apply(Observation::RequestSeen(perm("a1", "k1", "old"))))[0]
        .id
        .clone();
    r.claim(&old, Answer::Option("allow".into()), None).unwrap();
    let changes = r.apply(Observation::RequestSeen(perm("a1", "k2", "new")));
    assert_eq!(
        resolved(&changes),
        vec![(old.clone(), ResolvedBy::Uncertain, None)]
    );
    assert_eq!(r.request(&old), None);
    assert_eq!(opened(&changes).len(), 1);
    assert!(
        r.apply(Observation::RequestEndedById {
            request_id: old,
            by: ResolvedBy::Inbox
        })
        .is_empty()
    );

    let mut r = reg_with_agent();
    let old = opened(&r.apply(Observation::RequestSeen(perm("a1", "k1", "old"))))[0]
        .id
        .clone();
    let changes = r.apply(Observation::RequestSeen(perm("a1", "k2", "new")));
    assert_eq!(
        resolved(&changes),
        vec![(old, ResolvedBy::Superseded, None)]
    );
}

#[test]
fn answering_agent_gone_is_uncertain_but_open_agent_gone_is_lost() {
    let mut r = reg_with_agent();
    let id = opened(&r.apply(Observation::RequestSeen(perm("a1", "k", "old"))))[0]
        .id
        .clone();
    r.claim(&id, Answer::Option("allow".into()), None).unwrap();
    let changes = r.apply(Observation::AgentGone {
        agent_id: "a1".into(),
    });
    assert_eq!(
        resolved(&changes),
        vec![(id.clone(), ResolvedBy::Uncertain, None)]
    );
    assert!(
        r.apply(Observation::RequestEndedById {
            request_id: id,
            by: ResolvedBy::Inbox
        })
        .is_empty()
    );

    let mut r = reg_with_agent();
    let id = opened(&r.apply(Observation::RequestSeen(perm("a1", "k", "old"))))[0]
        .id
        .clone();
    assert_eq!(
        resolved(&r.apply(Observation::AgentGone {
            agent_id: "a1".into()
        })),
        vec![(id, ResolvedBy::Lost, None)]
    );
}

#[test]
fn typed_answers_with_bidi_or_control_characters_are_refused_at_claim() {
    let mut r = reg_with_agent();
    let mut obs = perm("a1", "k", "q");
    obs.kind = RequestKind::Question;
    obs.answer_path = AnswerPath::T3;
    obs.free_text = true;
    obs.questions = Some(vec![Question {
        id: "q".into(),
        header: None,
        text: "q".into(),
        multi_select: false,
        options: obs.options.clone(),
    }]);
    let rid = opened(&r.apply(Observation::RequestSeen(obs)))[0]
        .id
        .clone();
    for bad in ["a\u{202E}b", "a\u{2028}b", "a\u{1b}b", "   "] {
        assert_eq!(
            r.claim(&rid, Answer::Text(bad.into()), None).unwrap_err(),
            ErrorCode::BadRequest,
            "{bad:?}"
        );
    }
    assert!(
        r.claim(&rid, Answer::Text("fine\ntext".into()), None)
            .is_ok()
    );
}

#[test]
fn a_message_reaches_the_resolution_and_a_plain_end_carries_none() {
    let mut r = Registry::new("m".into());
    r.apply(Observation::AgentUpsert(acd_core::testing::agent_obs("a")));
    r.apply(Observation::RequestSeen(acd_core::testing::permission_obs("a", "k")));
    let rid = r.agent("a").unwrap().pending.as_ref().unwrap().id.clone();
    let ch = r.apply(Observation::RequestEndedWithReason {
        request_id: rid.clone(),
        by: ResolvedBy::Agent,
        reason: Some(ResolveReason::Rejected),
        message: Some("stale plan".into()),
    });
    assert!(ch.iter().any(|c| matches!(c, Change::RequestResolved {
        by: ResolvedBy::Agent, reason: Some(ResolveReason::Rejected), message: Some(m), .. } if m == "stale plan")));

    r.apply(Observation::RequestSeen(acd_core::testing::permission_obs("a", "k2")));
    let rid = r.agent("a").unwrap().pending.as_ref().unwrap().id.clone();
    let ch = r.apply(Observation::RequestEndedById { request_id: rid, by: ResolvedBy::Terminal });
    assert!(ch.iter().any(|c| matches!(c, Change::RequestResolved { message: None, reason: None, .. })));
}

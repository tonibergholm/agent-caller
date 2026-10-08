use acd_adapters::t3::wire::*;
use serde_json::Value;

fn fx(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../tests/fixtures/t3/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}
fn one(name: &str) -> ServerFrame {
    let mut f = parse_frames(&fx(name)).unwrap();
    assert_eq!(f.len(), 1);
    f.remove(0)
}
fn chunk_values(name: &str) -> Vec<Value> {
    match one(name) {
        ServerFrame::Chunk { values, .. } => values,
        f => panic!("{f:?}"),
    }
}

#[test]
fn every_fixture_frame_decodes() {
    let dir = format!("{}/../../tests/fixtures/t3", env!("CARGO_MANIFEST_DIR"));
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap().to_str().unwrap().to_owned();
        if name.starts_with("http_")
            || name.starts_with("cmd_request")
            || name.starts_with("rpc_request")
            || name == "rpc_ack.json"
            || name == "rpc_interrupt.json"
        {
            continue;
        }
        let text = std::fs::read_to_string(&p).unwrap();
        // `*_frames.json` files are arrays of frames, including the client Request.
        let frames = parse_frames(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(!frames.is_empty(), "{name}");
        let raw: Value = serde_json::from_str(&text).unwrap();
        let raw = match raw {
            Value::Array(a) => a,
            v => vec![v],
        };
        for (r, f) in raw.iter().zip(frames) {
            let tag = r["_tag"].as_str().unwrap();
            if ["Chunk", "Exit", "Ping", "Pong"].contains(&tag) {
                assert!(
                    !matches!(f, ServerFrame::Other),
                    "{name}: {tag} decoded as Other"
                );
            }
            match f {
                ServerFrame::Exit { exit, .. } => {
                    assert!(
                        !matches!(exit, ExitValue::Other),
                        "{name}: exit decoded as Other"
                    );
                }
                ServerFrame::Chunk { values, .. } => {
                    for v in values {
                        let shell = name.starts_with("shell_") || name.starts_with("rpc_chunk");
                        let kind = v["kind"].as_str().unwrap_or("").to_owned();
                        if shell {
                            let item =
                                ShellItem::decode(&v).unwrap_or_else(|e| panic!("{name}: {e}"));
                            let known = [
                                "snapshot",
                                "synchronized",
                                "thread.updated",
                                "thread.removed",
                                "project.updated",
                            ];
                            assert!(
                                !known.contains(&kind.as_str())
                                    || !matches!(item, ShellItem::Other),
                                "{name}: {kind} decoded as Other"
                            );
                        } else {
                            let item =
                                ThreadItem::decode(&v).unwrap_or_else(|e| panic!("{name}: {e}"));
                            let known = ["snapshot", "synchronized", "event"];
                            assert!(
                                !known.contains(&kind.as_str())
                                    || !matches!(item, ThreadItem::Other),
                                "{name}: {kind} decoded as Other"
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

#[test]
fn exits_classify() {
    let ServerFrame::Exit {
        exit: ExitValue::Success { value },
        ..
    } = one("rpc_exit_success_dispatch.json")
    else {
        panic!()
    };
    assert_eq!(value["sequence"], 2);
    let ServerFrame::Exit {
        exit: ExitValue::Failure { cause },
        ..
    } = one("cmd_exit_respond_fail_already_resolved.json")
    else {
        panic!()
    };
    let Failure::Fail { tag, message } = failure(&cause) else {
        panic!()
    };
    assert_eq!(tag, "OrchestrationV2DispatchCommandError");
    assert!(message.ends_with(" is resolved."));
    let ServerFrame::Exit {
        exit: ExitValue::Failure { cause },
        ..
    } = one("rpc_exit_die_bad_decision_literal.json")
    else {
        panic!()
    };
    assert!(matches!(failure(&cause), Failure::Die(d) if d.contains("decision")));
    let ServerFrame::Exit {
        exit: ExitValue::Failure { cause },
        ..
    } = one("rpc_exit_interrupted_stream.json")
    else {
        panic!()
    };
    assert_eq!(failure(&cause), Failure::Interrupted);
    let ServerFrame::Exit {
        exit: ExitValue::Failure { cause },
        ..
    } = one("rpc_exit_fail_missing_scope_operate.json")
    else {
        panic!()
    };
    assert!(
        matches!(failure(&cause), Failure::Fail { tag, .. } if tag == "EnvironmentAuthorizationError")
    );
}

#[test]
fn multi_cause_failures_are_never_a_single_fail() {
    let fail = serde_json::json!({"_tag": "Fail", "error": {"_tag": "EnvironmentAuthorizationError", "message": "m"}});
    let die = serde_json::json!({"_tag": "Die", "defect": "d"});
    let int = serde_json::json!({"_tag": "Interrupt", "fiberId": 1});
    let causes = |v: Vec<serde_json::Value>| -> Vec<Cause> {
        serde_json::from_value(serde_json::Value::Array(v)).unwrap()
    };
    assert!(matches!(
        failure(&causes(vec![fail.clone()])),
        Failure::Fail { .. }
    ));
    for mixed in [
        vec![fail.clone(), die.clone()],
        vec![die.clone(), fail.clone()],
        vec![fail.clone(), fail.clone()],
        vec![fail.clone(), int.clone()],
        vec![die.clone(), int.clone()],
    ] {
        assert!(
            matches!(failure(&causes(mixed.clone())), Failure::Mixed(ref c) if c.len() == mixed.len()),
            "{mixed:?}"
        );
    }
    assert!(matches!(
        failure(&causes(vec![die.clone(), die])),
        Failure::Die(_)
    ));
    assert_eq!(
        failure(&causes(vec![int.clone(), int])),
        Failure::Interrupted
    );
    assert_eq!(failure(&[]), Failure::Unknown);
}

#[test]
fn shell_items_and_the_project_only_marker() {
    let v = &chunk_values("shell_chunk_full_snapshot_with_pending_request.json")[0];
    assert!(!is_project_only(v));
    let ShellItem::Snapshot { snapshot } = ShellItem::decode(v).unwrap() else {
        panic!()
    };
    assert_eq!(snapshot.snapshot_sequence, 24);
    let t = ShellThread::decode(&snapshot.threads[0]).unwrap();
    assert_eq!(t.status.as_deref(), Some("running"));
    assert_eq!(t.pending_runtime_request.as_ref().unwrap().kind, "command");
    let p = Project::decode(&snapshot.projects[0]).unwrap();
    assert_eq!(p.workspace_root.as_deref(), Some("/tmp/acp4/proj"));

    let v = &chunk_values("shell_chunk_project_metadata_snapshot_threads_empty.json")[0];
    assert!(is_project_only(v));

    let v = &chunk_values("shell_chunk_thread_updated_waiting_postturn_no_request.json")[0];
    assert!(item_sequence(v).is_some());
    let ShellItem::ThreadUpdated {
        location, thread, ..
    } = ShellItem::decode(v).unwrap()
    else {
        panic!()
    };
    assert_eq!(location, "active");
    let t = ShellThread::decode(&thread).unwrap();
    assert_eq!(t.status.as_deref(), Some("waiting"));
    assert!(t.pending_runtime_request.is_none());
}

#[test]
fn unknown_kinds_decode_as_other() {
    let v: Value = serde_json::json!({"kind": "thread.teleported", "sequence": 9});
    assert!(matches!(ShellItem::decode(&v).unwrap(), ShellItem::Other));
    assert_eq!(item_sequence(&v), Some(9));
    let v: Value = serde_json::json!({"kind": "event", "sequence": 5, "event": {"type": "galaxy.updated", "payload": {}}});
    let ThreadItem::Event { event, .. } = ThreadItem::decode(&v).unwrap() else {
        panic!()
    };
    assert_eq!(event.kind, "galaxy.updated");
    let frames = parse_frames(r#"[{"_tag":"Pong"},{"_tag":"Telemetry","x":1}]"#).unwrap();
    assert!(matches!(frames[0], ServerFrame::Pong {}));
    assert!(matches!(frames[1], ServerFrame::Other));
    assert!(matches!(
        turn_item(&serde_json::json!({"type": "hologram", "id": "x"})).unwrap(),
        TurnItem::Other
    ));
}

#[test]
fn turn_items_from_the_claude_question_snapshot() {
    let v = &chunk_values("thread_chunk_snapshot_waiting_question_claude.json")[0];
    let ThreadItem::Snapshot { projection, .. } = ThreadItem::decode(v).unwrap() else {
        panic!()
    };
    let items: Vec<TurnItem> = projection["turnItems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| turn_item(i).unwrap())
        .collect();
    let q = items
        .iter()
        .find_map(|i| match i {
            TurnItem::UserInputRequest { questions, .. } => Some(questions.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(q[0].id, "Which color do you prefer?");
    assert_eq!(
        q[0].options
            .iter()
            .map(|o| o.label.as_str())
            .collect::<Vec<_>>(),
        ["red", "blue"]
    );
    assert!(q[0].options.iter().all(|o| o.value.is_none()));
    assert!(!q[0].multi_select);
}

#[test]
fn client_frames_match_the_recording() {
    let want: Value = serde_json::from_str(&fx("rpc_request_subscribeShell.json")).unwrap();
    let got: Value = serde_json::from_str(&request_frame(
        "1",
        SUBSCRIBE_SHELL,
        &serde_json::json!({"requestCompletionMarker": true}),
    ))
    .unwrap();
    assert_eq!(got, want);
    let want: Value = serde_json::from_str(&fx("rpc_ack.json")).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&ack_frame("1")).unwrap(),
        want
    );
    let want: Value = serde_json::from_str(&fx("rpc_interrupt.json")).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&interrupt_frame("1")).unwrap(),
        want
    );
}

#[test]
fn a_bad_array_element_never_drops_a_chunk() {
    let text = r#"[{"_tag":"Chunk","requestId":"1","values":[]},{"_tag":"Chunk","values":[]},"junk",{"_tag":"Exit","requestId":"1","exit":{"_tag":"Success"}}]"#;
    let f = parse_frames(text);
    assert!(
        f.is_err(),
        "malformed Chunk requestId closes the protocol read"
    );
    let f = parse_frames(r#"[{"_tag":"Chunk","requestId":"1","values":[]},"junk",{"_tag":"Exit","requestId":"1","exit":{"_tag":"Success"}}]"#).unwrap();
    assert_eq!(f.len(), 3);
    assert!(matches!(&f[0], ServerFrame::Chunk { request_id, .. } if request_id == "1"));
    assert!(matches!(f[1], ServerFrame::Other));
    assert!(matches!(f[2], ServerFrame::Exit { .. }));
    assert!(parse_frames(r#"{"_tag":"Chunk","values":[]}"#).is_err());
    assert!(parse_frames(r#"{"_tag":"Chunk","requestId":42,"values":[]}"#).is_err());
    assert!(parse_frames("not json").is_err());
}

#[test]
fn unknown_exit_tag_decodes_as_other() {
    let f = parse_frames(r#"{"_tag":"Exit","requestId":"2","exit":{"_tag":"Timeout"}}"#).unwrap();
    assert!(matches!(
        f[0],
        ServerFrame::Exit {
            exit: ExitValue::Other,
            ..
        }
    ));
}

#[test]
fn one_malformed_project_does_not_fail_the_snapshot() {
    let v = serde_json::json!({"kind":"snapshot","snapshot":{"snapshotSequence":3,"threads":[],
        "projects":[{"title":"no id"},{"id":"p2","workspaceRoot":"/w"}]}});
    let ShellItem::Snapshot { snapshot } = ShellItem::decode(&v).unwrap() else {
        panic!()
    };
    assert!(Project::decode(&snapshot.projects[0]).is_err());
    assert_eq!(Project::decode(&snapshot.projects[1]).unwrap().id, "p2");
}

fn projection(name: &str) -> Value {
    let v = &chunk_values(name)[0];
    let ThreadItem::Snapshot { projection, .. } = ThreadItem::decode(v).unwrap() else {
        panic!()
    };
    projection
}

#[test]
fn approval_snapshot_is_pinned() {
    let p = projection("thread_chunk_snapshot_waiting_approval_claude.json");
    let items: Vec<TurnItem> = p["turnItems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| turn_item(i).unwrap())
        .collect();
    let TurnItem::ApprovalRequest {
        id,
        request_id,
        request_kind,
        prompt,
        ..
    } = items
        .iter()
        .find(|i| matches!(i, TurnItem::ApprovalRequest { .. }))
        .unwrap()
    else {
        panic!()
    };
    assert!(id.starts_with("turn-item:runtime-request:"));
    assert!(!request_id.is_empty());
    assert_eq!(request_kind.as_deref(), Some("command"));
    assert_eq!(prompt.as_deref(), Some("Create empty file x.txt"));
    let TurnItem::CommandExecution {
        status,
        input,
        native_item_ref,
        ..
    } = items
        .iter()
        .find(|i| matches!(i, TurnItem::CommandExecution { .. }))
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(status.as_deref(), Some("running"));
    assert_eq!(input.as_deref(), Some("touch /tmp/acp4/proj/x.txt"));
    assert!(native_item_ref.as_ref().unwrap().native_id.is_some());

    let rr: Vec<RuntimeRequest> = serde_json::from_value(p["runtimeRequests"].clone()).unwrap();
    assert_eq!(
        (rr[0].kind.as_str(), rr[0].status.as_str()),
        ("command", "pending")
    );
    assert_eq!(rr[0].id, p["runtimeRequests"][0]["id"].as_str().unwrap());

    let pt: Vec<ProviderThread> = serde_json::from_value(p["providerThreads"].clone()).unwrap();
    assert_eq!(
        pt[0]
            .native_thread_ref
            .as_ref()
            .unwrap()
            .native_id
            .as_deref(),
        Some("d67b7431-38fa-47ca-b40d-11d54f568263")
    );
}

#[test]
fn shell_project_updated_and_synchronized_are_pinned() {
    let v = &chunk_values("shell_chunk_project_updated.json")[0];
    let ShellItem::ProjectUpdated { sequence, project } = ShellItem::decode(v).unwrap() else {
        panic!()
    };
    assert_eq!((sequence, project.title.as_deref()), (1, Some("acp4")));
    assert_eq!(project.workspace_root.as_deref(), Some("/tmp/acp4/proj"));
    let v = &chunk_values("rpc_chunk_synchronized.json")[0];
    assert!(matches!(
        ShellItem::decode(v).unwrap(),
        ShellItem::Synchronized {}
    ));
    assert!(matches!(
        ThreadItem::decode(v).unwrap(),
        ThreadItem::Synchronized {}
    ));
}

#[test]
fn thread_events_are_pinned() {
    let v = &chunk_values("thread_event_runtime_request_pending_command_claude.json")[0];
    let ThreadItem::Event { sequence, event } = ThreadItem::decode(v).unwrap() else {
        panic!()
    };
    assert_eq!(sequence, item_sequence(v).unwrap());
    assert_eq!(event.kind, "node.updated");
    assert_eq!(event.payload["kind"], "approval_request");
    assert_eq!(event.payload["status"], "waiting");
}

use acd_adapters::t3::{detail::*, map::*};
use acd_core::{Answer, CorrelationKey};
use acd_protocol::*;
use std::collections::HashSet;

fn shell_thread(name: &str) -> ShellThread {
    let ShellItem::ThreadUpdated { thread, .. } =
        ShellItem::decode(&chunk_values(name)[0]).unwrap()
    else {
        panic!()
    };
    ShellThread::decode(&thread).unwrap()
}
fn pending_id(p: &Value) -> String {
    p["runtimeRequests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["status"] == "pending")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn state_follows_the_pending_request_not_the_status() {
    assert_eq!(state(Some("running"), true), (AgentState::NeedsYou, false));
    assert_eq!(state(Some("waiting"), false), (AgentState::Working, false));
    for s in ["running", "starting", "preparing", "queued"] {
        assert_eq!(state(Some(s), false).0, AgentState::Working);
    }
    assert_eq!(state(Some("completed"), false), (AgentState::Done, false));
    assert_eq!(state(Some("failed"), false), (AgentState::Done, true));
    assert_eq!(state(Some("interrupted"), false), (AgentState::Done, true));
    for s in ["idle", "cancelled", "rolled_back"] {
        assert_eq!(state(Some(s), false).0, AgentState::Idle);
    }
    assert_eq!(state(Some("teleporting"), false).0, AgentState::Unknown);
    assert_eq!(state(None, false).0, AgentState::Unknown);
}

#[test]
fn agent_obs_from_recorded_shell_entries() {
    let t = shell_thread("shell_chunk_thread_updated_running_pending_approval_claude.json");
    let project = Project {
        id: t.project_id.clone().unwrap(),
        title: Some("acp4".into()),
        workspace_root: Some("/tmp/acp4/proj".into()),
    };
    let o = agent_obs(&t, Some(&project), &HashSet::new());
    assert_eq!(o.id, format!("t3:{}", t.id));
    assert_eq!(
        (o.source, o.kind, o.state),
        (Source::T3, AgentKind::Claude, AgentState::NeedsYou)
    );
    assert_eq!(o.cwd.as_deref(), Some("/tmp/acp4/proj"));
    assert_eq!(o.project.as_deref(), Some("acp4"));
    assert_eq!(o.title, "Create Empty File");
    assert!(o.can_send);
    let hidden: HashSet<String> = [t.pending_runtime_request.clone().unwrap().id].into();
    assert_eq!(
        agent_obs(&t, Some(&project), &hidden).state,
        AgentState::Working
    );
    let codex = shell_thread("shell_chunk_thread_updated_starting_codex.json");
    assert_eq!(
        agent_obs(&codex, None, &HashSet::new()).kind,
        AgentKind::Codex
    );
}

#[test]
fn hostile_text_is_cleaned() {
    let mut t = shell_thread("shell_chunk_thread_updated_running.json");
    t.title = Some(format!(
        "\u{1b}]0;evil\u{7}fix\u{202E}tests{}",
        "x".repeat(300)
    ));
    t.latest_visible_message =
        Some(serde_json::json!({"text": format!("done\u{1b}[31m{}", "y".repeat(600))}));
    let o = agent_obs(&t, None, &HashSet::new());
    assert!(!o.title.contains('\u{1b}') && !o.title.contains('\u{202E}'));
    assert_eq!(o.title.chars().count(), 200);
    let m = o.last_message.unwrap();
    assert!(!m.contains('\u{1b}'));
    assert_eq!(m.chars().count(), 500);
}

#[test]
fn claude_approval_joins_the_command() {
    let p = projection("thread_chunk_snapshot_waiting_approval_claude.json");
    let (d, skipped) = ThreadDetail::from_projection(&p);
    assert_eq!(skipped, 0);
    assert_eq!(
        d.native_session_id(),
        Some("d67b7431-38fa-47ca-b40d-11d54f568263")
    );
    let id = pending_id(&p);
    let view = d.pending_view(&id).unwrap();
    let (obs, target) = request_obs("t3:x", "x", &view);
    assert_eq!(obs.key, CorrelationKey(id.clone()));
    assert!(!obs.reusable_key && obs.ready);
    assert_eq!(
        (obs.kind, obs.answer_path),
        (RequestKind::Permission, AnswerPath::T3)
    );
    assert_eq!(
        obs.detail,
        "touch /tmp/acp4/proj/x.txt\nCreate empty file x.txt"
    );
    let opts: Vec<(&str, Effect)> = obs
        .options
        .iter()
        .map(|o| (o.id.as_str(), o.effect))
        .collect();
    assert_eq!(
        opts,
        [
            ("accept", Effect::Allow),
            ("acceptForSession", Effect::AllowAlways),
            ("decline", Effect::Deny)
        ]
    );
    assert_eq!(target.answer, TargetAnswer::Decision);
    assert_eq!(target.t3_request_id, id);
}

#[test]
fn codex_approval_detail_is_the_command_once() {
    let p = projection("thread_chunk_snapshot_waiting_approval_codex.json");
    let (d, _) = ThreadDetail::from_projection(&p);
    let (obs, _) = request_obs("t3:y", "y", &d.pending_view(&pending_id(&p)).unwrap());
    assert!(!obs.detail.is_empty());
    assert_eq!(
        obs.detail.lines().count(),
        1,
        "prompt equals the command: {}",
        obs.detail
    );
}

#[test]
fn claude_question_keeps_its_structure() {
    let p = projection("thread_chunk_snapshot_waiting_question_claude.json");
    let (d, _) = ThreadDetail::from_projection(&p);
    let (obs, target) = request_obs("t3:x", "x", &d.pending_view(&pending_id(&p)).unwrap());
    assert_eq!(obs.kind, RequestKind::Question);
    let q = &obs.questions.as_ref().unwrap()[0];
    assert_eq!(
        (q.id.as_str(), q.header.as_deref(), q.multi_select),
        ("Which color do you prefer?", Some("Color"), false)
    );
    assert_eq!(
        obs.options
            .iter()
            .map(|o| (o.id.as_str(), o.label.as_str(), o.effect))
            .collect::<Vec<_>>(),
        [
            ("o1", "red", Effect::Choose),
            ("o2", "blue", Effect::Choose)
        ]
    );
    assert!(!obs.free_text);
    let TargetAnswer::Question {
        question_id,
        choices,
    } = &target.answer
    else {
        panic!()
    };
    assert_eq!(question_id, "Which color do you prefer?");
    assert_eq!(choices[1].value, "blue");
}

#[test]
fn unknown_or_unmatched_requests_are_read_only() {
    let p = projection("thread_chunk_snapshot_waiting_approval_claude.json");
    let (mut d, _) = ThreadDetail::from_projection(&p);
    let id = pending_id(&p);
    let mut view = d.pending_view(&id).unwrap();
    let TurnItem::ApprovalRequest { request_kind, .. } = view.item.as_mut().unwrap() else {
        panic!()
    };
    *request_kind = Some("file_change".into());
    let (obs, target) = request_obs("t3:x", "x", &view);
    assert_eq!(obs.answer_path, AnswerPath::None);
    assert_eq!(target.answer, TargetAnswer::ReadOnly);
    view.item = None;
    let (obs, _) = request_obs("t3:x", "x", &view);
    assert_eq!(obs.answer_path, AnswerPath::None);
    assert!(obs.detail.contains("T3"));
    // A resolved request has no pending view.
    d.requests.get_mut(&id).unwrap().status = "resolved".into();
    assert!(d.pending_view(&id).is_none());
}

#[test]
fn events_update_the_detail() {
    let p = projection("thread_chunk_snapshot_waiting_approval_claude.json");
    let (mut d, _) = ThreadDetail::from_projection(&p);
    let id = pending_id(&p);
    for v in chunk_values("thread_event_runtime_request_resolved_accept.json") {
        let ThreadItem::Event { event, .. } = ThreadItem::decode(&v).unwrap() else {
            panic!()
        };
        d.apply(&event).unwrap();
    }
    assert_eq!(d.request_status(&id), Some("resolved"));
    assert!(d.pending_view(&id).is_none());

    let mut fresh = ThreadDetail::default();
    for v in
        chunk_values("thread_event_provider_thread_updated_codex_native_id_null_before_start.json")
    {
        let ThreadItem::Event { event, .. } = ThreadItem::decode(&v).unwrap() else {
            panic!()
        };
        fresh.apply(&event).unwrap();
    }
    assert_eq!(
        fresh.native_session_id(),
        None,
        "null before the first turn"
    );
    for v in chunk_values("thread_event_provider_thread_updated_claude_native_id.json") {
        let ThreadItem::Event { event, .. } = ThreadItem::decode(&v).unwrap() else {
            panic!()
        };
        fresh.apply(&event).unwrap();
    }
    // The active provider thread is still null (the codex metadata event set it so).
    assert_eq!(
        fresh.native_session_id(),
        None,
        "only the active provider thread counts"
    );
    fresh.active_provider_thread = Some(
        "provider-thread:provider:claudeAgent:native-thread:pending%3Arun%3Athread%3Ad613fd68-3cc5-4231-a36d-2e6f9998e7c0%3Aordinal%3A1".into());
    assert_eq!(
        fresh.native_session_id(),
        Some("d67b7431-38fa-47ca-b40d-11d54f568263")
    );
}

#[test]
fn restart_snapshot_shows_the_request_cancelled() {
    let p = projection("thread_chunk_snapshot_after_restart_not_resumable.json");
    let (d, _) = ThreadDetail::from_projection(&p);
    let cancelled = p["runtimeRequests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["status"] == "cancelled")
        .unwrap();
    assert_eq!(
        d.request_status(cancelled["id"].as_str().unwrap()),
        Some("cancelled")
    );
}

#[test]
fn prune_keeps_only_live_state() {
    let p = projection("thread_chunk_snapshot_waiting_approval_claude.json");
    let (mut d, _) = ThreadDetail::from_projection(&p);
    let id = pending_id(&p);
    for i in 0..500 {
        for (status, n) in [("pending", 0), ("resolved", 1)] {
            let ev = serde_json::json!({"kind": "event", "sequence": 1000 + 2 * i + n,
                "event": {"type": "runtime-request.updated", "payload": {"id": format!("old-{i}"), "kind": "command", "status": status}}});
            let ThreadItem::Event { event, .. } = ThreadItem::decode(&ev).unwrap() else {
                panic!()
            };
            d.apply(&event).unwrap();
        }
        let ev = serde_json::json!({"kind": "event", "sequence": 5000 + i,
            "event": {"type": "turn-item.updated", "payload": {"id": format!("cmd-{i}"), "type": "command_execution", "status": "completed", "input": "ls"}}});
        let ThreadItem::Event { event, .. } = ThreadItem::decode(&ev).unwrap() else {
            panic!()
        };
        d.apply(&event).unwrap();
    }
    d.prune(&[&id]);
    let (requests, items) = d.retained();
    assert_eq!(requests, 1);
    assert_eq!(items, 2, "the approval and its command");
    // The live approval still joins its command after pruning.
    let (obs, _) = request_obs("t3:x", "x", &d.pending_view(&id).unwrap());
    assert!(obs.detail.starts_with("touch /tmp/acp4/proj/x.txt"));
}

#[test]
fn a_live_approvals_command_survives_the_cap() {
    // Sol round 2, finding 4: more than 64 non-terminal commands arrive
    // after the live approval. The cap must not take its command.
    let p = projection("thread_chunk_snapshot_waiting_approval_claude.json");
    let (mut d, _) = ThreadDetail::from_projection(&p);
    let id = pending_id(&p);
    for i in 0..(MAX_COMMAND_ITEMS + 36) {
        let ev = serde_json::json!({"kind": "event", "sequence": 9000 + i,
            "event": {"type": "turn-item.updated", "payload": {"id": format!("run-{i}"), "type": "command_execution",
                "status": "running", "input": format!("sleep {i}"), "nativeItemRef": {"nativeId": format!("n-{i}")}}}});
        let ThreadItem::Event { event, .. } = ThreadItem::decode(&ev).unwrap() else {
            panic!()
        };
        d.apply(&event).unwrap();
        d.prune(&[&id]);
    }
    let (obs, _) = request_obs("t3:x", "x", &d.pending_view(&id).unwrap());
    assert_eq!(
        obs.detail,
        "touch /tmp/acp4/proj/x.txt\nCreate empty file x.txt"
    );
    let (_, items) = d.retained();
    assert!(
        items <= MAX_COMMAND_ITEMS + 2,
        "64 unreferenced + the approval + its command: {items}"
    );
}

fn request_for(obs: &acd_core::RequestObs) -> Request {
    Request {
        id: "r1".into(),
        agent_id: obs.agent_id.clone(),
        kind: obs.kind,
        status: RequestStatus::Answering,
        answer_path: obs.answer_path,
        title: obs.title.clone(),
        detail: obs.detail.clone(),
        options: obs.options.clone(),
        questions: obs.questions.clone(),
        free_text: false,
        raw_screen: None,
        opened_at: chrono::Utc::now(),
    }
}

#[test]
fn respond_payloads_match_the_recordings() {
    let p = projection("thread_chunk_snapshot_waiting_approval_claude.json");
    let (d, _) = ThreadDetail::from_projection(&p);
    let (obs, target) = request_for_target(&d, &p);
    let want: Value =
        serde_json::from_str(&fx("cmd_request_respond_decision_accept.json")).unwrap();
    let cmd = want["payload"]["commandId"].as_str().unwrap();
    let mut target = target;
    target.thread_id = want["payload"]["threadId"].as_str().unwrap().into();
    let got = respond_payload(
        cmd,
        &target,
        &request_for(&obs),
        &Answer::Option("accept".into()),
    )
    .unwrap();
    assert_eq!(got, want["payload"]);
    assert!(
        respond_payload(
            cmd,
            &target,
            &request_for(&obs),
            &Answer::Option("allow".into())
        )
        .is_err()
    );

    let p = projection("thread_chunk_snapshot_waiting_question_claude.json");
    let (d, _) = ThreadDetail::from_projection(&p);
    let (obs, mut target) = request_for_target(&d, &p);
    let want: Value = serde_json::from_str(&fx("cmd_request_respond_answers.json")).unwrap();
    target.thread_id = want["payload"]["threadId"].as_str().unwrap().into();
    let cmd = want["payload"]["commandId"].as_str().unwrap();
    let got = respond_payload(
        cmd,
        &target,
        &request_for(&obs),
        &Answer::Option("o2".into()),
    )
    .unwrap();
    assert_eq!(got, want["payload"]);
    // The frozen label must match the recorded choice.
    let mut req = request_for(&obs);
    req.options[1].label = "green".into();
    assert!(respond_payload(cmd, &target, &req, &Answer::Option("o2".into())).is_err());
}

fn request_for_target(d: &ThreadDetail, p: &Value) -> (acd_core::RequestObs, Target) {
    request_obs("t3:x", "x", &d.pending_view(&pending_id(p)).unwrap())
}

#[test]
fn dispatch_payload_matches_the_recording() {
    let want: Value =
        serde_json::from_str(&fx("cmd_request_message_dispatch_followup_auto.json")).unwrap();
    let w = &want["payload"];
    let got = dispatch_payload(
        w["commandId"].as_str().unwrap(),
        w["messageId"].as_str().unwrap(),
        w["threadId"].as_str().unwrap(),
        w["text"].as_str().unwrap(),
    );
    assert_eq!(&got, w);
}

fn apply_json(d: &mut ThreadDetail, ev: Value) {
    let ThreadItem::Event { event, .. } = ThreadItem::decode(&ev).unwrap() else {
        panic!()
    };
    d.apply(&event).unwrap();
}

fn without_commands(p: &Value) -> Value {
    let mut p = p.clone();
    p["turnItems"]
        .as_array_mut()
        .unwrap()
        .retain(|i| i["type"] != "command_execution");
    p
}

#[test]
fn approval_without_its_command_is_read_only() {
    let p = projection("thread_chunk_snapshot_waiting_approval_claude.json");
    let id = pending_id(&p);
    let (d, _) = ThreadDetail::from_projection(&without_commands(&p));
    let view = d.pending_view(&id).unwrap();
    assert!(view.command.is_none());
    let (obs, target) = request_obs("t3:x", "x", &view);
    assert_eq!(obs.answer_path, AnswerPath::None);
    assert!(obs.options.is_empty());
    assert_eq!(target.answer, TargetAnswer::ReadOnly);
    assert_eq!(obs.title, "Run a command");
    assert!(
        obs.detail.starts_with("Command not available yet"),
        "{}",
        obs.detail
    );
    assert!(
        obs.detail.contains("Description: Create empty file x.txt"),
        "{}",
        obs.detail
    );
    assert_ne!(obs.detail, "Create empty file x.txt");
    // With the command item it is answerable and leads with the command.
    let (d, _) = ThreadDetail::from_projection(&p);
    let (obs, target) = request_obs("t3:x", "x", &d.pending_view(&id).unwrap());
    assert_eq!(obs.answer_path, AnswerPath::T3);
    assert_eq!(target.answer, TargetAnswer::Decision);
    assert!(obs.detail.starts_with("touch /tmp/acp4/proj/x.txt"));
    assert_eq!(target.answer, TargetAnswer::Decision);
    // A blank command counts as absent.
    let mut view = d.pending_view(&id).unwrap();
    view.command = Some("  ".into());
    assert_eq!(
        request_obs("t3:x", "x", &view).0.answer_path,
        AnswerPath::None
    );
}

#[test]
fn only_one_single_select_question_is_answerable() {
    let p = projection("thread_chunk_snapshot_waiting_question_claude.json");
    let (d, _) = ThreadDetail::from_projection(&p);
    let view = d.pending_view(&pending_id(&p)).unwrap();
    let Some(TurnItem::UserInputRequest { questions, .. }) = &view.item else {
        panic!()
    };
    let one = questions[0].clone();

    let mut two = view.clone();
    let Some(TurnItem::UserInputRequest { questions, .. }) = two.item.as_mut() else {
        panic!()
    };
    questions.push(one.clone());
    let (obs, target) = request_obs("t3:x", "x", &two);
    assert_eq!(obs.answer_path, AnswerPath::None);
    assert_eq!(target.answer, TargetAnswer::ReadOnly);
    assert_eq!(obs.questions.as_ref().unwrap().len(), 2);

    let mut multi = view.clone();
    let Some(TurnItem::UserInputRequest { questions, .. }) = multi.item.as_mut() else {
        panic!()
    };
    questions[0].multi_select = true;
    let (obs, target) = request_obs("t3:x", "x", &multi);
    assert_eq!(obs.answer_path, AnswerPath::None);
    assert_eq!(target.answer, TargetAnswer::ReadOnly);
    assert!(obs.questions.as_ref().unwrap()[0].multi_select);
}

#[test]
fn prune_keeps_a_referenced_terminal_command() {
    let p = projection("thread_chunk_snapshot_waiting_approval_claude.json");
    let id = pending_id(&p);
    let mut p = p.clone();
    for i in p["turnItems"].as_array_mut().unwrap() {
        if i["type"] == "command_execution" {
            i["status"] = "completed".into();
        }
    }
    let (mut d, _) = ThreadDetail::from_projection(&p);
    d.prune(&[&id]);
    let view = d.pending_view(&id).unwrap();
    assert_eq!(
        view.command.as_deref().map(str::trim),
        Some("touch /tmp/acp4/proj/x.txt")
    );
}

#[test]
fn the_cap_drops_the_oldest_unreferenced_commands_first() {
    let p = projection("thread_chunk_snapshot_waiting_approval_claude.json");
    let id = pending_id(&p);
    let (mut d, _) = ThreadDetail::from_projection(&p);
    let n = MAX_COMMAND_ITEMS + 6;
    for i in 0..n {
        apply_json(
            &mut d,
            serde_json::json!({"kind": "event", "sequence": 9000 + i,
            "event": {"type": "turn-item.updated", "payload": {"id": format!("run-{i}"), "type": "command_execution",
                "status": "running", "input": format!("sleep {i}"), "nativeItemRef": {"nativeId": format!("n-{i}")}}}}),
        );
        d.prune(&[&id]);
    }
    for (name, native) in [("oldest", 0), ("newest", n - 1)] {
        apply_json(
            &mut d,
            serde_json::json!({"kind": "event", "sequence": 20000 + native,
            "event": {"type": "runtime-request.updated", "payload": {"id": format!("late-{name}"), "kind": "command", "status": "pending"}}}),
        );
        apply_json(
            &mut d,
            serde_json::json!({"kind": "event", "sequence": 30000 + native,
            "event": {"type": "turn-item.updated", "payload": {"id": format!("appr-{name}"), "type": "approval_request",
                "requestId": format!("late-{name}"), "requestKind": "command", "prompt": "p",
                "nativeItemRef": {"nativeId": format!("n-{native}")}}}}),
        );
    }
    assert_eq!(d.pending_view("late-oldest").unwrap().command, None);
    assert_eq!(
        d.pending_view("late-newest").unwrap().command,
        Some(format!("sleep {}", n - 1))
    );
}

#[test]
fn a_waiting_postturn_thread_without_a_request_is_working() {
    let t = shell_thread("shell_chunk_thread_updated_waiting_postturn_no_request.json");
    assert!(t.pending_runtime_request.is_none());
    assert_eq!(
        agent_obs(&t, None, &HashSet::new()).state,
        AgentState::Working
    );
}

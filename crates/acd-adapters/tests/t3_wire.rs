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
    let f = parse_frames(text).unwrap();
    assert_eq!(f.len(), 4);
    assert!(matches!(&f[0], ServerFrame::Chunk { request_id, .. } if request_id == "1"));
    assert!(matches!(f[1], ServerFrame::Other));
    assert!(matches!(f[2], ServerFrame::Other));
    assert!(matches!(f[3], ServerFrame::Exit { .. }));
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

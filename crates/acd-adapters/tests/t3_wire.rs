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
        for f in frames {
            if let ServerFrame::Chunk { values, .. } = f {
                for v in values {
                    if name.starts_with("shell_") || name.starts_with("rpc_chunk") {
                        ShellItem::decode(&v).unwrap_or_else(|e| panic!("{name}: {e}"));
                    } else {
                        ThreadItem::decode(&v).unwrap_or_else(|e| panic!("{name}: {e}"));
                    }
                }
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
    assert_eq!(
        snapshot.projects[0].workspace_root.as_deref(),
        Some("/tmp/acp4/proj")
    );

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

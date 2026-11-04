//! T3 Code websocket frames and their mapping (t3/wire.rs, detail.rs, map.rs).
//! The input is one JSON document that is tried as a frame list, a shell
//! item, a thread item, a shell thread and a thread projection (its
//! `events` array is applied to it). Invariants for every pending request:
//! - an answerable command shows the command (and the description when it
//!   differs) whole: the detail decodes back to exactly that text;
//! - a read-only request offers no option and no target answer;
//! - every option offered is accepted by `respond_payload`, with the value
//!   the user saw, and a typed answer only where free text is offered.
#![no_main]
use acd_adapters::t3::detail::ThreadDetail;
use acd_adapters::t3::map::{DECISIONS, DETAIL_CHARS, TargetAnswer, agent_obs, request_obs, respond_payload};
use acd_adapters::t3::wire::{
    Project, ShellItem, ShellThread, ThreadEvent, ThreadItem, TurnItem, is_project_only, item_sequence, parse_frames,
};
use acd_core::{AgentObs, Answer, RequestObs};
use acd_protocol::{AnswerPath, Request};
use fuzzlib::{hidden_or_forbidden, unescape_raw};
use libfuzzer_sys::fuzz_target;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;

fn as_request(obs: &RequestObs) -> Request {
    serde_json::from_value(json!({
        "id": "r", "revision": 1, "agent_id": obs.agent_id, "kind": obs.kind, "status": "open",
        "answer_path": obs.answer_path, "title": obs.title, "detail": obs.detail, "options": obs.options,
        "questions": obs.questions, "free_text": obs.free_text, "raw_screen": obs.raw_screen,
        "opened_at": "2026-10-08T00:00:00Z",
    }))
    .expect("an observation is a valid request")
}

fn check(provider: Option<&str>, agent: Option<&AgentObs>, view: &acd_adapters::t3::detail::PendingView) {
    let (obs, target) = request_obs("a", "t", provider, view);
    assert!(!obs.title.chars().any(char::is_control), "{:?}", obs.title);
    fuzzlib::assert_agent_fits(agent, Some(&obs));
    let req = as_request(&obs);
    let answerable = obs.answer_path != AnswerPath::None;
    assert_eq!(answerable, !matches!(target.answer, TargetAnswer::ReadOnly), "path and target disagree");
    if !answerable {
        assert!(obs.options.is_empty() || obs.questions.is_some(), "a read-only command offers options");
        assert!(respond_payload("c", &target, &req, &Answer::Option("accept".into())).is_err());
        assert!(respond_payload("c", &target, &req, &Answer::Text("x".into())).is_err());
        return;
    }
    match &target.answer {
        TargetAnswer::Decision => {
            // The text that would run, as request_obs joins it.
            let Some(TurnItem::ApprovalRequest { prompt, .. }) = &view.item else { panic!("decision without approval") };
            let c = view.command.as_deref().filter(|c| !c.trim().is_empty()).expect("decision without command");
            let prompt = prompt.as_deref().unwrap_or("").trim();
            let text = if !prompt.is_empty() && c != prompt { format!("{c}\n{prompt}") } else { c.to_owned() };
            assert!(obs.detail.chars().count() <= DETAIL_CHARS);
            assert!(!obs.detail.chars().any(hidden_or_forbidden), "raw hidden character in {:?}", obs.detail);
            assert_eq!(unescape_raw(&obs.detail).as_deref(), Some(text.as_str()), "command shown cut or altered");
            for o in &obs.options {
                assert!(DECISIONS.contains(&o.id.as_str()));
                let p = respond_payload("c", &target, &req, &Answer::Option(o.id.clone())).expect("offered option refused");
                assert_eq!(p["decision"], o.id.as_str());
            }
            assert!(respond_payload("c", &target, &req, &Answer::Option("bogus".into())).is_err());
            assert!(respond_payload("c", &target, &req, &Answer::Text("x".into())).is_err());
        }
        TargetAnswer::Question { question_id, choices, free_text } => {
            let qs = obs.questions.as_ref().expect("answerable question has questions");
            assert_eq!(qs.len(), 1);
            assert!(!qs[0].multi_select);
            // Whole or read-only: nothing shown was cut or altered.
            let Some(TurnItem::UserInputRequest { questions, .. }) = &view.item else { panic!("question without request") };
            let q = &questions[0];
            assert_eq!(qs[0].text, q.question.trim(), "question text cut or altered");
            assert_eq!(qs[0].header.as_deref(), q.header.as_deref().map(str::trim), "header cut or altered");
            assert_eq!(qs[0].options.len(), q.options.len(), "options dropped");
            for (so, o) in qs[0].options.iter().zip(&q.options) {
                assert_eq!(so.label, o.label.trim(), "label cut or altered");
                assert_eq!(so.description.as_deref(), o.description.as_deref().map(str::trim), "description cut or altered");
            }
            assert_eq!(choices.len(), obs.options.len());
            assert_eq!(obs.free_text, *free_text);
            assert!(!choices.is_empty() || *free_text, "answerable with nothing to answer");
            for (o, ch) in obs.options.iter().zip(choices) {
                assert_eq!((&o.id, &o.label), (&ch.option_id, &ch.label));
                let p = respond_payload("c", &target, &req, &Answer::Option(o.id.clone())).expect("offered option refused");
                assert_eq!(p["answers"][question_id.as_str()], ch.value.as_str());
            }
            let typed = respond_payload("c", &target, &req, &Answer::Text("hello".into()));
            assert_eq!(typed.is_ok(), *free_text);
        }
        TargetAnswer::ReadOnly => unreachable!(),
    }
}

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = parse_frames(s);
    }
    let Ok(v) = serde_json::from_slice::<Value>(data) else { return };
    let _ = ShellItem::decode(&v);
    let _ = ThreadItem::decode(&v);
    let _ = (is_project_only(&v), item_sequence(&v));
    let mut shell_agent = None;
    if let Ok(t) = ShellThread::decode(&v) {
        let project = v.get("project").and_then(|p| Project::decode(p).ok());
        let hidden: HashSet<String> = t.pending_runtime_request.iter().map(|p| p.id.clone()).take(1).collect();
        for h in [&HashSet::new(), &hidden] {
            let o = agent_obs(&t, project.as_ref(), h);
            assert!(!o.title.chars().any(char::is_control));
            fuzzlib::assert_agent_fits(Some(&o), None);
            shell_agent = Some(o);
        }
    }
    if !ThreadDetail::valid_projection(&v) {
        return;
    }
    let (mut d, _) = ThreadDetail::from_projection(&v);
    for ev in v.get("events").and_then(Value::as_array).into_iter().flatten() {
        if let Ok(ev) = ThreadEvent::deserialize(ev) {
            let _ = d.apply(&ev);
        }
    }
    let provider = v.get("provider").and_then(Value::as_str);
    let ids: Vec<String> = d.requests.keys().cloned().collect();
    for id in &ids {
        if let Some(view) = d.pending_view(id) {
            check(provider, shell_agent.as_ref(), &view);
        }
    }
    d.prune(&[]);
    let _ = d.retained();
});

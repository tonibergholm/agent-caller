//! The Omarchy inbox socket protocol (acd-inbox/src/protocol.rs and the
//! line reader in server.rs). Views are built from arbitrary cards and
//! encoded under an arbitrary line limit; raw bytes go through `read_line`.
//! Invariants: a line is one JSON object under the limit with no newline;
//! Needs-you cards are never left out; a card the line shows answerable
//! shows exactly the request it was given (whole-or-read-only); the omitted
//! counts add up; `read_line` never returns a line over its limit and never
//! loses or invents bytes.
#![no_main]
use acd_client::view::{
    CardGroup, CardView, InboxView, MachineEntryView, Notice, NoticeTone, PairingState, TrayColor, TraySummary,
};
use acd_client::settings::{EndpointConfig, SecretStatus};
use acd_inbox::protocol::{
    ErrorCode, SettingsSnapshot, error_line, ok_line, outcomes_lost_line, settings_line, view_line, view_or_overflow_line,
    view_overflow_line,
};
use acd_inbox::server::{LineError, read_line};
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use serde_json::{Value, json};

#[derive(Arbitrary, Debug)]
struct Card {
    text: String,
    title: String,
    label: String,
    raw: Option<String>,
    group: u8,
    answerable: bool,
    free_text: bool,
    has_pending: bool,
    question: bool,
    notice: Option<String>,
}

#[derive(Arbitrary, Debug)]
struct Input {
    cards: Vec<Card>,
    max: u16,
    big: u8,
    names: Vec<String>,
    stream: Vec<u8>,
    cap: u8,
    chunk: u8,
}

fn card(i: usize, c: &Card, big: usize) -> CardView {
    let group = match c.group % 5 {
        0 => CardGroup::Request,
        1 => CardGroup::Done,
        2 => CardGroup::Working,
        3 => CardGroup::Unknown,
        _ => CardGroup::Idle,
    };
    let state = if group == CardGroup::Request { "needs_you" } else { "done" };
    let text = c.text.repeat(1 + big);
    let pending = c.has_pending.then(|| {
        json!({
            "id": format!("r{i}"), "revision": 1, "agent_id": format!("a{i}"),
            "kind": if c.question { "question" } else { "permission" }, "status": "open",
            "answer_path": if c.question { "keys" } else { "hook" }, "title": c.title, "detail": text,
            "options": [{"id": "o1", "label": c.label, "description": null, "effect": "allow"}],
            "questions": if c.question { json!([{"id": "q", "header": null, "text": text, "multi_select": false,
                "options": [{"id": "o1", "label": c.label, "description": null, "effect": "choose"}]}]) } else { Value::Null },
            "free_text": c.free_text, "raw_screen": c.raw, "opened_at": "2026-10-08T00:00:00Z",
        })
    });
    let agent = serde_json::from_value(json!({
        "id": format!("a{i}"), "machine_id": "m", "source": "herdr", "kind": "claude", "title": c.title, "cwd": null,
        "project": null, "state": state, "state_since": "2026-10-08T00:00:00Z", "failed": false,
        "last_message": text, "can_send": true, "pending": pending,
    }))
    .expect("a valid agent");
    CardView {
        key: format!("c{i}"),
        machine_key: "m1".into(),
        machine_name: "Box".into(),
        agent,
        group,
        live: true,
        offline_since_ms: None,
        answerable: c.answerable && c.has_pending,
        free_text_answerable: c.free_text && c.has_pending,
        selectable_options: if c.answerable && c.has_pending { vec!["o1".into()] } else { vec![] },
        can_reply: false,
        can_mark_seen: false,
        busy: false,
        notice: c.notice.clone().map(|text| Notice { tone: NoticeTone::Info, text }),
        action: None,
    }
}

fn shown(agent: &Value) -> Value {
    let mut p = agent.get("pending").cloned().unwrap_or(Value::Null);
    if let Some(m) = p.as_object_mut() {
        m.remove("raw_screen");
    }
    p
}

fn check_lines(lines: &[String]) {
    for l in lines {
        assert!(!l.contains('\n'), "a line holds a newline");
        let v: Value = serde_json::from_str(l).expect("a line is JSON");
        assert!(v.is_object());
        assert!(v["type"].is_string());
    }
}

fuzz_target!(|data: &[u8]| {
    let mut u = arbitrary::Unstructured::new(data);
    let Ok(i) = Input::arbitrary(&mut u) else { return };
    let max = i.max as usize * 2;

    // Views.
    let cards: Vec<CardView> = i.cards.iter().take(8).enumerate().map(|(n, c)| card(n, c, (i.big % 12) as usize)).collect();
    let needs_you = cards.iter().filter(|c| c.group == CardGroup::Request).count();
    let view = InboxView {
        machines: vec![],
        cards: cards.clone(),
        hidden_idle: 0,
        show_idle: false,
        show_subagents: false,
        show_hook_only: false,
        tray: TraySummary { color: TrayColor::Amber, needs_you: needs_you as u32 },
        actions: vec![],
    };
    let line = view_line(&view, max);
    let or_overflow = view_or_overflow_line(&view, max);
    check_lines(&[or_overflow.clone(), view_overflow_line(needs_you), outcomes_lost_line()]);
    match &line {
        None => {
            let v: Value = serde_json::from_str(&or_overflow).unwrap();
            assert_eq!(v["type"], "view_overflow");
            assert_eq!(v["needs_you"], needs_you);
        }
        Some(l) => {
            assert!(l.len() < max, "{} bytes for a limit of {max}", l.len());
            assert_eq!(&or_overflow, l);
            check_lines(std::slice::from_ref(l));
            let v: Value = serde_json::from_str(l).unwrap();
            let out = v["view"]["cards"].as_array().unwrap();
            let omitted = v["omitted"]["done"].as_u64().unwrap() + v["omitted"]["working"].as_u64().unwrap();
            assert_eq!(out.len() as u64 + omitted, cards.len() as u64, "cards unaccounted for");
            let kept: Vec<&str> = out.iter().map(|c| c["key"].as_str().unwrap()).collect();
            for c in cards.iter().filter(|c| c.group == CardGroup::Request) {
                assert!(kept.contains(&c.key.as_str()), "a Needs-you card was left out");
            }
            for oc in out {
                let key = oc["key"].as_str().unwrap();
                let input = cards.iter().find(|c| c.key == key).expect("a card from nowhere");
                let input_agent = serde_json::to_value(&input.agent).unwrap();
                let whole = shown(&oc["agent"]) == shown(&input_agent);
                if oc["answerable"] == true || oc["free_text_answerable"] == true || !oc["selectable_options"].as_array().unwrap().is_empty() {
                    assert!(whole, "card {key} is answerable but its request was cut: {oc}");
                    assert!(input.answerable || input.free_text_answerable, "card {key} became answerable");
                }
                if !whole {
                    assert_eq!(oc["agent"]["pending"]["answer_path"], "none", "a cut request keeps its answer path");
                }
            }
        }
    }

    // Settings.
    let machines: Vec<MachineEntryView> = i
        .names
        .iter()
        .take(40)
        .enumerate()
        .map(|(n, name)| MachineEntryView {
            key: format!("k{n}"),
            name: name.clone(),
            endpoint: if n % 2 == 0 { EndpointConfig::Local } else { EndpointConfig::Tcp { host: name.clone(), port: 1 } },
            pairing: PairingState::Paired,
            secret: SecretStatus::Ok,
        })
        .collect();
    let total = machines.len();
    let snap = SettingsSnapshot { machines, notifications: true, show_idle: false, show_subagents: false, show_hook_only: false };
    let sl = settings_line(&snap, max);
    check_lines(std::slice::from_ref(&sl));
    let v: Value = serde_json::from_str(&sl).unwrap();
    let kept = v["machines"].as_array().unwrap().len();
    let omitted = v["omitted_machines"].as_u64().unwrap_or(0) as usize;
    assert_eq!(kept + omitted, total, "machines unaccounted for");
    assert!(sl.len() < max || kept == 0, "settings line over the limit with machines left");
    check_lines(&[ok_line("i", json!({"x": i.names.first()})), error_line("i", ErrorCode::BadRequest)]);

    // The line reader: lines come back whole, none over the limit, and the
    // bytes read are a prefix of the input, whatever the chunking.
    let cap = (i.cap as usize % 64) + 1;
    let chunk = (i.chunk as usize % 16) + 1;
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    rt.block_on(async {
        let mut reader = tokio::io::BufReader::with_capacity(chunk, &i.stream[..]);
        let mut buf = Vec::new();
        let mut joined: Vec<u8> = Vec::new();
        loop {
            match read_line(&mut reader, &mut buf, cap).await {
                Ok(true) => {
                    assert!(buf.len() <= cap);
                    assert!(!buf.contains(&b'\n'));
                    joined.extend_from_slice(&buf);
                    joined.push(b'\n');
                }
                Ok(false) => break,
                Err(LineError::TooLong) => break,
                Err(LineError::Io) => panic!("an in-memory read failed"),
            }
        }
        assert!(i.stream.starts_with(&joined), "bytes lost or invented");
    });
});

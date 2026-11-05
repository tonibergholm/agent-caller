//! `AskUserQuestion` hook payloads built with random lengths and contents
//! around every cap (hooks/mapping.rs `claude_question`), then published by a
//! registry. Whole-or-read-only: a question the card offers to answer by key
//! is never cut or altered.
//! Invariants: an answerable question's text, header, labels and descriptions
//! are exactly the payload's (trimmed), within their caps, with no control
//! character; the same holds for what the registry publishes (it either keeps
//! the question as it is or makes the request read-only with no options);
//! the other way round, a plain single-select question with 1 to 9 distinct
//! options inside every cap is answerable (a valid question is not left
//! read-only for no reason).
#![no_main]
use acd_adapters::hooks::mapping::{DETAIL_CHARS, claude_question, question_specs};
use acd_adapters::hooks::wire::Payload;
use acd_adapters::text::TITLE_CHARS;
use acd_core::{AgentObs, CorrelationKey, Observation, Registry};
use acd_protocol::{AgentKind, AgentRole, AgentState, AnswerPath, Source};
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use serde_json::{Value, json};

const DESCRIPTION_CHARS: usize = 400;
const BOUNDS: [usize; 14] = [0, 1, 2, 9, 199, 200, 201, 399, 400, 401, 3999, 4000, 4001, 4500];

#[derive(Arbitrary, Debug)]
struct Field {
    len: u16,
    snap: bool,
    class: u8,
    /// Where to put a character that must not be shown, and which.
    ctrl: Option<(u16, u8)>,
    pad: u8,
}

#[derive(Arbitrary, Debug)]
struct Q {
    text: Field,
    header: Field,
    multi: bool,
    options: Vec<(Field, Option<Field>)>,
}

#[derive(Arbitrary, Debug)]
struct Input {
    questions: Vec<Q>,
    bound: bool,
    cwd: u16,
}

/// The text and whether it is "plain": nothing a cleaner would touch.
fn build(f: &Field, distinct: Option<usize>) -> (String, bool) {
    let n = if f.snap { BOUNDS[f.len as usize % BOUNDS.len()] } else { f.len as usize % 4500 };
    let unit = ["a", "é", "日", "😀", "b c"][f.class as usize % 5];
    let mut s: String = unit.chars().cycle().take(n).collect();
    if let (Some(i), true) = (distinct, n > 0) {
        s.replace_range(0..s.chars().next().unwrap().len_utf8(), &(i % 10).to_string());
    }
    let mut plain = true;
    if let Some((at, c)) = f.ctrl {
        let ch = ['\u{7}', '\u{200b}', '\t', '\u{202e}', '\n', '\u{feff}', '\u{85}'][c as usize % 7];
        let idx = s.char_indices().nth(at as usize % (n + 1)).map_or(s.len(), |(i, _)| i);
        s.insert(idx, ch);
        plain = false;
    }
    if f.pad % 4 == 1 {
        s.insert(0, ' ');
        plain = false;
    }
    if f.pad % 4 == 2 {
        s.push(' ');
        plain = false;
    }
    (s, plain)
}

fuzz_target!(|i: Input| {
    let mut all_plain = true;
    let mut within_caps = true;
    let mut qs = vec![];
    for q in i.questions.iter().take(5) {
        let (text, p1) = build(&q.text, None);
        let (header, p2) = build(&q.header, None);
        all_plain &= p1 && p2;
        within_caps &= !text.is_empty() && text.trim().chars().count() <= DETAIL_CHARS && header.trim().chars().count() <= TITLE_CHARS;
        let mut opts = vec![];
        for (k, (l, d)) in q.options.iter().take(11).enumerate() {
            let (label, p3) = build(l, Some(k));
            all_plain &= p3;
            within_caps &= !label.is_empty() && label.trim().chars().count() <= TITLE_CHARS;
            let mut o = json!({"label": label});
            if let Some(d) = d {
                let (d, p4) = build(d, None);
                all_plain &= p4;
                within_caps &= d.trim().chars().count() <= DESCRIPTION_CHARS;
                o["description"] = Value::String(d);
            }
            opts.push(o);
        }
        qs.push(json!({"question": text, "header": header, "multiSelect": q.multi, "options": opts}));
    }
    let single = i.questions.len().min(5) == 1 && !i.questions[0].multi && (1..=9).contains(&i.questions[0].options.len().min(11)) && i.questions[0].options.len() <= 9;
    let input = json!({"questions": qs});
    let payload = Payload::from_value(&json!({"session_id": "s", "tool_name": "AskUserQuestion", "tool_input": input})).unwrap();
    let Some(obs) = claude_question("a", CorrelationKey("k".into()), None, &payload, i.bound) else {
        assert!(qs.is_empty() && false, "a payload with questions gave no request");
        return;
    };
    let answerable = obs.answer_path == AnswerPath::Keys;
    if answerable {
        // Compare with the ORIGINAL generated JSON, never with what the
        // parser itself returns, so a cut made inside it cannot hide.
        assert!(within_caps, "over cap yet answerable: {}", format!("{:?}", (qs[0]["question"].as_str().unwrap().chars().count(), qs[0]["header"].as_str().unwrap().chars().count(), qs[0]["options"].as_array().unwrap().iter().map(|o| (o["label"].as_str().unwrap().chars().count(), o.get("description").map(|d| d.as_str().unwrap().chars().count()))).collect::<Vec<_>>())));
        assert_eq!(qs.len(), 1);
        let orig = &qs[0];
        let specs = question_specs(&payload.tool_input).expect("answerable needs valid specs");
        assert_eq!(specs.len(), 1);
        let shown = &obs.questions.as_ref().unwrap()[0];
        let want = |v: &Value| v.as_str().unwrap().trim().to_owned();
        assert_eq!(shown.text, want(&orig["question"]), "question cut or altered");
        assert!(shown.text.chars().count() <= DETAIL_CHARS);
        assert_eq!(shown.header.as_deref(), Some(want(&orig["header"]).as_str()), "header cut or altered");
        assert!(shown.header.as_deref().unwrap().chars().count() <= TITLE_CHARS);
        let orig_opts = orig["options"].as_array().unwrap();
        assert_eq!(shown.options.len(), orig_opts.len(), "options dropped");
        for (o, w) in shown.options.iter().zip(orig_opts) {
            assert_eq!(o.label, want(&w["label"]), "label cut or altered");
            assert_eq!(o.description, w.get("description").map(want), "description cut or altered");
            assert!(!o.label.chars().any(char::is_control));
            assert!(o.description.as_deref().is_none_or(|d| d.chars().count() <= DESCRIPTION_CHARS));
        }
        assert!(!shown.text.chars().any(|c| c.is_control() && c != '\n'), "control character in the text");
    }
    if i.bound && single && all_plain && within_caps {
        assert!(answerable, "a plain question inside every cap is read-only: {}", serde_json::to_string(&payload.tool_input).map(|s| s.len()).unwrap_or(0));
    }
    // What a registry publishes: the same question, or a read-only request.
    let mut r = Registry::new("m".into());
    r.apply(Observation::AgentUpsert(AgentObs {
        id: "a".into(),
        source: Source::Herdr,
        kind: AgentKind::Claude,
        title: "agent".into(),
        cwd: Some("/".repeat(1 + i.cwd as usize)),
        project: None,
        state: AgentState::Working,
        failed: false,
        last_message: None,
        can_send: true,
        role: AgentRole::Main,
        parent_title: None,
    }));
    r.apply(Observation::RequestSeen(obs.clone()));
    for a in r.agents() {
        let n = serde_json::to_vec(&a).unwrap().len();
        assert!(n <= acd_protocol::MAX_AGENT_BYTES, "agent of {n} bytes");
        if let Some(p) = &a.pending {
            if p.answer_path != AnswerPath::None {
                assert_eq!(p.questions, obs.questions, "an answerable question was changed by the registry");
            } else {
                // Made read-only by the size bound: nothing left to choose from.
                if answerable {
                    assert!(p.options.is_empty() && p.questions.is_none(), "a request made read-only still offers options");
                }
            }
        }
    }
});

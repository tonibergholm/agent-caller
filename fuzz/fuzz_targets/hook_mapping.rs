//! Claude Code and Codex hook payloads to requests (hooks/mapping.rs).
//! The input is one or two payloads split at a NUL byte; text that is not a
//! JSON object becomes the `command` of a Bash payload.
//! Invariants:
//! - whole-or-read-only: an answerable permission shows its command whole
//!   (the detail decodes back to the exact command), within the cap, with no
//!   control, bidi or invisible character left raw; a cut one has no options;
//! - two payloads that differ in what would run never render identically;
//! - "yes, always" is offered only for a rule shown whole and saved as shown;
//! - an answerable question is one single-select question with 1 to 9
//!   distinct, non-empty options whose ids match the keys sent.
#![no_main]
use acd_adapters::hooks::mapping::{
    DETAIL_CHARS, claude_permission, claude_question, codex_permission, question_keys, question_specs, rule_displayable,
    rule_text,
};
use acd_adapters::hooks::wire::{Payload, always_rule, canonical_json, reply_allow_always, tool_digest};
use acd_core::{CorrelationKey, RequestObs};
use acd_protocol::{AnswerPath, Effect, RequestKind};
use fuzzlib::{hidden_or_forbidden, unescape_raw};
use libfuzzer_sys::fuzz_target;
use serde_json::{Value, json};

fn key() -> CorrelationKey {
    CorrelationKey("k".into())
}

fn payload(side: &[u8]) -> Option<Payload> {
    if side.is_empty() {
        return None;
    }
    if let Ok(v @ Value::Object(_)) = serde_json::from_slice::<Value>(side) {
        return Payload::from_value(&v);
    }
    let command = String::from_utf8_lossy(side).into_owned();
    Payload::from_value(&json!({"session_id": "s", "tool_name": "Bash", "tool_input": {"command": command}}))
}

/// The command lines of a detail: from the `$ ` line through its `> ` lines.
fn command_in(detail: &str) -> Option<String> {
    let mut out: Vec<&str> = vec![];
    for l in detail.split('\n') {
        if out.is_empty() {
            if let Some(r) = l.strip_prefix("$ ") {
                out.push(r);
            }
        } else if let Some(r) = l.strip_prefix("> ") {
            out.push(r);
        } else {
            break;
        }
    }
    (!out.is_empty()).then(|| out.join("\n"))
}

fn bash_command(p: &Payload) -> Option<&str> {
    (p.tool_name.as_deref() == Some("Bash")).then(|| p.tool_input.get("command")?.as_str()).flatten()
}

/// What decides how the request renders: the input without the agent's own
/// `description` of a Bash command, which is never part of the detail.
fn runs_as(p: &Payload) -> Value {
    let mut v = p.tool_input.clone();
    if bash_command(p).is_some()
        && let Some(o) = v.as_object_mut()
    {
        o.remove("description");
    }
    v
}

fn check_shown(obs: &RequestObs) {
    assert!(!obs.title.chars().any(char::is_control), "title {:?}", obs.title);
    assert!(!acd_protocol::text::has_forbidden_chars(&obs.title));
    for o in obs.options.iter().chain(obs.questions.iter().flatten().flat_map(|q| q.options.iter())) {
        assert!(!o.label.chars().any(char::is_control), "label {:?}", o.label);
        assert!(!o.label.is_empty());
    }
}

fn check_permission(p: &Payload, obs: &RequestObs, codex: bool) {
    check_shown(obs);
    fuzzlib::assert_agent_fits(None, Some(obs));
    assert_eq!(obs.kind, RequestKind::Permission);
    let answerable = obs.answer_path != AnswerPath::None;
    assert_eq!(answerable, !obs.options.is_empty(), "options and path disagree");
    if !answerable {
        assert!(obs.detail.chars().count() <= DETAIL_CHARS, "cut detail over the cap");
        return;
    }
    assert!(obs.detail.chars().count() <= DETAIL_CHARS);
    assert!(
        !obs.detail.chars().any(hidden_or_forbidden),
        "a hidden character is shown raw in {:?}",
        obs.detail
    );
    if let Some(c) = bash_command(p) {
        let shown = command_in(&obs.detail).expect("an answerable Bash card shows its command");
        assert_eq!(unescape_raw(&shown).as_deref(), Some(c), "command shown cut or altered: {:?}", obs.detail);
    }
    assert!(obs.options.iter().any(|o| o.effect == Effect::Allow));
    assert!(obs.options.iter().any(|o| o.effect == Effect::Deny));
    if let Some(o) = obs.options.iter().find(|o| o.effect == Effect::AllowAlways) {
        assert!(!codex, "Codex offers no always option");
        let rule = always_rule(p).expect("always option without a rule");
        let text = rule_text(&rule);
        assert!(rule_displayable(&text));
        assert!(text.chars().count() <= DETAIL_CHARS);
        assert!(!text.chars().any(|c| c.is_control() || hidden_or_forbidden(c)), "{text:?}");
        assert!(o.description.as_deref().is_some_and(|d| d.contains(&text)), "rule not shown whole");
        let sent: Value = serde_json::from_str(&reply_allow_always(&rule)).unwrap();
        let saved = &sent["hookSpecificOutput"]["decision"]["updatedPermissions"][0]["rules"][0];
        assert_eq!(saved["ruleContent"], rule.rule_content.as_str());
        assert_eq!(saved["toolName"], rule.tool_name.as_str());
        assert!(
            p.tool_input.get("dangerouslyDisableSandbox").is_none_or(|v| v == &Value::Bool(false)),
            "a saved rule would ignore the sandbox flag"
        );
    }
}

fn check_question(obs: &RequestObs) {
    check_shown(obs);
    fuzzlib::assert_agent_fits(None, Some(obs));
    if obs.answer_path != AnswerPath::Keys {
        return;
    }
    let qs = obs.questions.as_ref().expect("an answerable question has questions");
    assert_eq!(qs.len(), 1);
    assert!(!qs[0].multi_select);
    let n = qs[0].options.len();
    assert!((1..=9).contains(&n));
    let keys = question_keys(n);
    let mut seen = std::collections::HashSet::new();
    for (i, o) in qs[0].options.iter().enumerate() {
        assert_eq!(o.id, format!("o{}", i + 1));
        assert!(keys.contains_key(&o.id));
        assert!(!o.label.is_empty());
        assert!(seen.insert(o.label.clone()), "two options show as {:?}", o.label);
    }
    assert_eq!(obs.options, qs[0].options);
}

fn run(p: &Payload) -> (Option<RequestObs>, Option<RequestObs>) {
    let claude = claude_permission("a", key(), None, p);
    check_permission(p, &claude, false);
    for collapsed in [false, true] {
        let codex = codex_permission("a", key(), None, p, collapsed);
        check_permission(p, &codex, true);
        if let (Some(c), true) = (bash_command(p), codex.answer_path != AnswerPath::None) {
            let shown = command_in(&codex.detail).expect("an answerable Codex card shows its command");
            assert_eq!(unescape_raw(&shown).as_deref(), Some(c));
        }
    }
    for bound in [false, true] {
        if let Some(q) = claude_question("a", key(), None, p, bound) {
            check_question(&q);
        }
    }
    let _ = question_specs(&p.tool_input);
    // The canonical form is what the correlation digest hashes: it must parse
    // back to the same value, so equal digests mean equal inputs.
    let canon = canonical_json(&p.tool_input);
    assert_eq!(serde_json::from_str::<Value>(&canon).ok().as_ref(), Some(&p.tool_input), "canonical {canon:?}");
    let _ = tool_digest("t", &p.tool_input);
    (Some(claude), None)
}

fuzz_target!(|data: &[u8]| {
    let mut halves = data.splitn(2, |b| *b == 0);
    let a = payload(halves.next().unwrap_or(&[]));
    let b = payload(halves.next().unwrap_or(&[]));
    let ra = a.as_ref().map(run);
    let rb = b.as_ref().map(run);
    if let (Some(a), Some(b), Some((Some(oa), _)), Some((Some(ob), _))) = (&a, &b, ra, rb)
        && a.tool_name == b.tool_name
        && oa.answer_path != AnswerPath::None
        && ob.answer_path != AnswerPath::None
        && runs_as(a) != runs_as(b)
    {
        assert_ne!(oa.detail, ob.detail, "different requests render the same: {:?} / {:?}", a.tool_input, b.tool_input);
    }
});

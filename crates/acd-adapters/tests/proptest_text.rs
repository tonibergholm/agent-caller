//! Whole-or-read-only (spec §6.2): a hook permission card shows exactly what
//! would run, or it is not answerable. For random commands and tool inputs,
//! with control, bidi and invisible characters, and lengths either side of
//! the 4000-character cap:
//!
//! - an answerable card's detail decodes back to the exact command (nothing
//!   cut, dropped or altered), holds no raw control or bidi character, and is
//!   within the cap;
//! - a card whose text was cut is read-only: no answer path, no options, not
//!   ready, so `Request::is_answerable` is false whatever a client does.
mod proptest_support;

use acd_adapters::hooks::mapping::{DETAIL_CHARS, claude_permission};
use acd_adapters::hooks::wire::Payload;
use acd_core::CorrelationKey;
use acd_protocol::text::{is_forbidden_control, is_forbidden_format};
use acd_protocol::*;
use proptest::prelude::*;
use proptest_support::check;
use serde_json::{Value, json};

fn any_char() -> impl Strategy<Value = char> {
    prop_oneof![
        8 => proptest::char::range('a', 'z'),
        3 => proptest::char::range(' ', '~'),
        2 => Just('\n'),
        1 => Just('\t'),
        1 => Just('\\'),
        1 => proptest::char::range('\u{0}', '\u{1f}'),
        1 => proptest::char::range('\u{7f}', '\u{9f}'),
        2 => proptest::sample::select(vec![
            '\u{061C}', '\u{200E}', '\u{202A}', '\u{202E}', '\u{2066}', '\u{2069}', '\u{2028}',
            '\u{2029}', '\u{200B}', '\u{200D}', '\u{FEFF}', '\u{34f}', '👩', 'é',
        ]),
        1 => any::<char>(),
    ]
}

fn text_of(len: impl Into<prop::collection::SizeRange>) -> impl Strategy<Value = String> {
    prop::collection::vec(any_char(), len).prop_map(String::from_iter)
}

/// Short most of the time, sometimes either side of the cap.
fn command() -> impl Strategy<Value = String> {
    prop_oneof![
        6 => text_of(1..80),
        1 => text_of(1900..2100),
        1 => text_of(3900..4100),
        1 => (3990usize..4010).prop_map(|n| "a".repeat(n)),
    ]
}

fn payload(tool: &str, input: Value) -> Payload {
    Payload {
        session_id: "s1".into(),
        tool_name: Some(tool.into()),
        tool_input: input,
        ..Payload::default()
    }
}

fn permission(tool: &str, input: Value) -> acd_core::RequestObs {
    claude_permission("a1", CorrelationKey("k".into()), None, &payload(tool, input))
}

/// The inverse of the display escape for one text: `\\`, `\n`, `\t`, `\r`,
/// `\u{hex}`.
fn unescape(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next()? {
            '\\' => out.push('\\'),
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            'u' => {
                if it.next()? != '{' {
                    return None;
                }
                let mut hex = String::new();
                loop {
                    match it.next()? {
                        '}' => break,
                        h => hex.push(h),
                    }
                }
                out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
            }
            _ => return None,
        }
    }
    Some(out)
}

/// The command a Bash card shows: lines marked `$ ` then `> `.
fn shown_command(detail: &str) -> Option<String> {
    let mut lines = Vec::new();
    for (i, line) in detail.split('\n').enumerate() {
        lines.push(line.strip_prefix(if i == 0 { "$ " } else { "> " })?);
    }
    unescape(&lines.join("\n"))
}

fn read_only(o: &acd_core::RequestObs) -> bool {
    o.answer_path == AnswerPath::None && o.options.is_empty() && !o.ready
}

fn as_request(o: &acd_core::RequestObs) -> Request {
    Request {
        id: "r1".into(),
        revision: 1,
        agent_id: o.agent_id.clone(),
        kind: o.kind,
        status: RequestStatus::Open,
        answer_path: o.answer_path,
        title: o.title.clone(),
        detail: o.detail.clone(),
        options: o.options.clone(),
        questions: o.questions.clone(),
        free_text: o.free_text,
        raw_screen: o.raw_screen.clone(),
        opened_at: Timestamp::from_timestamp_millis(0).unwrap(),
    }
}

fn no_raw_hazards(detail: &str) -> bool {
    detail.chars().all(|c| c == '\n' || !(is_forbidden_control(c) || is_forbidden_format(c)))
}

#[test]
fn a_bash_card_shows_the_exact_command_or_is_read_only() {
    check("bash_whole_or_read_only", 400, command(), |cmd| {
        let o = permission("Bash", json!({ "command": cmd }));
        let answerable = as_request(&o).is_answerable();
        if answerable {
            prop_assert!(!o.options.is_empty() && o.ready);
            prop_assert!(o.detail.chars().count() <= DETAIL_CHARS);
            prop_assert!(no_raw_hazards(&o.detail), "a raw control or bidi char is shown");
            // Exactly the command: not cut, not stripped, not reordered.
            prop_assert_eq!(shown_command(&o.detail), Some(cmd.clone()), "altered text on an answerable card");
        } else {
            prop_assert!(read_only(&o), "not answerable, yet it offers an answer path or options");
        }
        // A command whose escaped form exceeds the cap can never be answered.
        if cmd.chars().count() + 2 > DETAIL_CHARS {
            prop_assert!(!answerable, "a card with cut text was answerable");
        }
        Ok(())
    });
}

#[test]
fn the_cut_boundary_is_exact_for_plain_text() {
    check("bash_cut_boundary", 120, 3990usize..4010, |n| {
        let o = permission("Bash", json!({ "command": "a".repeat(n) }));
        // "$ " plus the command is the whole detail.
        prop_assert_eq!(read_only(&o), n + 2 > DETAIL_CHARS, "n = {}", n);
        prop_assert_eq!(as_request(&o).is_answerable(), n + 2 <= DETAIL_CHARS);
        Ok(())
    });
}

#[test]
fn any_tool_input_is_shown_whole_or_read_only() {
    let value = prop_oneof![
        text_of(0..40).prop_map(Value::from),
        any::<i64>().prop_map(Value::from),
        any::<bool>().prop_map(Value::from),
        prop::collection::vec(text_of(0..10).prop_map(Value::from), 0..4).prop_map(Value::Array),
    ];
    let key = prop_oneof![
        Just("file_path".to_string()),
        Just("command".to_string()),
        Just("description".to_string()),
        text_of(1..8),
    ];
    let input = prop::collection::vec((key, value), 0..6).prop_map(|kv| Value::Object(kv.into_iter().collect()));
    let tool = prop_oneof![Just("Bash".to_string()), Just("Edit".to_string()), Just("Write".to_string()), text_of(0..10)];
    check("tool_input_whole_or_read_only", 400, (tool, input), |(tool, input)| {
        let o = permission(&tool, input.clone());
        if as_request(&o).is_answerable() {
            prop_assert!(o.detail.chars().count() <= DETAIL_CHARS);
            prop_assert!(no_raw_hazards(&o.detail));
            // Every key of the input is on the card, none is hidden
            // (`description` of a Bash command is the one documented skip).
            if let Some(obj) = input.as_object() {
                for k in obj.keys().filter(|k| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')) {
                    let hidden_ok = tool == "Bash" && (k == "command" || k == "description") && obj.get("command").is_some_and(Value::is_string);
                    let name = serde_json::to_string(k).unwrap();
                    prop_assert!(hidden_ok || o.detail.contains(&name) || o.detail.contains(&name.replace('\\', "\\\\")),
                        "key {k:?} is not on the card");
                }
            }
        } else {
            prop_assert!(read_only(&o));
        }
        Ok(())
    });
}

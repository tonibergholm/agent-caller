//! Whole-or-read-only (spec §6.2): a hook permission card shows exactly what
//! would run, or it is not answerable. For random commands and tool inputs,
//! with control, bidi and invisible characters, and lengths either side of
//! the 4000-character cap:
//!
//! - an answerable card's detail decodes back to the exact command (nothing
//!   cut, dropped or altered), holds no raw control or bidi character, and is
//!   within the cap;
//! - an answerable card of any other tool input decodes back to the complete
//!   input, keys and values (a Bash `description` is the one documented skip);
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

/// The inverse of the display escape for one non-command line, whose text is
/// already JSON: the escape adds `\u{hex}` for hidden characters and leaves
/// every other backslash alone. A backslash pair from the JSON is kept as a
/// pair, so a typed `\\u{41}` is not read as an escape.
fn unescape_json_line(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next()? {
            'u' if it.peek() == Some(&'{') => {
                it.next();
                let mut hex = String::new();
                loop {
                    match it.next()? {
                        '}' => break,
                        h => hex.push(h),
                    }
                }
                out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
            }
            other => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    Some(out)
}

/// The `"key": value` lines of a card decoded back to the input object.
fn shown_pairs(lines: &[&str]) -> Option<serde_json::Map<String, Value>> {
    let mut map = serde_json::Map::new();
    for line in lines {
        let json = unescape_json_line(line)?;
        let mut stream = serde_json::Deserializer::from_str(&json).into_iter::<Value>();
        let Value::String(key) = stream.next()?.ok()? else { return None };
        let value: Value = serde_json::from_str(json[stream.byte_offset()..].strip_prefix(": ")?).ok()?;
        if map.insert(key, value).is_some() {
            return None;
        }
    }
    Some(map)
}

/// Splits a card into the Bash command (lines marked `$ ` and `> `, which a
/// key line never starts with) and the remaining lines.
fn split_card(detail: &str) -> (Option<String>, Vec<&str>) {
    let lines: Vec<&str> = if detail.is_empty() { vec![] } else { detail.split('\n').collect() };
    let n = lines.iter().take_while(|l| l.starts_with("$ ") || l.starts_with("> ")).count();
    let command = (n > 0).then(|| shown_command(&lines[..n].join("\n"))).flatten();
    (command, lines[n..].to_vec())
}

#[test]
fn any_tool_input_is_shown_whole_or_read_only() {
    let scalar = prop_oneof![
        text_of(0..40).prop_map(Value::from),
        any::<i64>().prop_map(Value::from),
        any::<bool>().prop_map(Value::from),
        Just(Value::Null),
    ];
    let value = prop_oneof![
        scalar.clone(),
        prop::collection::vec(text_of(0..10).prop_map(Value::from), 0..4).prop_map(Value::Array),
        prop::collection::vec((text_of(1..4), scalar.clone()), 0..3).prop_map(|kv| Value::Object(kv.into_iter().collect())),
    ];
    let key = prop_oneof![
        Just("file_path".to_string()),
        Just("command".to_string()),
        Just("description".to_string()),
        Just("new_string".to_string()),
        text_of(1..8),
    ];
    let object = prop::collection::vec((key, value.clone()), 0..6).prop_map(|kv| Value::Object(kv.into_iter().collect()));
    // Mostly objects, as the tools send them; sometimes a bare value.
    let input = prop_oneof![8 => object, 1 => value];
    let tool = prop_oneof![Just("Bash".to_string()), Just("Edit".to_string()), Just("Write".to_string()), text_of(0..10)];
    check("tool_input_whole_or_read_only", 400, (tool, input), |(tool, input)| {
        let o = permission(&tool, input.clone());
        if as_request(&o).is_answerable() {
            prop_assert!(o.detail.chars().count() <= DETAIL_CHARS);
            prop_assert!(no_raw_hazards(&o.detail));
            let (command, lines) = split_card(&o.detail);
            match input.as_object() {
                Some(obj) => {
                    // The whole input comes back, keys and values. A Bash
                    // command shows as the command and its `description` is
                    // the one documented skip.
                    let bash = if tool == "Bash" { obj.get("command").and_then(Value::as_str) } else { None };
                    let mut want = obj.clone();
                    if bash.is_some() {
                        want.remove("command");
                        want.remove("description");
                    }
                    prop_assert_eq!(command.as_deref(), bash, "the command on the card");
                    prop_assert_eq!(shown_pairs(&lines), Some(want), "the card differs from the input: {:?}", o.detail);
                }
                // A bare value is shown as its own JSON; nothing at all for null.
                None => {
                    prop_assert!(command.is_none());
                    let shown = match lines.as_slice() {
                        [] => Some(Value::Null),
                        [line] => unescape_json_line(line).and_then(|j| serde_json::from_str(&j).ok()),
                        _ => None,
                    };
                    prop_assert_eq!(shown, Some(input.clone()), "the card differs from the input: {:?}", o.detail);
                }
            }
        } else {
            prop_assert!(read_only(&o));
        }
        Ok(())
    });
}

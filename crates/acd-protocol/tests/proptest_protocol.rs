//! Property tests for the acd/1 wire types: round-trips of every message
//! type, pairing strings and the text rules. Seeds are fixed (see
//! `proptest_support`); `ACD_PROPTEST_CASES` raises the case count.
mod proptest_support;

use std::sync::LazyLock;

use acd_protocol::pairing::{Pairing, pins_equal, spki_pin};
use acd_protocol::text::{has_forbidden_chars, is_forbidden_control, is_forbidden_format};
use acd_protocol::*;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use proptest::prelude::*;
use proptest_support::check;
use serde_json::Value;

// ---- strategies -----------------------------------------------------------

/// Characters that matter to the text rules: plain, controls, bidi, joiners.
fn any_char() -> impl Strategy<Value = char> {
    prop_oneof![
        6 => proptest::char::range('a', 'z'),
        2 => proptest::char::range(' ', '~'),
        1 => Just('\n'),
        1 => Just('\t'),
        1 => proptest::char::range('\u{0}', '\u{1f}'),
        1 => Just('\u{7f}'),
        1 => proptest::char::range('\u{80}', '\u{9f}'),
        2 => proptest::sample::select(vec![
            '\u{061C}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}',
            '\u{202E}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}', '\u{2028}', '\u{2029}',
            '\u{200B}', '\u{200C}', '\u{200D}', '\u{FE0F}', '👩', 'é', '\u{FEFF}', '\u{34f}',
        ]),
        1 => any::<char>(),
    ]
}

fn text() -> impl Strategy<Value = String> {
    prop::collection::vec(any_char(), 0..16).prop_map(String::from_iter)
}

fn word() -> impl Strategy<Value = String> {
    prop::collection::vec(proptest::char::range('a', 'z'), 1..8).prop_map(String::from_iter)
}

fn opt<T: std::fmt::Debug + Clone>(s: impl Strategy<Value = T>) -> impl Strategy<Value = Option<T>> {
    prop_oneof![1 => Just(None), 3 => s.prop_map(Some)]
}

macro_rules! enum_of {
    ($($v:path),+ $(,)?) => { proptest::sample::select(vec![$($v),+]) };
}

fn timestamp() -> impl Strategy<Value = Timestamp> {
    // acd/1 timestamps are whole milliseconds.
    (0i64..4_102_444_800_000).prop_map(|ms| Timestamp::from_timestamp_millis(ms).unwrap())
}

fn request_option() -> impl Strategy<Value = RequestOption> {
    (
        word(),
        text(),
        opt(text()),
        enum_of![Effect::Allow, Effect::AllowAlways, Effect::Deny, Effect::Choose, Effect::Unknown],
        any::<bool>(),
    )
        .prop_map(|(id, label, description, effect, confirm)| RequestOption {
            id,
            label,
            description,
            effect,
            confirm,
        })
}

fn question() -> impl Strategy<Value = Question> {
    (word(), opt(text()), text(), any::<bool>(), prop::collection::vec(request_option(), 0..3))
        .prop_map(|(id, header, text, multi_select, options)| Question {
            id,
            header,
            text,
            multi_select,
            options,
        })
}

fn request() -> impl Strategy<Value = Request> {
    let head = (
        word(),
        any::<u64>(),
        word(),
        enum_of![
            RequestKind::Permission,
            RequestKind::Question,
            RequestKind::BlockedUnparsed,
            RequestKind::Unknown
        ],
        enum_of![
            RequestStatus::Open,
            RequestStatus::Answering,
            RequestStatus::Resolved,
            RequestStatus::Unknown
        ],
        enum_of![
            AnswerPath::Hook,
            AnswerPath::Keys,
            AnswerPath::T3,
            AnswerPath::External,
            AnswerPath::None,
            AnswerPath::Unknown
        ],
    );
    let body = (
        text(),
        text(),
        prop::collection::vec(request_option(), 0..3),
        opt(prop::collection::vec(question(), 0..3)),
        any::<bool>(),
        opt(text()),
        timestamp(),
    );
    (head, body).prop_map(
        |(
            (id, revision, agent_id, kind, status, answer_path),
            (title, detail, options, questions, free_text, raw_screen, opened_at),
        )| Request {
            id,
            revision,
            agent_id,
            kind,
            status,
            answer_path,
            title,
            detail,
            options,
            questions,
            free_text,
            raw_screen,
            opened_at,
        },
    )
}

fn agent() -> impl Strategy<Value = Agent> {
    let head = (
        word(),
        word(),
        enum_of![Source::Herdr, Source::T3, Source::Hooks, Source::External, Source::Unknown],
        enum_of![AgentKind::Claude, AgentKind::Codex, AgentKind::Other, AgentKind::Unknown],
        text(),
        opt(text()),
        opt(text()),
        enum_of![
            AgentState::Working,
            AgentState::NeedsYou,
            AgentState::Done,
            AgentState::Idle,
            AgentState::Unknown
        ],
    );
    let tail = (
        timestamp(),
        any::<bool>(),
        opt(text()),
        opt(request()),
        any::<bool>(),
        enum_of![AgentRole::Main, AgentRole::Subagent],
        opt(text()),
    );
    (head, tail).prop_map(
        |(
            (id, machine_id, source, kind, title, cwd, project, state),
            (state_since, failed, last_message, pending, can_send, role, parent_title),
        )| Agent {
            id,
            machine_id,
            source,
            kind,
            title,
            cwd,
            project,
            state,
            state_since,
            failed,
            last_message,
            pending,
            can_send,
            role,
            parent_title,
        },
    )
}

fn source_status() -> impl Strategy<Value = SourceStatus> {
    (
        enum_of![
            SourceState::Ok,
            SourceState::Degraded,
            SourceState::Disabled,
            SourceState::Unconfigured,
            SourceState::Unknown
        ],
        opt(text()),
    )
        .prop_map(|(state, message)| SourceStatus { state, message })
}

fn machine() -> impl Strategy<Value = Machine> {
    (word(), text(), word(), source_status(), source_status(), source_status(), any::<bool>())
        .prop_map(|(id, name, acd_version, hooks, herdr, t3, external)| Machine {
            id,
            name,
            acd_version,
            sources: Sources { hooks, herdr, t3, external },
        })
}

fn error_code() -> impl Strategy<Value = ErrorCode> {
    enum_of![
        ErrorCode::RequestGone,
        ErrorCode::RequestChanged,
        ErrorCode::RequestBusy,
        ErrorCode::RequestNotReady,
        ErrorCode::NotAnswerable,
        ErrorCode::AgentGone,
        ErrorCode::AgentBusy,
        ErrorCode::NotSupported,
        ErrorCode::ScreenMismatch,
        ErrorCode::BadRequest,
        ErrorCode::DuplicateMismatch,
        ErrorCode::SourceUnavailable,
        ErrorCode::Unauthorized,
        ErrorCode::UnsupportedProtocol,
        ErrorCode::Unknown
    ]
}

fn command_result() -> impl Strategy<Value = CommandResult> {
    prop_oneof![
        word().prop_map(|id| CommandResult::ok(&id)),
        word().prop_map(|id| CommandResult::delivered(&id)),
        (word(), error_code(), text()).prop_map(|(id, c, m)| CommandResult::err(&id, c, &m)),
    ]
}

fn cursor() -> impl Strategy<Value = Cursor> {
    (word(), any::<u64>()).prop_map(|(epoch, seq)| Cursor { epoch, seq })
}

fn ev() -> impl Strategy<Value = (String, u64)> {
    (word(), any::<u64>())
}

fn server_message() -> impl Strategy<Value = ServerMessage> {
    prop_oneof![
        (machine(), prop::collection::vec(word(), 0..4), ev()).prop_map(
            |(machine, capabilities, (epoch, seq))| ServerMessage::Welcome {
                machine,
                capabilities,
                epoch,
                seq
            }
        ),
        (ev(), prop::collection::vec(agent(), 0..3))
            .prop_map(|((epoch, seq), agents)| ServerMessage::Snapshot { epoch, seq, agents }),
        (ev(), agent())
            .prop_map(|((epoch, seq), agent)| ServerMessage::AgentUpdated { epoch, seq, agent }),
        (ev(), word())
            .prop_map(|((epoch, seq), agent_id)| ServerMessage::AgentRemoved { epoch, seq, agent_id }),
        (ev(), request())
            .prop_map(|((epoch, seq), request)| ServerMessage::RequestOpened { epoch, seq, request }),
        (
            ev(),
            word(),
            word(),
            enum_of![
                ResolvedBy::Inbox,
                ResolvedBy::Terminal,
                ResolvedBy::Agent,
                ResolvedBy::Superseded,
                ResolvedBy::Uncertain,
                ResolvedBy::Lost,
                ResolvedBy::Unknown
            ],
            opt(word()),
            opt(enum_of![
                ResolveReason::StrayKeystroke,
                ResolveReason::Rejected,
                ResolveReason::Unknown
            ]),
            opt(text()),
        )
            .prop_map(|((epoch, seq), request_id, agent_id, by, option_id, reason, message)| {
                ServerMessage::RequestResolved {
                    epoch,
                    seq,
                    request_id,
                    agent_id,
                    by,
                    option_id,
                    reason,
                    message,
                }
            }),
        (ev(), machine())
            .prop_map(|((epoch, seq), machine)| ServerMessage::MachineUpdated { epoch, seq, machine }),
        command_result().prop_map(ServerMessage::Result),
        word().prop_map(|id| ServerMessage::Pong { id }),
        (error_code(), text(), opt(prop::collection::vec(word(), 0..3))).prop_map(
            |(code, message, supported)| ServerMessage::Error { code, message, supported }
        ),
    ]
}

/// A token wrapper whose `Debug` shows only the length, so a shrunk input
/// never prints a secret.
#[derive(Clone)]
struct Secret(String);
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secret({} bytes)", self.0.len())
    }
}

fn client_message() -> impl Strategy<Value = ClientMessage> {
    let info = (word(), text(), word()).prop_map(|(id, name, version)| ClientInfo { id, name, version });
    let token = prop::collection::vec(any::<u8>(), 32).prop_map(|b| Secret(URL_SAFE_NO_PAD.encode(b)));
    // Exactly one of option_id or text, as the schema requires.
    let answer_body = prop_oneof![
        word().prop_map(|o| (Some(o), None)),
        text().prop_map(|t| (None, Some(t)))
    ];
    prop_oneof![
        (word(), token, info, opt(cursor())).prop_map(|(protocol, token, client, resume)| {
            ClientMessage::Hello { protocol, token: token.0, client, resume }
        }),
        (word(), word(), opt(any::<u64>()), answer_body).prop_map(
            |(id, request_id, revision, (option_id, text))| ClientMessage::RequestAnswer {
                id,
                request_id,
                revision,
                option_id,
                text
            }
        ),
        (word(), word(), text())
            .prop_map(|(id, agent_id, text)| ClientMessage::AgentSend { id, agent_id, text }),
        (word(), word()).prop_map(|(id, agent_id)| ClientMessage::AgentMarkSeen { id, agent_id }),
        word().prop_map(|id| ClientMessage::Ping { id }),
    ]
}

// ---- schemas --------------------------------------------------------------

fn validator(name: &str) -> jsonschema::Validator {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("schema/{name}.schema.json"));
    let schema: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    jsonschema::validator_for(&schema).unwrap()
}
static CLIENT_SCHEMA: LazyLock<jsonschema::Validator> = LazyLock::new(|| validator("client"));
static SERVER_SCHEMA: LazyLock<jsonschema::Validator> = LazyLock::new(|| validator("server"));

// ---- message round-trips ----------------------------------------------------

#[test]
fn every_server_message_round_trips_and_validates() {
    check("server_round_trip", 300, server_message(), |msg| {
        let json = serde_json::to_string(&msg).unwrap();
        prop_assert_eq!(decode_server(&json).unwrap(), Some(msg.clone()));
        let value: Value = serde_json::from_str(&json).unwrap();
        // Re-encoding the decoded value is byte-stable.
        let again =
            serde_json::to_string(&serde_json::from_value::<ServerMessage>(value.clone()).unwrap())
                .unwrap();
        prop_assert_eq!(again, json);
        let errors: Vec<String> = SERVER_SCHEMA.iter_errors(&value).map(|e| e.to_string()).collect();
        prop_assert!(errors.is_empty(), "schema errors: {errors:?}");
        Ok(())
    });
}

#[test]
fn every_client_message_round_trips_and_validates() {
    check("client_round_trip", 300, client_message(), |msg| {
        let json = serde_json::to_string(&msg).unwrap();
        let back: ClientMessage = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(&back, &msg);
        let value: Value = serde_json::from_str(&json).unwrap();
        let errors: Vec<String> = CLIENT_SCHEMA.iter_errors(&value).map(|e| e.to_string()).collect();
        prop_assert!(errors.is_empty(), "schema errors: {errors:?}");
        // `command_id` is present for exactly the commands (not hello).
        prop_assert_eq!(msg.command_id().is_some(), !matches!(msg, ClientMessage::Hello { .. }));
        Ok(())
    });
}

#[test]
fn an_answer_with_both_or_neither_of_option_and_text_fails_the_schema() {
    let both_or_neither = (
        word(),
        word(),
        prop_oneof![
            (word(), text()).prop_map(|(o, t)| (Some(o), Some(t))),
            Just((None, None)),
        ],
    );
    check("answer_xor_schema", 100, both_or_neither, |(id, request_id, (option_id, text))| {
        let msg = ClientMessage::RequestAnswer { id, request_id, revision: None, option_id, text };
        let value = serde_json::to_value(&msg).unwrap();
        prop_assert!(!CLIENT_SCHEMA.is_valid(&value));
        Ok(())
    });
}

#[test]
fn debug_output_never_holds_the_token_or_typed_text() {
    check("client_debug_redacts", 200, client_message(), |msg| {
        let shown = format!("{msg:?}");
        match &msg {
            ClientMessage::Hello { token, .. } => prop_assert!(!shown.contains(token.as_str())),
            ClientMessage::RequestAnswer { text: Some(t), .. }
            | ClientMessage::AgentSend { text: t, .. }
                if t.len() > 3 =>
            {
                // A short text can occur inside a field name; only long ones are checked.
                prop_assert!(!shown.contains(t.as_str()), "typed text leaked into Debug");
            }
            _ => {}
        }
        Ok(())
    });
}

// ---- unknown and malformed frames --------------------------------------------

fn json_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::from),
        any::<i64>().prop_map(Value::from),
        text().prop_map(Value::from),
    ];
    leaf.prop_recursive(3, 24, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            prop::collection::vec((word(), inner), 0..4)
                .prop_map(|kv| Value::Object(kv.into_iter().collect())),
        ]
    })
}

#[test]
fn decode_server_never_panics_and_skips_only_unknown_types() {
    let known: Vec<String> = SERVER_MESSAGE_TYPES.iter().map(|t| t.to_string()).collect();
    let frames = prop_oneof![
        // arbitrary JSON
        json_value().prop_map(|v| v.to_string()),
        // arbitrary text
        text(),
        // a typed object with a random body
        (prop_oneof![word(), proptest::sample::select(known)], json_value()).prop_map(
            |(t, body)| {
                let mut o = match body {
                    Value::Object(o) => o,
                    _ => Default::default(),
                };
                o.insert("type".into(), Value::String(t));
                Value::Object(o).to_string()
            }
        ),
    ];
    check("decode_server_total", 400, frames, |raw| {
        let decoded = decode_server(&raw);
        let value: Result<Value, _> = serde_json::from_str(&raw);
        match (&decoded, &value) {
            (_, Err(_)) => prop_assert!(decoded.is_err(), "malformed JSON must not decode"),
            (Ok(None), Ok(v)) => {
                let t = v.get("type").and_then(Value::as_str);
                prop_assert!(
                    t.is_some_and(|t| !SERVER_MESSAGE_TYPES.contains(&t)),
                    "skipped a known or typeless frame"
                );
            }
            (Ok(Some(m)), Ok(v)) => {
                // What decoded must re-encode to the same type tag.
                let again = serde_json::to_value(m).unwrap();
                prop_assert_eq!(again.get("type"), v.get("type"));
            }
            (Err(_), Ok(_)) => {}
        }
        Ok(())
    });
}

// ---- text rules ---------------------------------------------------------------

#[test]
fn text_rules_are_per_character_and_disjoint() {
    check("text_rules_chars", 300, any_char(), |c| {
        prop_assert!(
            !(is_forbidden_control(c) && is_forbidden_format(c)),
            "classes overlap on {c:?}"
        );
        if c == '\n' || c == '\t' {
            prop_assert!(!has_forbidden_chars(&c.to_string()));
        }
        prop_assert_eq!(
            has_forbidden_chars(&c.to_string()),
            is_forbidden_control(c) || is_forbidden_format(c)
        );
        // Every forbidden control is a control; the format set is never ASCII.
        prop_assert!(!is_forbidden_control(c) || c.is_control());
        prop_assert!(!is_forbidden_format(c) || !c.is_ascii());
        Ok(())
    });
}

/// The forbidden sets written out, not derived from the classifiers: C0
/// controls, DEL and C1 controls except `\n` and `\t`; and the bidi marks,
/// embeddings/overrides, isolates and line/paragraph separators.
fn expected_control(c: char) -> bool {
    matches!(c as u32, 0x00..=0x08 | 0x0B..=0x1F | 0x7F..=0x9F)
}

fn expected_format(c: char) -> bool {
    matches!(c as u32, 0x061C | 0x200E | 0x200F | 0x202A..=0x202E | 0x2066..=0x2069 | 0x2028 | 0x2029)
}

#[test]
fn the_character_classes_match_independent_expected_sets() {
    // Every code point up to the last one the sets name, then sampled ones.
    for c in (0u32..=0x2100).filter_map(char::from_u32) {
        assert_eq!(is_forbidden_control(c), expected_control(c), "control class of U+{:04X}", c as u32);
        assert_eq!(is_forbidden_format(c), expected_format(c), "format class of U+{:04X}", c as u32);
        assert_eq!(has_forbidden_chars(&c.to_string()), expected_control(c) || expected_format(c), "U+{:04X}", c as u32);
    }
    for c in ['\n', '\t', ' ', 'a', '\u{200B}', '\u{200C}', '\u{200D}', '\u{FEFF}', '👩', 'é'] {
        assert!(!has_forbidden_chars(&c.to_string()), "{c:?} must pass");
    }
    check("text_rules_expected_sets", 300, any_char(), |c| {
        prop_assert_eq!(is_forbidden_control(c), expected_control(c), "control class of {:?}", c);
        prop_assert_eq!(is_forbidden_format(c), expected_format(c), "format class of {:?}", c);
        Ok(())
    });
}

#[test]
fn forbidden_text_is_closed_under_concatenation_and_insertion() {
    check("text_rules_concat", 300, (text(), text(), any_char(), any::<prop::sample::Index>()), |(a, b, c, at)| {
        let joined = format!("{a}{b}");
        prop_assert_eq!(
            has_forbidden_chars(&joined),
            has_forbidden_chars(&a) || has_forbidden_chars(&b)
        );
        // Inserting a forbidden character anywhere makes the text forbidden.
        let boundaries: Vec<usize> = a.char_indices().map(|(i, _)| i).chain([a.len()]).collect();
        let mut spliced = a.clone();
        spliced.insert(boundaries[at.index(boundaries.len())], c);
        if is_forbidden_control(c) || is_forbidden_format(c) {
            prop_assert!(has_forbidden_chars(&spliced));
        }
        Ok(())
    });
}

// ---- pairing strings -----------------------------------------------------------

fn host() -> impl Strategy<Value = String> {
    let label = prop::collection::vec(
        prop_oneof![
            proptest::char::range('a', 'z'),
            proptest::char::range('0', '9'),
            Just('-')
        ],
        0..8,
    )
    .prop_map(|cs| {
        // A label may not start or end with a hyphen.
        let mut s = String::from("a");
        s.extend(cs);
        s.push('z');
        s
    });
    prop_oneof![
        any::<u32>().prop_map(|n| std::net::Ipv4Addr::from(n).to_string()),
        any::<u128>().prop_map(|n| std::net::Ipv6Addr::from(n).to_string()),
        prop::collection::vec(label, 1..4).prop_map(|ls| ls.join(".")),
    ]
}

fn pairing() -> impl Strategy<Value = Pairing> {
    (
        host(),
        1u16..=u16::MAX,
        prop::collection::vec(any::<u8>(), 32),
        prop::collection::vec(any::<u8>(), 32),
    )
        .prop_map(|(host, port, pin, token)| Pairing {
            host,
            port,
            pin: format!("sha256:{}", URL_SAFE_NO_PAD.encode(pin)),
            token: URL_SAFE_NO_PAD.encode(token),
        })
}

#[test]
fn a_pairing_line_round_trips() {
    check("pairing_round_trip", 400, pairing(), |p| {
        let line = p.to_line();
        prop_assert_eq!(Pairing::parse(&line), Ok(p.clone()));
        // Surrounding whitespace (a pasted line) is tolerated.
        prop_assert_eq!(Pairing::parse(&format!("  {line}\n")), Ok(p));
        Ok(())
    });
}

/// A pairing line that was edited by a handful of random operations. The
/// value redacts itself: it holds a token.
#[derive(Clone)]
struct MutatedLine(String);
impl std::fmt::Debug for MutatedLine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MutatedLine({} bytes, redacted)", self.0.len())
    }
}

fn mutated_line() -> impl Strategy<Value = MutatedLine> {
    let edit = (any::<prop::sample::Index>(), 0u8..4, any_char());
    (pairing(), prop::collection::vec(edit, 0..4)).prop_map(|(p, edits)| {
        let mut chars: Vec<char> = p.to_line().chars().collect();
        for (at, op, c) in edits {
            match op {
                0 if !chars.is_empty() => {
                    chars.remove(at.index(chars.len()));
                }
                1 => chars.insert(at.index(chars.len() + 1), c),
                2 if !chars.is_empty() => {
                    let i = at.index(chars.len());
                    chars[i] = c;
                }
                _ => chars.truncate(at.index(chars.len() + 1)),
            }
        }
        MutatedLine(chars.into_iter().collect())
    })
}

#[test]
fn parsing_is_total_and_accepted_lines_are_canonical() {
    check("pairing_parse_total", 500, mutated_line(), |MutatedLine(line)| {
        // Never panics; and whatever parses re-parses to itself from its
        // canonical line (no second reading of the same string).
        if let Ok(p) = Pairing::parse(&line) {
            prop_assert!(p.port != 0);
            prop_assert_eq!(Pairing::parse(&p.to_line()), Ok(p));
        }
        Ok(())
    });
}

#[test]
fn arbitrary_text_only_parses_as_a_pairing_with_the_scheme() {
    check("pairing_arbitrary", 300, text(), |s| {
        if Pairing::parse(&s).is_ok() {
            prop_assert!(s.trim().starts_with("acd://"));
        }
        Ok(())
    });
}

#[test]
fn pins_compare_equal_only_for_the_same_key_bytes() {
    let der = prop::collection::vec(any::<u8>(), 0..64);
    check("pins_equal", 300, (der.clone(), der), |(a, b)| {
        let (pa, pb) = (spki_pin(&a), spki_pin(&b));
        prop_assert!(pins_equal(&pa, &pa));
        prop_assert_eq!(pins_equal(&pa, &pb), a == b);
        // A pin with anything appended or a different prefix never matches.
        let longer = format!("{pa}A");
        let other_prefix = pa.replacen("sha256:", "sha1:", 1);
        prop_assert!(!pins_equal(&pa, &longer));
        prop_assert!(!pins_equal(&pa, &other_prefix));
        Ok(())
    });
}

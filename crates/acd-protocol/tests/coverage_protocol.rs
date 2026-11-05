//! Coverage-driven tests (docs/research/2026-11-05-coverage.md).
use acd_protocol::pairing::{Pairing, PairingError};
use acd_protocol::{ClientInfo, ClientMessage, Cursor};

fn info() -> ClientInfo {
    ClientInfo { id: "c".into(), name: "n".into(), version: "1".into() }
}

/// `Debug` is what tracing prints; it must show lengths, never the bearer
/// token or the user's answer text.
#[test]
fn debug_of_client_messages_never_prints_tokens_or_text() {
    let secret = "TOKEN-SECRET-abcdefghijklmnopqrstuvwxyz0123456789";
    let hello = ClientMessage::Hello {
        protocol: "acd/1".into(),
        token: secret.into(),
        client: info(),
        resume: Some(Cursor { epoch: "e".into(), seq: 3 }),
    };
    let answer = ClientMessage::RequestAnswer {
        id: "i".into(),
        request_id: "r".into(),
        revision: Some(2),
        option_id: None,
        text: Some("TYPED-ANSWER-TEXT".into()),
    };
    let send = ClientMessage::AgentSend { id: "i".into(), agent_id: "a".into(), text: "TYPED-REPLY".into() };
    let seen = ClientMessage::AgentMarkSeen { id: "i".into(), agent_id: "a".into() };
    let ping = ClientMessage::Ping { id: "p".into() };

    let shown = format!("{hello:?} {answer:?} {send:?} {seen:?} {ping:?}");
    for hidden in [secret, "TYPED-ANSWER-TEXT", "TYPED-REPLY"] {
        assert!(!shown.contains(hidden), "{hidden} leaked: {shown}");
    }
    assert!(shown.contains(&format!("token_bytes: {}", secret.len())));
    assert!(shown.contains("text_bytes: Some(17)"));
    assert!(shown.contains("text_bytes: 11"));
}

/// Only commands are idempotency-keyed; `hello` has no id.
#[test]
fn command_ids_exist_for_every_command_but_hello() {
    let hello = ClientMessage::Hello { protocol: "acd/1".into(), token: "t".into(), client: info(), resume: None };
    assert_eq!(hello.command_id(), None);
    for (m, id) in [
        (ClientMessage::RequestAnswer { id: "a".into(), request_id: "r".into(), revision: None, option_id: None, text: None }, "a"),
        (ClientMessage::AgentSend { id: "b".into(), agent_id: "x".into(), text: "t".into() }, "b"),
        (ClientMessage::AgentMarkSeen { id: "c".into(), agent_id: "x".into() }, "c"),
        (ClientMessage::Ping { id: "d".into() }, "d"),
    ] {
        assert_eq!(m.command_id(), Some(id));
    }
}

const PIN: &str = "sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const TOKEN: &str = "CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCA";

#[test]
fn pairing_parse_rejects_each_malformed_part_with_its_own_error() {
    let line = |addr: &str| format!("acd://{addr}?pin={PIN}#{TOKEN}");
    assert!(Pairing::parse(&line("host.example:7443")).is_ok());
    assert!(Pairing::parse(&line("[::1]:7443")).is_ok());
    let cases: Vec<(String, PairingError)> = vec![
        (format!("http://h:1?pin={PIN}#{TOKEN}"), PairingError::Scheme),
        (format!("acd://h:1?pin={PIN}"), PairingError::Token),
        (format!("acd://h:1?pin={PIN}#{TOKEN}#x"), PairingError::Token),
        (format!("acd://h:1?pin={PIN}#short"), PairingError::Token),
        (format!("acd://h:1#{TOKEN}"), PairingError::Pin),
        (format!("acd://h:1?pin=sha256:short#{TOKEN}"), PairingError::Pin),
        (line("[::1]7443"), PairingError::Port),
        (line("[not-ipv6]:1"), PairingError::Host),
        (line("[::1"), PairingError::Host),
        (line("h"), PairingError::Port),
        (line("-bad.example:1"), PairingError::Host),
        (line("a..b:1"), PairingError::Host),
        (line("under_score:1"), PairingError::Host),
        (line("h:0"), PairingError::Port),
        (line("h:65536"), PairingError::Port),
        (line("h:x"), PairingError::Port),
    ];
    for (input, want) in cases {
        assert_eq!(Pairing::parse(&input).unwrap_err(), want, "{input}");
    }
    // A hostname may not be longer than 253 bytes.
    let long = format!("{}.{}.{}.{}", "a".repeat(63), "b".repeat(63), "c".repeat(63), "d".repeat(63));
    assert_eq!(Pairing::parse(&line(&format!("{long}:1"))).unwrap_err(), PairingError::Host);
}

#[test]
fn a_pairing_line_round_trips_and_its_debug_hides_the_token() {
    for addr in ["host.example:7443", "[fd7a::1]:7443", "100.64.0.1:7443"] {
        let s = format!("acd://{addr}?pin={PIN}#{TOKEN}");
        let p = Pairing::parse(&s).unwrap();
        assert_eq!(p.to_line(), s);
        assert!(!format!("{p:?}").contains(TOKEN));
    }
}

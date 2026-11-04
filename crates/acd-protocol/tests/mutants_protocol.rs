//! Tests added from a cargo-mutants run (docs/research/2026-11-04-mutation-testing.md).
//! Each test names the mutant it kills.
use acd_protocol::{pairing::*, *};

const TOKEN: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";
const PIN: &str = "sha256:ICEiIyQlJicoKSorLC0uLzAxMjM0NTY3ODk6Ozw9Pj8";

/// `lib.rs`: `<<` -> `>>` and `*` -> `/` or `+` in the limits. The numbers are
/// part of the wire contract (crates/acd-protocol/PROTOCOL.md, Limits), so they are pinned.
#[test]
fn the_limits_are_the_documented_numbers() {
    assert_eq!(MAX_FRAME_BYTES, 1_048_576);
    assert_eq!(MAX_AGENT_BYTES, 65_536);
    assert_eq!(MAX_AGENTS_PER_MACHINE, 1000);
}

/// `valid_token`: `&&` -> `||`. A token of the right length that is not
/// base64url is refused, and so is a right-length token of the standard
/// alphabet.
#[test]
fn a_token_of_the_right_length_but_the_wrong_alphabet_is_refused() {
    for bad in ["!".repeat(43), format!("{}+", &TOKEN[..42]), format!("{}/", &TOKEN[..42])] {
        let line = format!("acd://host.example:47800?pin={PIN}#{bad}");
        assert_eq!(Pairing::parse(&line), Err(PairingError::Token), "{bad}");
    }
    // Last character carries nonzero trailing bits: not the canonical encoding.
    let line = format!("acd://host.example:47800?pin={PIN}#{}B", &TOKEN[..42]);
    assert_eq!(Pairing::parse(&line), Err(PairingError::Token));
}

/// `decode_pin`: the same two shape checks on the pin.
#[test]
fn a_pin_of_the_right_length_but_the_wrong_alphabet_is_refused() {
    let good = format!("sha256:{}", "A".repeat(43));
    assert!(pins_equal(&good, &good));
    for bad in [format!("sha256:{}", "!".repeat(43)), format!("sha256:{}B", "A".repeat(42))] {
        assert!(!pins_equal(&bad, &bad), "{bad}");
        let line = format!("acd://host.example:47800?pin={bad}#{TOKEN}");
        assert_eq!(Pairing::parse(&line), Err(PairingError::Pin), "{bad}");
    }
}

/// `Debug for Pairing`: the token never reaches a log through `{:?}`; the
/// rest of the pairing stays readable.
#[test]
fn pairing_debug_hides_the_token_only() {
    let p = Pairing { host: "devbox.example".into(), port: 47800, pin: PIN.into(), token: TOKEN.into() };
    let s = format!("{p:?}");
    assert!(!s.contains(TOKEN), "{s}");
    assert!(s.contains("devbox.example") && s.contains("47800") && s.contains("REDACTED"), "{s}");
}

/// `Debug for ClientMessage`: tokens and answer text are reduced to lengths.
#[test]
fn client_message_debug_never_prints_tokens_or_text() {
    let hello = ClientMessage::Hello {
        protocol: PROTOCOL.into(),
        token: "SECRET-TOKEN-VALUE".into(),
        client: ClientInfo { id: "i".into(), name: "t".into(), version: "1".into() },
        resume: None,
    };
    let s = format!("{hello:?}");
    assert!(!s.contains("SECRET-TOKEN-VALUE") && s.contains("Hello") && s.contains("18"), "{s}");
    let answer = ClientMessage::RequestAnswer {
        id: "c1".into(),
        request_id: "r1".into(),
        revision: Some(3),
        option_id: Some("o".into()),
        text: Some("SECRET-ANSWER-TEXT".into()),
    };
    let s = format!("{answer:?}");
    assert!(!s.contains("SECRET-ANSWER-TEXT") && s.contains("RequestAnswer") && s.contains("r1"), "{s}");
    let send = ClientMessage::AgentSend { id: "c2".into(), agent_id: "a1".into(), text: "SECRET-SEND-TEXT".into() };
    let s = format!("{send:?}");
    assert!(!s.contains("SECRET-SEND-TEXT") && s.contains("AgentSend") && s.contains("a1"), "{s}");
}

//! Coverage-driven tests (docs/research/2026-11-05-coverage.md). One test owns
//! the thread-local log subscriber so call-site interest cannot race.
use std::sync::{Arc, Mutex};

use acd_adapters::hooks::mapping::payload_for_log;
use acd_adapters::t3::conn::CallError;
use acd_adapters::t3::outcome::{classify, Outcome, DEFECT, INTERRUPTED, MIXED, NO_REPLY, REFUSED};
use acd_adapters::t3::wire::{Cause, Failure};
use acd_adapters::text::{redact_log, LOG_BYTES};
use acd_protocol::ErrorCode;
use serde_json::json;

#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for Buffer {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Every failed dispatch is classified, never read as "nothing happened"
/// unless T3 proved it, and the debug log carries tags and fingerprints, not
/// the server's text.
#[test]
fn failed_dispatches_classify_conservatively_and_never_log_server_text() {
    let buffer = Buffer::default();
    let writer = buffer.clone();
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish(),
    );
    tracing::callsite::rebuild_interest_cache();

    let secret = "SECRET-SERVER-TEXT-7f3a";
    let fail = |tag: &str| {
        Err(CallError::Failed(Failure::Fail { tag: tag.into(), message: secret.into() }))
    };
    let mixed = Failure::Mixed(vec![
        Cause::Fail { error: json!({"_tag": "SomeError", "message": secret}) },
        Cause::Die { defect: json!({"message": secret}) },
        Cause::Interrupt {},
        Cause::Other,
    ]);

    // Without a request id an untyped refusal stays uncertain.
    assert_eq!(classify(&fail("OrchestrationV2DispatchCommandError"), None), Outcome::Uncertain(REFUSED));
    assert_eq!(classify(&fail("SomethingElse"), Some("r1")), Outcome::Uncertain(REFUSED));
    // A dispatch error with unrecognised text is uncertain, not "gone".
    assert_eq!(
        classify(&fail("OrchestrationV2DispatchCommandError"), Some("r1")),
        Outcome::Uncertain(REFUSED)
    );
    assert_eq!(classify(&Err(CallError::Failed(Failure::Unknown)), None), Outcome::Uncertain(REFUSED));
    assert_eq!(classify(&Err(CallError::Failed(mixed)), None), Outcome::Uncertain(MIXED));
    assert_eq!(
        classify(&Err(CallError::Failed(Failure::Die(secret.into()))), None),
        Outcome::Uncertain(DEFECT)
    );
    assert_eq!(
        classify(&Err(CallError::Failed(Failure::Interrupted)), None),
        Outcome::Uncertain(INTERRUPTED)
    );
    assert_eq!(classify(&Err(CallError::NoReply("x")), None), Outcome::Uncertain(NO_REPLY));
    // Nothing was written: T3 cannot have seen it.
    for e in [CallError::NotConnected, CallError::NotWritten("x")] {
        assert!(matches!(
            classify(&Err(e), None),
            Outcome::NotDelivered { code: ErrorCode::SourceUnavailable, end: None, degrade: None }
        ));
    }
    assert_eq!(classify(&Ok(json!({})), None), Outcome::Delivered);

    let logged = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    assert!(logged.contains("T3 dispatch failed"), "the debug line was not captured: {logged}");
    assert!(!logged.contains(secret), "server text reached the log: {logged}");
}

#[test]
fn hook_payload_logs_redact_secrets_and_are_capped() {
    let raw = br#"{"authorization":"Bearer abcdef.ghi","note":"ok"}"#;
    let logged = payload_for_log(raw);
    assert!(!logged.contains("abcdef"), "{logged}");
    assert!(logged.contains("[REDACTED]"));
    // Invalid UTF-8 is lossy-decoded, not an error.
    assert!(payload_for_log(&[0xff, b'a']).contains('a'));
    let big = "x".repeat(LOG_BYTES * 3);
    assert!(redact_log(&big).len() <= LOG_BYTES + 64);
}

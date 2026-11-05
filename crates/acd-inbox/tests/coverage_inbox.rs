//! Coverage-driven tests (docs/research/2026-11-05-coverage.md). Each pins a
//! mapping that decides which error code a view sees, or which class a log
//! line carries, and that had no assertion on most of its branches.
use acd_client::inbox::{MachineError, PickError};
use acd_client::settings::SecretError;
use acd_inbox::commands::machine_error_code;
use acd_inbox::protocol::{pick_error_code, ErrorCode};
use acd_inbox::secrets::{BackendError, WorkerError};
use acd_inbox::server::CloseReason;

/// The reply carries a code only: every keyring failure kind maps to its own
/// code and an unknown one fails closed as `secret_store_failed`.
#[test]
fn keyring_failures_map_to_distinct_codes_and_unknown_kinds_fail_closed() {
    let read = |s: SecretError| machine_error_code(&MachineError::SecretRead(s));
    assert_eq!(read(SecretError::locked()), ErrorCode::SecretStoreLocked);
    assert_eq!(read(SecretError::timeout()), ErrorCode::SecretStoreTimeout);
    // An unconfirmed write or delete is reported as a timeout (D36).
    assert_eq!(
        machine_error_code(&MachineError::SecretWrite(SecretError::unknown())),
        ErrorCode::SecretStoreTimeout
    );
    assert_eq!(
        machine_error_code(&MachineError::SecretDelete(SecretError::unknown())),
        ErrorCode::SecretStoreTimeout
    );
    assert_eq!(
        read(SecretError("D-Bus said: host=secret.example".to_string())),
        ErrorCode::SecretStoreFailed
    );
}

#[test]
fn every_other_machine_error_has_its_own_code() {
    use MachineError::*;
    assert_eq!(machine_error_code(&UnknownMachine), ErrorCode::UnknownMachine);
    assert_eq!(machine_error_code(&NotPaired), ErrorCode::NotPaired);
    assert_eq!(
        machine_error_code(&Settings("path /home/x".into())),
        ErrorCode::SettingsFailed
    );
    assert_eq!(machine_error_code(&PairRecoveryFailed), ErrorCode::SecretStoreFailed);
    assert_eq!(machine_error_code(&RemoveRecoveryFailed), ErrorCode::SecretStoreFailed);
}

/// A card that changed under the user and a connection that changed both
/// read as stale; neither may become an answerable state.
#[test]
fn pick_errors_map_to_wire_codes() {
    use PickError::*;
    let cases = [
        (UnknownCard, "unknown_card"),
        (NotAnswerable, "not_answerable"),
        (StaleRequest, "stale"),
        (ConnectionChanged, "stale"),
        (UnknownOption, "unknown_option"),
        (AlreadyCounting, "already_counting"),
        (BadText, "bad_text"),
        (BadAnswerText, "bad_text"),
        (CannotReply, "cannot_reply"),
        (Hidden, "hidden"),
    ];
    for (e, want) in cases {
        assert_eq!(pick_error_code(&e).as_str(), want, "{e:?}");
    }
}

#[test]
fn every_error_code_has_a_distinct_snake_case_name() {
    let all = [
        ErrorCode::Stale, ErrorCode::UnknownCard, ErrorCode::NotAnswerable,
        ErrorCode::UnknownOption, ErrorCode::AlreadyCounting, ErrorCode::BadText,
        ErrorCode::CannotReply, ErrorCode::CannotMarkSeen, ErrorCode::Hidden,
        ErrorCode::UnknownMachine, ErrorCode::NotPaired, ErrorCode::BadName,
        ErrorCode::BadPairing, ErrorCode::BadRequest, ErrorCode::SecretStoreLocked,
        ErrorCode::SecretStoreTimeout, ErrorCode::SecretStoreFailed,
        ErrorCode::SettingsFailed, ErrorCode::DuplicateId, ErrorCode::Unsupported,
    ];
    let mut seen = std::collections::HashSet::new();
    for c in all {
        // The serde name is the wire name; the helper must agree with it.
        let wire = serde_json::to_string(&c).unwrap();
        assert_eq!(wire, format!("\"{}\"", c.as_str()));
        assert!(seen.insert(c.as_str()), "duplicate {}", c.as_str());
    }
}

#[test]
fn log_tags_are_fixed_classes_never_error_text() {
    assert_eq!(CloseReason::ReadError.tag(), "read_error");
    assert_eq!(CloseReason::Oversize.tag(), "oversize");
    assert_eq!(CloseReason::Malformed.tag(), "malformed");
    assert_eq!(CloseReason::WriteTimeout.tag(), "write_timeout");
    assert_eq!(CloseReason::WriteError.tag(), "write_error");
    assert_eq!(CloseReason::ReplyOverflow.tag(), "reply_overflow");
    assert_eq!(CloseReason::Shutdown.tag(), "shutdown");
    assert_eq!(CloseReason::Eof.tag(), "eof");
    assert_eq!(BackendError::Locked.tag(), "locked");
    assert_eq!(BackendError::Dismissed.tag(), "dismissed");
    assert_eq!(BackendError::Timeout.tag(), "timeout");
    assert_eq!(BackendError::Unavailable.tag(), "unavailable");
    assert_eq!(BackendError::Failed.tag(), "failed");
    assert_eq!(WorkerError::Backend(BackendError::Locked).tag(), "locked");
    assert_eq!(WorkerError::Expired.tag(), "expired");
    assert_eq!(WorkerError::Ambiguous.tag(), "ambiguous");
}

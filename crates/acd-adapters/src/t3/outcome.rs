//! The one place that decides what a dispatch result means (Global
//! Constraints). Server text never leaves this module except in a redacted
//! debug log.
use acd_protocol::{ErrorCode, ResolvedBy};
use serde_json::{Value, json};

use super::conn::CallError;
use super::wire::{Failure, failure_json};
use crate::text::redact_log;

pub const NO_REPLY: &str = "T3 did not confirm the command";
pub const DEFECT: &str = "T3 reported an internal error";
pub const INTERRUPTED: &str = "T3 interrupted the command";
pub const REFUSED: &str = "T3 refused the command";
pub const SCOPE: &str = "the T3 token lacks the orchestration:operate scope";
pub const MIXED: &str = "T3 reported several failures";

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Delivered,
    /// Nothing reached T3's orchestrator. `end` says how the request ended
    /// at the source when T3 said so; `degrade` is a source status message.
    NotDelivered {
        code: ErrorCode,
        end: Option<ResolvedBy>,
        degrade: Option<&'static str>,
    },
    Uncertain(&'static str),
}

const DISPATCH_ERROR: &str = "OrchestrationV2DispatchCommandError";
const SCOPE_ERROR: &str = "EnvironmentAuthorizationError";

/// The debug-log text for a failed call: JSON, redacted and capped.
fn debug_detail(e: &CallError) -> String {
    let raw = match e {
        CallError::Failed(f) => failure_json(f),
        other => json!({"error": other.to_string()}),
    };
    redact_log(&raw.to_string())
}

pub fn classify(res: &Result<Value, CallError>, t3_request_id: Option<&str>) -> Outcome {
    if let Err(e) = res {
        tracing::debug!(error = %debug_detail(e), "T3 dispatch failed");
    }
    let not = |code, end, degrade| Outcome::NotDelivered { code, end, degrade };
    match res {
        Ok(_) => Outcome::Delivered,
        Err(CallError::NotConnected) | Err(CallError::NotWritten(_)) => {
            not(ErrorCode::SourceUnavailable, None, None)
        }
        Err(CallError::Failed(Failure::Fail { tag, .. })) if tag == SCOPE_ERROR => {
            not(ErrorCode::SourceUnavailable, None, Some(SCOPE))
        }
        Err(CallError::Failed(Failure::Fail { tag, message })) if tag == DISPATCH_ERROR => {
            let Some(id) = t3_request_id else {
                return Outcome::Uncertain(REFUSED);
            };
            if *message == format!("Runtime request {id} is resolved.") {
                not(ErrorCode::RequestGone, Some(ResolvedBy::Terminal), None)
            } else if *message == format!("Runtime request {id} is cancelled.")
                || *message == format!("Runtime request {id} was not found.")
            {
                not(ErrorCode::RequestGone, Some(ResolvedBy::Lost), None)
            } else {
                Outcome::Uncertain(REFUSED)
            }
        }
        // A single Fail is summarized by its stable tag/classification; raw
        // message context is retained only in the redacted debug diagnostic.
        Err(CallError::Failed(Failure::Fail { .. })) | Err(CallError::Failed(Failure::Unknown)) => {
            Outcome::Uncertain(REFUSED)
        }
        // Several causes: never read as "nothing happened" (Sol round 2, finding 6).
        Err(CallError::Failed(Failure::Mixed(_))) => Outcome::Uncertain(MIXED),
        Err(CallError::Failed(Failure::Die(_))) => Outcome::Uncertain(DEFECT),
        Err(CallError::Failed(Failure::Interrupted)) => Outcome::Uncertain(INTERRUPTED),
        Err(CallError::NoReply(_)) => Outcome::Uncertain(NO_REPLY),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t3::{conn::CallError, fake::fixture, wire::*};
    use serde_json::json;

    fn failed(name: &str) -> Result<Value, CallError> {
        let f = fixture(name);
        let cause: Vec<Cause> = serde_json::from_value(f["exit"]["cause"].clone()).unwrap();
        Err(CallError::Failed(failure(&cause)))
    }
    fn id_in(name: &str) -> String {
        let m = fixture(name)["exit"]["cause"][0]["error"]["message"]
            .as_str()
            .unwrap()
            .to_owned();
        m.strip_prefix("Runtime request ")
            .unwrap()
            .rsplit_once(' ')
            .unwrap()
            .0
            .rsplit_once(' ')
            .unwrap()
            .0
            .to_owned()
    }

    #[test]
    fn known_refusals_are_not_delivered() {
        let id = id_in("cmd_exit_respond_fail_already_resolved.json");
        assert_eq!(
            classify(
                &failed("cmd_exit_respond_fail_already_resolved.json"),
                Some(&id)
            ),
            Outcome::NotDelivered {
                code: ErrorCode::RequestGone,
                end: Some(ResolvedBy::Terminal),
                degrade: None
            }
        );
        let id = id_in("cmd_exit_respond_fail_cancelled_after_restart.json");
        assert!(matches!(
            classify(
                &failed("cmd_exit_respond_fail_cancelled_after_restart.json"),
                Some(&id)
            ),
            Outcome::NotDelivered {
                code: ErrorCode::RequestGone,
                end: Some(ResolvedBy::Lost),
                ..
            }
        ));
        assert_eq!(
            classify(&failed("rpc_exit_fail_missing_scope_operate.json"), None),
            Outcome::NotDelivered {
                code: ErrorCode::SourceUnavailable,
                end: None,
                degrade: Some(SCOPE)
            }
        );
        let missing = Failure::Fail {
            tag: DISPATCH_ERROR.into(),
            message: "Runtime request rq-9 was not found.".into(),
        };
        assert_eq!(
            classify(&Err(CallError::Failed(missing)), Some("rq-9")),
            Outcome::NotDelivered {
                code: ErrorCode::RequestGone,
                end: Some(ResolvedBy::Lost),
                degrade: None
            }
        );
        assert!(matches!(
            classify(&Err(CallError::NotConnected), None),
            Outcome::NotDelivered { .. }
        ));
        assert!(matches!(
            classify(&Err(CallError::NotWritten("x")), None),
            Outcome::NotDelivered { .. }
        ));
    }

    #[test]
    fn everything_else_after_a_write_is_uncertain() {
        // "is resolved" about a different request id is not ours to interpret.
        assert_eq!(
            classify(
                &failed("cmd_exit_respond_fail_already_resolved.json"),
                Some("runtime-request:other")
            ),
            Outcome::Uncertain(REFUSED)
        );
        assert_eq!(
            classify(&failed("rpc_exit_die_bad_decision_literal.json"), None),
            Outcome::Uncertain(DEFECT)
        );
        assert_eq!(
            classify(&failed("rpc_exit_interrupted_stream.json"), None),
            Outcome::Uncertain(INTERRUPTED)
        );
        assert_eq!(
            classify(
                &failed("cmd_exit_respond_fail_previously_rejected.json"),
                Some("rq-1")
            ),
            Outcome::Uncertain(REFUSED)
        );
        assert_eq!(
            classify(&Err(CallError::NoReply("x")), None),
            Outcome::Uncertain(NO_REPLY)
        );
        assert_eq!(
            classify(&Ok(json!({"sequence": 3})), None),
            Outcome::Delivered
        );
    }

    #[test]
    fn multi_cause_failures_are_uncertain() {
        let id = "rq-1";
        let resolved = json!({"_tag": "Fail", "error": {"_tag": "OrchestrationV2DispatchCommandError",
            "message": format!("Runtime request {id} is resolved.")}});
        let scope = json!({"_tag": "Fail", "error": {"_tag": "EnvironmentAuthorizationError", "message": "m"}});
        let die = json!({"_tag": "Die", "defect": "d"});
        let int = json!({"_tag": "Interrupt", "fiberId": 1});
        for set in [
            vec![resolved.clone(), die.clone()],
            vec![die.clone(), resolved.clone()],
            vec![scope.clone(), die],
            vec![resolved.clone(), scope.clone()],
            vec![scope, int],
        ] {
            let causes: Vec<Cause> = serde_json::from_value(Value::Array(set.clone())).unwrap();
            assert_eq!(
                classify(&Err(CallError::Failed(failure(&causes))), Some(id)),
                Outcome::Uncertain(MIXED),
                "{set:?}"
            );
        }
        let one: Vec<Cause> = serde_json::from_value(json!([resolved])).unwrap();
        assert!(matches!(
            classify(&Err(CallError::Failed(failure(&one))), Some(id)),
            Outcome::NotDelivered { .. }
        ));
    }

    #[test]
    fn debug_detail_redacts_and_caps() {
        let secret = "sk-ant-api03-SECRETSECRET";
        let bearer = "Authorization: Bearer abc.def-ghi";
        for e in [
            CallError::Failed(Failure::Die(format!("boom {secret}"))),
            CallError::Failed(Failure::Fail {
                tag: "SomethingNew".into(),
                message: format!("{bearer} {secret}"),
            }),
        ] {
            let text = debug_detail(&e);
            for bad in ["SECRETSECRET", "abc.def-ghi"] {
                assert!(!text.contains(bad), "{text}");
            }
        }
        let long = CallError::Failed(Failure::Die("x".repeat(10_000)));
        assert!(debug_detail(&long).len() <= crate::text::LOG_BYTES + '…'.len_utf8());
        let formatted = debug_detail(&CallError::Failed(Failure::Fail {
            tag: "Other".into(),
            message: "TOKEN=\"first second\" WSTICKET=abc".into(),
        }));
        assert!(!formatted.contains("first second"), "{formatted}");
        assert!(!formatted.contains("WSTICKET=abc"), "{formatted}");
    }
}

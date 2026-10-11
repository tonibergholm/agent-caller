//! What a voice client needs from the inbox (voice spec §4.3, §5.6):
//! per-action outcomes, connection stamps, operation guards, and a view
//! published together with the stamps it was built from.
//!
//! Every action id gets at most one terminal outcome. `Fired` comes first
//! when the countdown ended; from then on `Inbox::cancel` returns false.
use std::collections::HashMap;

use acd_protocol::ErrorCode;

use crate::commands::{NotSent, Outcome};
use crate::view::{CardView, InboxView};

/// Why an action ended without being sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotSentReason {
    Offline,
    NotSupported,
    AgentGone,
    AnsweredElsewhere,
    Changed,
    AgentBusy,
    /// The user hid the card (subagents or idle agents turned off).
    Hidden,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionOutcome {
    Fired,
    Delivered,
    Rejected { code: ErrorCode },
    Uncertain,
    Cancelled,
    NotSent { reason: NotSentReason },
}

impl ActionOutcome {
    pub fn is_terminal(self) -> bool {
        self != ActionOutcome::Fired
    }

    pub(crate) fn from_submit(outcome: &Outcome) -> Self {
        match outcome {
            Outcome::Delivered | Outcome::Done => Self::Delivered,
            Outcome::Failed { code, .. } => Self::Rejected { code: *code },
            Outcome::Uncertain => Self::Uncertain,
            Outcome::NotSent(NotSent::Offline) => Self::NotSent { reason: NotSentReason::Offline },
            Outcome::NotSent(NotSent::NotAdvertised) => Self::NotSent { reason: NotSentReason::NotSupported },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionEvent {
    pub action_id: String,
    pub outcome: ActionOutcome,
}

/// A machine connection's identity: handle instance and connection
/// generation. Kept out of the view, which is a ts-rs binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnStamp {
    pub instance: u64,
    pub conn_gen: u64,
}

/// Everything an operation was planned on: the connection, the agent, and
/// its pending request (`None`: there was none).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Guard {
    pub conn: ConnStamp,
    pub agent_id: String,
    pub request: Option<(String, u64)>,
}

/// One rebuild's output: the view, the idle cards it hides, and the
/// connection stamps of the states it was built from.
#[derive(Clone, Debug, PartialEq)]
pub struct StampedView {
    pub view: InboxView,
    pub hidden_idle_cards: Vec<CardView>,
    pub stamps: HashMap<String, ConnStamp>,
}

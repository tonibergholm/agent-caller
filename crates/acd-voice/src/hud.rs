//! What the HUD shows (spec §4.4). The app serializes it to the HUD
//! webview inside a `HudEvent`; `app/src/lib/hud.ts` mirrors these shapes.
use acd_protocol::{Effect, ErrorCode};
use serde::Serialize;

use crate::plan::NotSentReason;
use crate::refusal::Refusal;
use crate::router::target::Provenance;
use crate::safety::Level;
use crate::text::VoiceText;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TargetView {
    pub card: String,
    pub machine: String,
    /// "Claude", "Codex" or "agent".
    pub agent: String,
    pub title: String,
    /// The request's spoken type tag, e.g. "bash approval".
    pub request: Option<String>,
    /// Not on screen (machine filter, collapsed Done, hidden idle): the
    /// HUD stresses the machine (spec §5.2, D7).
    pub off_screen: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalAction {
    Answer,
    AnswerText,
    Send,
}

/// Target, action and full payload (spec §6.1): nothing here is cut.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProposalView {
    pub action: ProposalAction,
    pub target: TargetView,
    pub option: Option<String>,
    pub effect: Option<Effect>,
    /// The whole persistent rule of an `allow_always` option.
    pub rule: Option<String>,
    /// The request's command or question.
    pub question: Option<String>,
    /// The text an `answer_text` or `send` delivers.
    pub text: Option<VoiceText>,
    pub provenance: Option<Provenance>,
    pub level: Level,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutcomeView {
    Delivered,
    Rejected { code: ErrorCode },
    Uncertain,
    Cancelled,
    NotSent { reason: NotSentReason },
    Selected,
}

// A short-lived UI value, built once per change and serialized; boxing the
// proposal would buy nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Hud {
    Hidden,
    /// Shown by the audio steps; step 1 has no microphone.
    Listening,
    Thinking { transcript: VoiceText },
    /// Waits for Enter, a click or "confirm" on this `proposal_id`, which
    /// the HUD must first acknowledge as shown (D22).
    Confirm { proposal: ProposalView, proposal_id: u64 },
    WhichOne { candidates: Vec<TargetView>, transcript: VoiceText },
    /// `action_id` is `None` until the HUD acknowledged `proposal_id` and
    /// the 2 s countdown started (D22).
    Countdown { proposal: ProposalView, proposal_id: u64, action_id: Option<String> },
    /// Fired; the result follows as `Done` whenever it arrives.
    Sending { proposal: ProposalView, too_late_to_cancel: bool },
    Done { proposal: Option<ProposalView>, outcome: OutcomeView },
    Error { refusal: Refusal, transcript: Option<VoiceText> },
    /// Step 1 shows the text; TTS comes in delivery step 4.
    Speaking { text: VoiceText },
}

impl Hud {
    /// The HUD takes key focus only while it waits for input (spec §4.4).
    pub fn wants_key(&self) -> bool {
        matches!(self, Hud::Confirm { .. } | Hud::WhichOne { .. } | Hud::Countdown { .. })
    }

    /// It fades on its own: 1.5 s after a step fires or ends, or after
    /// speech. A later outcome is shown again as `Done`.
    pub fn fades(&self) -> bool {
        matches!(self, Hud::Sending { .. } | Hud::Done { .. } | Hud::Error { .. } | Hud::Speaking { .. })
    }

    /// The proposal the HUD must acknowledge as shown (D22).
    pub fn awaiting_display(&self) -> Option<u64> {
        match self {
            Hud::Confirm { proposal_id, .. } | Hud::Countdown { proposal_id, action_id: None, .. } => Some(*proposal_id),
            _ => None,
        }
    }
}

/// The event payload. `seq` grows with every publish, so the HUD can drop
/// a state that arrives after a newer one.
#[derive(Debug, Serialize)]
pub struct HudEvent<'a> {
    pub seq: u64,
    pub hud: &'a Hud,
}

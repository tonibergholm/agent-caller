//! What the HUD shows (spec §4.4). The app serializes it to the HUD
//! webview inside a `HudEvent`; `app/src/lib/hud.ts` mirrors these shapes.
use acd_protocol::{Effect, ErrorCode};
use serde::Serialize;
use std::fmt;

use crate::plan::NotSentReason;
use crate::refusal::Refusal;
use crate::router::target::Provenance;
use crate::safety::Level;
use crate::text::{Redacted, VoiceText};

/// `Debug` shows the lengths of agent text only (spec §3.7).
#[derive(Clone, PartialEq, Eq, Serialize)]
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
    /// Set only when another card would show the same agent, machine and
    /// title: its project, or else its card key.
    pub disambiguator: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalAction {
    Answer,
    AnswerText,
    Send,
}

/// Target, action and full payload (spec §6.1): nothing here is cut.
/// `Debug` shows the lengths of agent and voice text only.
#[derive(Clone, PartialEq, Eq, Serialize)]
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

impl fmt::Debug for TargetView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TargetView")
            .field("card", &self.card)
            .field("machine", &Redacted(&self.machine))
            .field("agent", &self.agent)
            .field("title", &Redacted(&self.title))
            .field("request", &Redacted::opt(&self.request))
            .field("off_screen", &self.off_screen)
            .field("disambiguator", &Redacted::opt(&self.disambiguator))
            .finish()
    }
}

impl fmt::Debug for ProposalView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProposalView")
            .field("action", &self.action)
            .field("target", &self.target)
            .field("option", &Redacted::opt(&self.option))
            .field("effect", &self.effect)
            .field("rule", &Redacted::opt(&self.rule))
            .field("question", &Redacted::opt(&self.question))
            .field("text", &self.text)
            .field("provenance", &self.provenance)
            .field("level", &self.level)
            .finish()
    }
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
    Thinking { transcript: VoiceText, id: u64 },
    /// Waits for Enter, a click or "confirm" on this `proposal_id`, which
    /// the HUD must first acknowledge as shown (D22).
    Confirm { proposal: ProposalView, proposal_id: u64 },
    WhichOne { candidates: Vec<TargetView>, transcript: VoiceText, choice_id: u64 },
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
    /// Not while thinking: every utterance passes through it, and taking
    /// key for an instant would bounce focus away from the inbox.
    pub fn wants_key(&self) -> bool {
        matches!(self, Hud::Confirm { .. } | Hud::WhichOne { .. } | Hud::Countdown { .. })
    }

    /// It fades on its own: 1.5 s after a step fires or ends, or after
    /// speech. A later outcome is shown again as `Done`.
    pub fn fades(&self) -> bool {
        matches!(self, Hud::Sending { .. } | Hud::Done { .. } | Hud::Error { .. } | Hud::Speaking { .. })
    }

    /// The id the HUD sends back with Esc, a click or a digit, so an input
    /// meant for a replaced state is ignored. `None` where nothing can be
    /// cancelled or chosen.
    pub fn input_id(&self) -> Option<u64> {
        match self {
            Hud::Thinking { id, .. } => Some(*id),
            Hud::WhichOne { choice_id, .. } => Some(*choice_id),
            Hud::Confirm { proposal_id, .. } | Hud::Countdown { proposal_id, .. } => Some(*proposal_id),
            _ => None,
        }
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
    /// `hud.input_id()`: send it back with the input.
    pub id: Option<u64>,
    pub hud: &'a Hud,
}

impl<'a> HudEvent<'a> {
    pub fn new(seq: u64, hud: &'a Hud) -> Self {
        Self { seq, id: hud.input_id(), hud }
    }
}

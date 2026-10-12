//! Why a step is refused. The HUD shows a classified message for each;
//! no variant carries text, so a refusal can never leak a transcript.
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    /// No selected or only-pending card to answer.
    NothingToAnswer,
    /// The target has no request to read.
    NothingToRead,
    /// Nothing named, nothing selected.
    NoTarget,
    /// Something was named, but no eligible card matches it.
    NoSuchTarget,
    /// The request has no option with that effect or number.
    NoSuchOption,
    /// More than one option has that effect: say the option number (D11).
    AmbiguousOption,
    /// Read-only or non-answerable request, or no typed answers.
    ReadOnly,
    Offline,
    CannotReply,
    SubagentHidden,
    /// In a collapsed Done section or a hidden idle agent: only a named
    /// send reaches it (D7).
    OutOfView,
    /// Hidden by the machine filter and not named (spec §5.2).
    FilterHiddenNotNamed,
    /// The model's target is not the one Rust resolved (D10).
    TargetMismatch,
    /// Empty, too long, or has control, bidi or separator characters.
    BadText,
    /// The plan is malformed, invalid or uses unknown ids.
    DidNotUnderstand,
    ModelUnavailable,
    /// Timeout or error; the provider's text is never shown.
    ModelFailed,
    /// The card, agent, request or connection changed before enqueue.
    SnapshotChanged,
    AlreadyCounting,
    NothingToCancel,
    /// The voice step already fired; it cannot be undone.
    AlreadySent,
    /// Not listed in the confirmation table (spec §6.1 default-deny).
    NotPermitted,
    /// The HUD did not confirm it showed the step in time; nothing was
    /// sent (D22).
    DisplayFailed,
    /// The user hid the agent (subagents, hook-only or idle agents turned
    /// off) after the step was planned; nothing was sent.
    Hidden,
    /// A fired step still waits for its result; the new utterance is
    /// refused so that result is still shown (final review B4).
    StillSending,
    /// Microphone access is off for Agent Caller (System Settings).
    MicrophoneDenied,
    /// No microphone, or it went away during the recording.
    MicrophoneUnavailable,
    /// Speech Recognition is off for Agent Caller (step-2 plan S14).
    SpeechDenied,
    /// On-device speech recognition is not available, failed, or did not
    /// finish in time.
    SpeechUnavailable,
    /// The speech model for the language is not installed (plan S21).
    SpeechAssetsMissing,
    /// The recording reached 60 s and was discarded (plan S9).
    RecordingTooLong,
    /// The hold key stopped working: the system disabled the event tap.
    HoldKeyOff,
}

impl From<crate::listen::ListenError> for Refusal {
    fn from(e: crate::listen::ListenError) -> Self {
        use crate::listen::ListenError as E;
        match e {
            E::MicrophoneDenied => Refusal::MicrophoneDenied,
            E::MicrophoneUnavailable => Refusal::MicrophoneUnavailable,
            E::SpeechDenied => Refusal::SpeechDenied,
            E::AssetsMissing => Refusal::SpeechAssetsMissing,
            E::SpeechUnavailable | E::Failed => Refusal::SpeechUnavailable,
        }
    }
}

impl From<crate::listen::Unavailable> for Refusal {
    fn from(u: crate::listen::Unavailable) -> Self {
        match u {
            crate::listen::Unavailable::AssetsMissing => Refusal::SpeechAssetsMissing,
            _ => Refusal::SpeechUnavailable,
        }
    }
}

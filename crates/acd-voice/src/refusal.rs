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
}

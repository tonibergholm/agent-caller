use acd_protocol::{Agent, AnswerPath, ErrorCode, Request, Source};
use async_trait::async_trait;

use crate::registry::Answer;

#[derive(Debug, thiserror::Error)]
pub enum DeliverError {
    /// Nothing reached the agent. The request goes back to `open`, and the
    /// command result is never cached, whatever the code.
    #[error("not delivered: {0:?}")]
    NotDelivered(ErrorCode),
    /// Something may have reached the agent, but there is no confirmation.
    #[error("delivery uncertain: {0}")]
    Uncertain(String),
}

/// A source of agents that can also deliver answers and follow-ups.
///
/// # Contract
///
/// **Deadlines and cancellation.** `deliver`, `send` and `mark_seen` must
/// finish within 10 s. They must be cancellation-safe: the core drops the
/// future when the client token is rotated, and then treats a `deliver` in
/// flight as possibly delivered (the request resolves `uncertain`). Dropping
/// the future must never leave a half-sent answer that completes later.
/// A request that was fully written to the source before the future was
/// dropped may still take effect. That is why the core resolves a dropped
/// delivery `uncertain` and caches `delivered`. What must never happen is
/// a send that *starts* after the drop.
/// Lifecycle (non-held) hook-style work must reply within 1 s.
///
/// **Errors.** Return `NotDelivered` only when nothing reached the agent.
/// If anything may have reached it, return `Uncertain`.
///
/// **Attribution.** Adapters report how a request ended through
/// `Observation::RequestEnded`; the core does not relabel it.
/// - `Inbox`: the adapter delivered acd's answer and the observed outcome is
///   consistent with it. Another decider that produced the same outcome
///   cannot be told apart and is accepted as `Inbox`.
/// - `Terminal`: the outcome contradicts acd's answer, or the dialog ended
///   without any acd delivery.
/// - `Uncertain`: acd delivered, but the evidence is insufficient.
///
/// **Agent state.** After a resolution that changes the agent's state (for
/// example `needs_you` to `working`), emit an `Observation::AgentUpsert`.
/// The core clears `pending` but does not guess the next state.
#[async_trait]
pub trait Adapter: Send + Sync {
    fn name(&self) -> &'static str;
    /// Answer paths this adapter delivers (routing for `request.answer`).
    fn answer_paths(&self) -> &[AnswerPath];
    /// Agent sources this adapter owns (routing for `agent.send` and `agent.mark_seen`).
    fn sources(&self) -> &[Source];
    async fn deliver(&self, request: &Request, answer: &Answer) -> Result<(), DeliverError>;
    async fn send(&self, agent: &Agent, text: &str) -> Result<(), DeliverError>;
    async fn mark_seen(&self, _agent: &Agent) -> Result<(), DeliverError> { Ok(()) }
}

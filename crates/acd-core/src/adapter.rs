use acd_protocol::{Agent, AnswerPath, ErrorCode, Request, Source};
use async_trait::async_trait;

use crate::registry::Answer;

#[derive(Debug, thiserror::Error)]
pub enum DeliverError {
    /// Nothing reached the agent. The request goes back to `open`.
    #[error("not delivered: {0:?}")]
    NotDelivered(ErrorCode),
    /// Something may have reached the agent, but there is no confirmation.
    #[error("delivery uncertain: {0}")]
    Uncertain(String),
}

/// A source of agents that can also deliver answers and follow-ups.
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

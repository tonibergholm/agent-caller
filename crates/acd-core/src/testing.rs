//! Test helpers. Enabled by the `testing` feature.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use acd_protocol::*;
use async_trait::async_trait;
use tokio::sync::Notify;

use crate::adapter::{Adapter, DeliverError};
use crate::registry::{AgentObs, Answer, CorrelationKey, RequestObs};

pub struct FakeAdapter {
    delivered: Mutex<Vec<(String, Answer)>>,
    sent: Mutex<Vec<(String, String)>>,
    next_error: Mutex<Option<DeliverError>>,
    gate: Option<Arc<Notify>>,
    panic_next: AtomicBool,
    panic_after_deliver: AtomicBool,
}

impl FakeAdapter {
    pub fn new() -> Arc<Self> { Arc::new(Self::build(None)) }
    /// `deliver` waits until `open_gate` is called.
    pub fn gated() -> Arc<Self> { Arc::new(Self::build(Some(Arc::new(Notify::new())))) }
    fn build(gate: Option<Arc<Notify>>) -> Self {
        Self { delivered: Mutex::default(), sent: Mutex::default(), next_error: Mutex::default(), gate, panic_next: AtomicBool::new(false), panic_after_deliver: AtomicBool::new(false) }
    }
    pub fn open_gate(&self) { if let Some(g) = &self.gate { g.notify_waiters(); g.notify_one(); } }
    pub fn fail_next(&self, e: DeliverError) { *self.next_error.lock().unwrap() = Some(e); }
    /// The next routing lookup (`answer_paths`) panics, i.e. after the claim but before `deliver` starts.
    pub fn panic_next(&self) { self.panic_next.store(true, Ordering::SeqCst); }
    /// The next `deliver` call records the delivery, then panics.
    pub fn panic_after_deliver_next(&self) { self.panic_after_deliver.store(true, Ordering::SeqCst); }
    pub fn delivered(&self) -> Vec<(String, Answer)> { self.delivered.lock().unwrap().clone() }
    pub fn sent(&self) -> Vec<(String, String)> { self.sent.lock().unwrap().clone() }
}

#[async_trait]
impl Adapter for FakeAdapter {
    fn name(&self) -> &'static str { "fake" }
    fn answer_paths(&self) -> &[AnswerPath] {
        if self.panic_next.swap(false, Ordering::SeqCst) { panic!("fake adapter panic before deliver"); }
        &[AnswerPath::Hook, AnswerPath::Keys, AnswerPath::T3]
    }
    fn sources(&self) -> &[Source] { &[Source::Herdr, Source::T3, Source::Hooks] }
    async fn deliver(&self, request: &Request, answer: &Answer) -> Result<(), DeliverError> {
        if let Some(g) = &self.gate { g.notified().await; }
        if let Some(e) = self.next_error.lock().unwrap().take() { return Err(e); }
        self.delivered.lock().unwrap().push((request.id.clone(), answer.clone()));
        if self.panic_after_deliver.swap(false, Ordering::SeqCst) { panic!("fake adapter panic after deliver"); }
        Ok(())
    }
    async fn send(&self, agent: &Agent, text: &str) -> Result<(), DeliverError> {
        if let Some(e) = self.next_error.lock().unwrap().take() { return Err(e); }
        self.sent.lock().unwrap().push((agent.id.clone(), text.to_string()));
        Ok(())
    }
}

pub fn test_machine() -> Machine {
    Machine {
        id: "test-machine".into(), name: "Test".into(), acd_version: "0.0.0".into(),
        sources: Sources { hooks: SourceStatus::unconfigured(), herdr: SourceStatus::unconfigured(), t3: SourceStatus::unconfigured() },
    }
}

pub fn agent_obs(id: &str) -> AgentObs {
    AgentObs {
        id: id.into(), source: Source::Herdr, kind: AgentKind::Claude, title: "agent".into(),
        cwd: None, project: None, state: AgentState::Working, failed: false, last_message: None, can_send: true,
    }
}

pub fn permission_obs(agent_id: &str, key: &str) -> RequestObs {
    RequestObs {
        agent_id: agent_id.into(), key: CorrelationKey(key.into()), upgrade_from: None,
        kind: RequestKind::Permission, answer_path: AnswerPath::Hook, title: "Bash".into(), detail: "ls".into(),
        options: vec![
            RequestOption { id: "allow".into(), label: "Yes".into(), description: None, effect: Effect::Allow },
            RequestOption { id: "deny".into(), label: "No".into(), description: None, effect: Effect::Deny },
        ],
        questions: None, free_text: false, raw_screen: None, ready: true,
    }
}

//! Answer targets: what the adapter needs to answer a surfaced T3 request.
use std::collections::HashMap;
use std::sync::Mutex;

use acd_core::{Core, CorrelationKey};
use acd_protocol::Request;

use super::map::Target;

#[derive(Default)]
pub struct Targets(Mutex<HashMap<String, Target>>);

impl Targets {
    pub fn insert(&self, t: Target) {
        self.0
            .lock()
            .expect("targets")
            .insert(t.t3_request_id.clone(), t);
    }

    pub fn remove(&self, t3_request_id: &str) {
        self.0.lock().expect("targets").remove(t3_request_id);
    }

    pub fn retain_thread(&self, thread_id: &str, keep: Option<&str>) {
        self.0
            .lock()
            .expect("targets")
            .retain(|id, t| t.thread_id != thread_id || Some(id.as_str()) == keep);
    }

    pub fn len(&self) -> usize {
        self.0.lock().expect("targets").len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.lock().expect("targets").is_empty()
    }

    /// The target bound to exactly this acd request (never matched by text).
    pub fn find(&self, core: &Core, request: &Request) -> Option<Target> {
        let candidates: Vec<Target> = self
            .0
            .lock()
            .expect("targets")
            .values()
            .filter(|t| t.agent_id == request.agent_id)
            .cloned()
            .collect();
        candidates.into_iter().find(|t| {
            core.request_id_for(&t.agent_id, &CorrelationKey(t.t3_request_id.clone()))
                .as_deref()
                == Some(request.id.as_str())
        })
    }
}

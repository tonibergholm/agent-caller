//! Provider session id -> T3 agent id, for spec §6.2 ownership rule 1 (D11).
use std::collections::HashMap;
use std::sync::RwLock;

use tokio::sync::watch;

pub struct SessionMap {
    by_session: RwLock<HashMap<String, String>>,
    by_agent: RwLock<HashMap<String, String>>,
    tx: watch::Sender<u64>,
}

impl Default for SessionMap {
    fn default() -> Self {
        SessionMap {
            by_session: Default::default(),
            by_agent: Default::default(),
            tx: watch::channel(0).0,
        }
    }
}

impl SessionMap {
    pub fn owner(&self, session_id: &str) -> Option<String> {
        self.by_session
            .read()
            .expect("sessions")
            .get(session_id)
            .cloned()
    }

    pub fn changes(&self) -> watch::Receiver<u64> {
        self.tx.subscribe()
    }

    /// Replaces `agent_id`'s entry; `None` removes it.
    pub(crate) fn set(&self, agent_id: &str, session_id: Option<&str>) {
        let changed = {
            let mut by_agent = self.by_agent.write().expect("sessions");
            let mut by_session = self.by_session.write().expect("sessions");
            let old = by_agent.get(agent_id).cloned();
            if old.as_deref() == session_id {
                false
            } else {
                if let Some(o) = old {
                    // Only this agent's own claim: another agent may have
                    // published the same session id since.
                    if by_session.get(&o).map(String::as_str) == Some(agent_id) {
                        by_session.remove(&o);
                    }
                    by_agent.remove(agent_id);
                }
                if let Some(s) = session_id {
                    by_session.insert(s.to_owned(), agent_id.to_owned());
                    by_agent.insert(agent_id.to_owned(), s.to_owned());
                }
                true
            }
        };
        if changed {
            self.tx.send_modify(|n| *n += 1);
        }
    }

    pub(crate) fn remove_agent(&self, agent_id: &str) {
        self.set(agent_id, None)
    }
}

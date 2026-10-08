//! Provider session id -> T3 agent id, for spec §6.2 ownership rule 1 (D11).
//!
//! Two threads can report the same native session id (a fork, say). The
//! first claim wins and is never overwritten; when the owner's claim goes,
//! the oldest remaining claimant becomes the owner.
use std::collections::HashMap;
use std::sync::RwLock;

use tokio::sync::watch;

use super::wire::string_fingerprint;

#[derive(Default)]
struct Claims {
    /// Agent id -> the session id it claims.
    by_agent: HashMap<String, String>,
    /// Session id -> claimants, oldest first. The first one owns it.
    by_session: HashMap<String, Vec<String>>,
}

pub struct SessionMap {
    claims: RwLock<Claims>,
    tx: watch::Sender<u64>,
}

impl Default for SessionMap {
    fn default() -> Self {
        SessionMap {
            claims: RwLock::default(),
            tx: watch::channel(0).0,
        }
    }
}

impl SessionMap {
    pub fn owner(&self, session_id: &str) -> Option<String> {
        self.claims
            .read()
            .expect("sessions")
            .by_session
            .get(session_id)
            .and_then(|c| c.first())
            .cloned()
    }

    pub fn changes(&self) -> watch::Receiver<u64> {
        self.tx.subscribe()
    }

    /// Replaces `agent_id`'s claim; `None` removes it.
    pub(crate) fn set(&self, agent_id: &str, session_id: Option<&str>) {
        let changed = {
            let mut claims = self.claims.write().expect("sessions");
            let Claims {
                by_agent,
                by_session,
            } = &mut *claims;
            let old = by_agent.get(agent_id).cloned();
            if old.as_deref() == session_id {
                false
            } else {
                if let Some(o) = old {
                    by_agent.remove(agent_id);
                    if let Some(c) = by_session.get_mut(&o) {
                        c.retain(|a| a != agent_id);
                        if c.is_empty() {
                            by_session.remove(&o);
                        }
                    }
                }
                if let Some(s) = session_id {
                    by_agent.insert(agent_id.to_owned(), s.to_owned());
                    let c = by_session.entry(s.to_owned()).or_default();
                    if let Some(first) = c.first() {
                        tracing::debug!(
                            owner = %string_fingerprint(first), claimant = %string_fingerprint(agent_id),
                            "T3 session id claimed by two threads; the first keeps it"
                        );
                    }
                    c.push(agent_id.to_owned());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_claim_wins_and_removal_hands_over() {
        let m = SessionMap::default();
        m.set("t3:a", Some("s"));
        m.set("t3:b", Some("s"));
        assert_eq!(m.owner("s").as_deref(), Some("t3:a"), "no silent hijack");
        m.set("t3:b", None);
        assert_eq!(m.owner("s").as_deref(), Some("t3:a"), "b clearing keeps a");
        m.set("t3:b", Some("s"));
        m.remove_agent("t3:a");
        assert_eq!(
            m.owner("s").as_deref(),
            Some("t3:b"),
            "a's removal hands over"
        );
        m.set("t3:b", Some("s2"));
        assert_eq!(m.owner("s"), None);
        assert_eq!(m.owner("s2").as_deref(), Some("t3:b"));
    }

    #[test]
    fn every_change_ticks() {
        let m = SessionMap::default();
        let mut rx = m.changes();
        m.set("t3:a", Some("s"));
        assert!(rx.has_changed().unwrap());
        rx.borrow_and_update();
        m.set("t3:a", Some("s"));
        assert!(!rx.has_changed().unwrap(), "no change, no tick");
    }
}

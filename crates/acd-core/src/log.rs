use std::collections::VecDeque;

use acd_protocol::{Cursor, ServerMessage};

use crate::registry::Change;

/// Assigns `(epoch, seq)` to changes and keeps the last `capacity` events
/// for resume (spec §7.4).
pub struct EventLog {
    epoch: String,
    seq: u64,
    ring: VecDeque<ServerMessage>,
    capacity: usize,
}

impl EventLog {
    pub fn new(epoch: String, capacity: usize) -> Self {
        assert!(capacity > 0, "EventLog capacity must be positive");
        Self { epoch, seq: 0, ring: VecDeque::with_capacity(capacity), capacity }
    }

    pub fn epoch(&self) -> &str { &self.epoch }
    pub fn seq(&self) -> u64 { self.seq }

    pub fn record(&mut self, change: Change) -> ServerMessage {
        self.seq += 1;
        let (epoch, seq) = (self.epoch.clone(), self.seq);
        let msg = match change {
            Change::AgentUpdated(agent) => ServerMessage::AgentUpdated { epoch, seq, agent },
            Change::AgentRemoved(agent_id) => ServerMessage::AgentRemoved { epoch, seq, agent_id },
            Change::RequestOpened(request) => ServerMessage::RequestOpened { epoch, seq, request },
            Change::RequestResolved { request_id, agent_id, by, option_id, reason, message } => {
                ServerMessage::RequestResolved { epoch, seq, request_id, agent_id, by, option_id, reason, message }
            }
            Change::MachineUpdated(machine) => ServerMessage::MachineUpdated { epoch, seq, machine },
        };
        if self.ring.len() == self.capacity { self.ring.pop_front(); }
        self.ring.push_back(msg.clone());
        msg
    }

    /// The events after `cursor`, or `None` when the client must take a snapshot.
    pub fn replay_after(&self, cursor: &Cursor) -> Option<Vec<ServerMessage>> {
        if cursor.epoch != self.epoch || cursor.seq > self.seq { return None; }
        let oldest = self.ring.front().and_then(ServerMessage::seq).unwrap_or(self.seq + 1);
        if cursor.seq + 1 < oldest { return None; }
        Some(self.ring.iter().filter(|m| m.seq().is_some_and(|s| s > cursor.seq)).cloned().collect())
    }
}

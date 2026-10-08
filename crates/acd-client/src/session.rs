//! One connection's model of a daemon (spec §7.4). Pure: no I/O, no clock.
use std::collections::BTreeMap;

use acd_protocol::{Agent, Cursor, Machine, Request, ResolvedBy, ServerMessage};

#[derive(Clone, Debug, Default)]
pub struct Session {
    pub machine: Option<Machine>,
    pub capabilities: Vec<String>,
    pub agents: BTreeMap<String, Agent>,
    cursor: Option<Cursor>,
    known_epoch: Option<String>,
    welcome_epoch: Option<String>,
    awaiting_snapshot: bool,
    watermark: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EpochChange {
    First,
    Same,
    New,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Applied {
    Ignored,
    Changed,
    Opened(Request),
    Resolved {
        request_id: String,
        agent_id: String,
        by: ResolvedBy,
        option_id: Option<String>,
    },
    Resync(&'static str),
}

fn event_pos(msg: &ServerMessage) -> Option<(&str, u64)> {
    match msg {
        ServerMessage::AgentUpdated { epoch, seq, .. }
        | ServerMessage::AgentRemoved { epoch, seq, .. }
        | ServerMessage::RequestOpened { epoch, seq, .. }
        | ServerMessage::RequestResolved { epoch, seq, .. }
        | ServerMessage::MachineUpdated { epoch, seq, .. } => Some((epoch, *seq)),
        _ => None,
    }
}

impl Session {
    pub fn cursor(&self) -> Option<&Cursor> {
        self.cursor.as_ref()
    }

    pub fn resume(&self) -> Option<Cursor> {
        self.cursor.clone()
    }

    pub fn has(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|c| c == capability)
    }

    pub fn synced(&self) -> bool {
        self.cursor.is_some() && !self.awaiting_snapshot
    }

    pub fn ready(&self) -> bool {
        self.synced()
            && self
                .cursor
                .as_ref()
                .is_some_and(|cursor| cursor.seq >= self.watermark)
    }

    pub fn welcome(
        &mut self,
        machine: Machine,
        capabilities: Vec<String>,
        epoch: &str,
        seq: u64,
    ) -> EpochChange {
        self.watermark = seq;
        let change = match self.known_epoch.as_deref() {
            None => EpochChange::First,
            Some(known_epoch) if known_epoch == epoch => EpochChange::Same,
            Some(_) => EpochChange::New,
        };
        self.machine = Some(machine);
        self.capabilities = capabilities;
        self.known_epoch = Some(epoch.to_owned());
        self.welcome_epoch = Some(epoch.to_owned());
        if self
            .cursor
            .as_ref()
            .is_some_and(|cursor| cursor.epoch != epoch)
        {
            self.cursor = None;
        }
        self.awaiting_snapshot = self.cursor.is_none();
        change
    }

    pub fn force_resync(&mut self) {
        self.cursor = None;
        self.awaiting_snapshot = true;
    }

    pub fn apply(&mut self, msg: &ServerMessage) -> Applied {
        if let ServerMessage::Snapshot { epoch, seq, agents } = msg {
            if self.welcome_epoch.as_deref() != Some(epoch.as_str()) {
                return Applied::Resync("snapshot from another epoch");
            }
            self.agents = agents
                .iter()
                .map(|agent| (agent.id.clone(), agent.clone()))
                .collect();
            self.cursor = Some(Cursor {
                epoch: epoch.clone(),
                seq: *seq,
            });
            self.awaiting_snapshot = false;
            return Applied::Changed;
        }

        let Some((epoch, seq)) = event_pos(msg) else {
            return Applied::Ignored;
        };
        if self.awaiting_snapshot {
            return Applied::Resync("event before snapshot");
        }
        let Some(cursor) = self.cursor.as_mut() else {
            return Applied::Resync("event before snapshot");
        };
        if cursor.epoch != epoch {
            return Applied::Resync("event from another epoch");
        }
        if seq <= cursor.seq {
            return Applied::Ignored;
        }
        if cursor.seq.checked_add(1) != Some(seq) {
            return Applied::Resync("sequence gap");
        }
        cursor.seq = seq;

        match msg {
            ServerMessage::AgentUpdated { agent, .. } => {
                self.agents.insert(agent.id.clone(), agent.clone());
                Applied::Changed
            }
            ServerMessage::AgentRemoved { agent_id, .. } => {
                self.agents.remove(agent_id);
                Applied::Changed
            }
            ServerMessage::MachineUpdated { machine, .. } => {
                self.machine = Some(machine.clone());
                Applied::Changed
            }
            ServerMessage::RequestOpened { request, .. } => Applied::Opened(request.clone()),
            ServerMessage::RequestResolved {
                request_id,
                agent_id,
                by,
                option_id,
                ..
            } => Applied::Resolved {
                request_id: request_id.clone(),
                agent_id: agent_id.clone(),
                by: *by,
                option_id: option_id.clone(),
            },
            _ => Applied::Ignored,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acd_protocol::*;

    fn machine() -> Machine {
        Machine {
            id: "m".into(),
            name: "M".into(),
            acd_version: "0".into(),
            sources: Sources {
                hooks: SourceStatus::unconfigured(),
                herdr: SourceStatus::unconfigured(),
                t3: SourceStatus::unconfigured(),
            },
        }
    }

    fn agent(id: &str) -> Agent {
        serde_json::from_value(serde_json::json!({"id": id, "machine_id": "m", "source": "herdr", "kind": "claude",
            "title": "t", "cwd": null, "project": null, "state": "working", "state_since": "2026-10-08T00:00:00.000Z",
            "failed": false, "last_message": null, "pending": null, "can_send": true})).unwrap()
    }

    fn snap(epoch: &str, seq: u64, ids: &[&str]) -> ServerMessage {
        ServerMessage::Snapshot {
            epoch: epoch.into(),
            seq,
            agents: ids.iter().map(|id| agent(id)).collect(),
        }
    }

    fn upd(epoch: &str, seq: u64, id: &str) -> ServerMessage {
        ServerMessage::AgentUpdated {
            epoch: epoch.into(),
            seq,
            agent: agent(id),
        }
    }

    #[test]
    fn first_connect_waits_for_snapshot_then_applies_in_order() {
        let mut s = Session::default();
        assert_eq!(
            s.welcome(machine(), vec!["answer".into()], "e1", 0),
            EpochChange::First
        );
        assert!(s.has("answer") && !s.has("send"));
        assert_eq!(
            s.apply(&upd("e1", 1, "a")),
            Applied::Resync("event before snapshot")
        );
        let mut s = Session::default();
        s.welcome(machine(), vec![], "e1", 0);
        assert_eq!(s.apply(&snap("e1", 5, &["a"])), Applied::Changed);
        assert!(s.synced());
        assert_eq!(s.apply(&upd("e1", 6, "b")), Applied::Changed);
        assert_eq!(
            s.apply(&upd("e1", 6, "b")),
            Applied::Ignored,
            "duplicate seq"
        );
        assert_eq!(
            s.resume(),
            Some(Cursor {
                epoch: "e1".into(),
                seq: 6
            })
        );
        assert_eq!(s.agents.len(), 2);
    }

    #[test]
    fn gaps_and_foreign_epochs_force_resync() {
        let mut s = Session::default();
        s.welcome(machine(), vec![], "e1", 0);
        s.apply(&snap("e1", 5, &[]));
        assert_eq!(s.apply(&upd("e1", 7, "a")), Applied::Resync("sequence gap"));
        assert!(s.cursor().is_some_and(|c| c.seq == 5));
        assert!(s.agents.is_empty());
        let mut s = Session::default();
        s.welcome(machine(), vec![], "e1", 0);
        s.apply(&snap("e1", 5, &[]));
        assert_eq!(
            s.apply(&upd("e2", 6, "a")),
            Applied::Resync("event from another epoch")
        );
        assert!(s.cursor().is_some_and(|c| c.epoch == "e1" && c.seq == 5));
        assert!(s.agents.is_empty());
        let mut s = Session::default();
        s.welcome(machine(), vec![], "e1", 0);
        assert_eq!(
            s.apply(&snap("e0", 5, &[])),
            Applied::Resync("snapshot from another epoch")
        );
        assert_eq!(s.resume(), None);
        assert!(s.agents.is_empty());
    }

    #[test]
    fn same_epoch_resume_keeps_agents_new_epoch_waits_for_snapshot() {
        let mut s = Session::default();
        s.welcome(machine(), vec![], "e1", 0);
        s.apply(&snap("e1", 5, &["a"]));
        assert_eq!(s.welcome(machine(), vec![], "e1", 0), EpochChange::Same);
        assert!(s.synced(), "resume: replayed events apply directly");
        assert_eq!(s.apply(&upd("e1", 6, "b")), Applied::Changed);
        assert_eq!(s.welcome(machine(), vec![], "e2", 0), EpochChange::New);
        assert_eq!(s.resume(), None);
        assert_eq!(
            s.agents.len(),
            2,
            "stale agents stay visible until the snapshot"
        );
        assert_eq!(
            s.apply(&upd("e2", 1, "c")),
            Applied::Resync("event before snapshot")
        );
        s.welcome(machine(), vec![], "e2", 0);
        assert_eq!(s.apply(&snap("e2", 3, &["z"])), Applied::Changed);
        assert_eq!(s.agents.keys().collect::<Vec<_>>(), ["z"]);
    }

    #[test]
    fn ready_waits_for_watermark() {
        let mut s = Session::default();
        s.welcome(machine(), vec![], "e1", 7);
        assert!(!s.ready(), "authenticated is not ready");
        s.apply(&snap("e1", 7, &["a"]));
        assert!(s.ready());
        s.welcome(machine(), vec![], "e1", 9);
        assert!(!s.ready(), "cursor 7 < watermark 9");
        s.apply(&upd("e1", 8, "b"));
        assert!(!s.ready());
        s.apply(&upd("e1", 9, "c"));
        assert!(s.ready());
        s.welcome(machine(), vec![], "e1", 9);
        assert!(s.ready(), "nothing to replay");
        s.welcome(machine(), vec![], "e2", 3);
        assert!(!s.ready());
        s.apply(&snap("e2", 3, &[]));
        assert!(s.ready());
    }

    #[test]
    fn resync_in_the_same_epoch_is_not_a_new_epoch() {
        let mut s = Session::default();
        s.welcome(machine(), vec![], "e1", 0);
        s.apply(&snap("e1", 5, &[]));
        s.force_resync();
        assert_eq!(s.resume(), None);
        assert_eq!(s.welcome(machine(), vec![], "e1", 0), EpochChange::Same);
        assert!(!s.synced(), "no cursor, so a snapshot is needed");
    }

    #[test]
    fn opened_resolved_removed_and_machine_updates() {
        let mut s = Session::default();
        s.welcome(machine(), vec![], "e1", 0);
        s.apply(&snap("e1", 1, &["a"]));
        let req: Request = serde_json::from_value(serde_json::json!({"id":"r1","agent_id":"a","kind":"permission",
            "status":"open","answer_path":"hook","title":"Bash","detail":"ls","options":[],"questions":null,
            "free_text":false,"raw_screen":null,"opened_at":"2026-10-08T00:00:00.000Z"})).unwrap();
        assert_eq!(
            s.apply(&ServerMessage::RequestOpened {
                epoch: "e1".into(),
                seq: 2,
                request: req.clone()
            }),
            Applied::Opened(req)
        );
        assert_eq!(
            s.apply(&ServerMessage::RequestResolved {
                epoch: "e1".into(),
                seq: 3,
                request_id: "r1".into(),
                agent_id: "a".into(),
                by: ResolvedBy::Terminal,
                option_id: None
            }),
            Applied::Resolved {
                request_id: "r1".into(),
                agent_id: "a".into(),
                by: ResolvedBy::Terminal,
                option_id: None
            }
        );
        let mut m2 = machine();
        m2.name = "Renamed".into();
        assert_eq!(
            s.apply(&ServerMessage::MachineUpdated {
                epoch: "e1".into(),
                seq: 4,
                machine: m2
            }),
            Applied::Changed
        );
        assert_eq!(s.machine.as_ref().unwrap().name, "Renamed");
        assert_eq!(
            s.apply(&ServerMessage::AgentRemoved {
                epoch: "e1".into(),
                seq: 5,
                agent_id: "a".into()
            }),
            Applied::Changed
        );
        assert!(s.agents.is_empty());
        assert_eq!(
            s.apply(&ServerMessage::Pong { id: "p".into() }),
            Applied::Ignored
        );
    }
}

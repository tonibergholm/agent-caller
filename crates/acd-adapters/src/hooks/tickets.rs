//! Read tickets bind asynchronous screen reads to the state that authorized them.
use std::collections::BTreeMap;

use acd_protocol::{AnswerPath, RequestStatus};
use tokio::task::AbortHandle;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ReadKind {
    Capture,
    Presence,
    HoldEnded,
    #[allow(dead_code)]
    CodexBind,
}

#[derive(Debug, Default)]
struct Slot {
    seq: u64,
    task: Option<AbortHandle>,
    wanted: bool,
}

#[derive(Debug, Default)]
pub(crate) struct ReadSlots(BTreeMap<ReadKind, Slot>);

impl ReadSlots {
    pub fn admit(&mut self, kind: ReadKind, admitted: bool) -> Option<u64> {
        let slot = self.0.entry(kind).or_default();
        if !admitted {
            slot.wanted = true;
            return None;
        }
        if let Some(task) = slot.task.take() {
            task.abort();
        }
        slot.wanted = false;
        slot.seq += 1;
        Some(slot.seq)
    }
    pub fn started(&mut self, kind: ReadKind, task: AbortHandle) {
        self.0.entry(kind).or_default().task = Some(task);
    }
    pub fn finished(&mut self, kind: ReadKind, seq: u64) {
        if let Some(slot) = self.0.get_mut(&kind)
            && slot.seq == seq
        {
            slot.task = None;
        }
    }
    pub fn latest(&self, kind: ReadKind) -> u64 {
        self.0.get(&kind).map_or(0, |slot| slot.seq)
    }
    #[allow(dead_code)] // Used by later Codex/watch task paths.
    pub fn busy(&self, kind: ReadKind) -> bool {
        self.0.get(&kind).is_some_and(|slot| slot.task.is_some())
    }
    pub fn wanted(&self) -> Vec<ReadKind> {
        self.0
            .iter()
            .filter(|(_, slot)| slot.wanted)
            .map(|(kind, _)| *kind)
            .collect()
    }
    pub fn unwant(&mut self, kind: ReadKind) {
        if let Some(slot) = self.0.get_mut(&kind) {
            slot.wanted = false;
        }
    }
    pub fn abort_all(&mut self) {
        for slot in self.0.values_mut() {
            if let Some(task) = slot.task.take() {
                task.abort();
            }
            slot.wanted = false;
        }
    }
}

impl Drop for ReadSlots {
    fn drop(&mut self) {
        self.abort_all();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Ticket {
    pub agent_id: String,
    pub incarnation: u64,
    pub session_id: String,
    pub bind_gen: u64,
    pub request_id: Option<String>,
    pub op: u64,
    pub kind: ReadKind,
    pub read_seq: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Now {
    pub incarnation: Option<u64>,
    pub bind_gen: Option<u64>,
    pub t3_owned: bool,
    pub track: Option<(u64, u64)>,
    pub request: Option<(RequestStatus, AnswerPath)>,
}

pub(crate) fn still_valid(ticket: &Ticket, now: &Now, paths: &[AnswerPath]) -> bool {
    let common = now.incarnation == Some(ticket.incarnation)
        && now.bind_gen == Some(ticket.bind_gen)
        && !now.t3_owned;
    match &ticket.request_id {
        None => common && now.track == Some((ticket.op, ticket.read_seq)),
        Some(_) => {
            common
                && now.track == Some((ticket.op, ticket.read_seq))
                && now.request.is_some_and(|(status, path)| {
                    status == RequestStatus::Open && paths.contains(&path)
                })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticket() -> Ticket {
        Ticket {
            agent_id: "herdr:h:w1:p1".into(),
            incarnation: 7,
            session_id: "s".into(),
            bind_gen: 3,
            request_id: Some("r1".into()),
            op: 11,
            kind: ReadKind::Capture,
            read_seq: 2,
        }
    }
    fn now() -> Now {
        Now {
            incarnation: Some(7),
            bind_gen: Some(3),
            t3_owned: false,
            track: Some((11, 2)),
            request: Some((RequestStatus::Open, AnswerPath::Hook)),
        }
    }
    const PATHS: &[AnswerPath] = &[AnswerPath::Hook];

    #[test]
    fn current_ticket_is_valid_and_state_changes_invalidate_it() {
        assert!(still_valid(&ticket(), &now(), PATHS));
        for changed in [
            Now {
                incarnation: Some(8),
                ..now()
            },
            Now {
                incarnation: None,
                ..now()
            },
            Now {
                bind_gen: Some(4),
                ..now()
            },
            Now {
                t3_owned: true,
                ..now()
            },
            Now {
                track: None,
                ..now()
            },
            Now {
                track: Some((12, 0)),
                ..now()
            },
            Now {
                request: Some((RequestStatus::Answering, AnswerPath::Hook)),
                ..now()
            },
            Now {
                request: None,
                ..now()
            },
            Now {
                request: Some((RequestStatus::Open, AnswerPath::None)),
                ..now()
            },
        ] {
            assert!(!still_valid(&ticket(), &changed, PATHS));
        }
    }

    #[test]
    fn a_codex_binding_watch_validates_its_pane_and_generation_without_a_request() {
        let watch = Ticket {
            request_id: None,
            op: 0,
            kind: ReadKind::CodexBind,
            read_seq: 1,
            ..ticket()
        };
        let now = Now {
            track: Some((0, 1)),
            request: None,
            ..now()
        };
        assert!(still_valid(&watch, &now, PATHS));
        assert!(!still_valid(
            &watch,
            &Now {
                track: Some((0, 2)),
                ..now.clone()
            },
            PATHS
        ));
        assert!(!still_valid(
            &watch,
            &Now {
                bind_gen: Some(9),
                ..now
            },
            PATHS
        ));
    }

    #[test]
    fn read_slots_bump_only_on_admission_and_keep_kinds_independent() {
        let mut slots = ReadSlots::default();
        assert_eq!(slots.admit(ReadKind::Capture, true), Some(1));
        assert_eq!(slots.admit(ReadKind::Presence, false), None);
        assert_eq!(slots.latest(ReadKind::Capture), 1);
        assert_eq!(slots.latest(ReadKind::Presence), 0);
        assert_eq!(slots.wanted(), vec![ReadKind::Presence]);
        assert_eq!(slots.admit(ReadKind::HoldEnded, true), Some(1));
        assert_eq!(slots.admit(ReadKind::Capture, true), Some(2));
    }

    #[tokio::test]
    async fn a_new_read_aborts_the_older_task() {
        let mut slots = ReadSlots::default();
        slots.admit(ReadKind::Presence, true);
        let old = tokio::spawn(std::future::pending::<()>());
        slots.started(ReadKind::Presence, old.abort_handle());
        slots.admit(ReadKind::Presence, true);
        assert!(old.await.unwrap_err().is_cancelled());
    }

    #[tokio::test]
    async fn same_purpose_out_of_order_completion_does_not_finish_the_new_read() {
        let mut slots = ReadSlots::default();
        let first = slots.admit(ReadKind::Capture, true).unwrap();
        let old_task = tokio::spawn(std::future::pending::<()>());
        slots.started(ReadKind::Capture, old_task.abort_handle());
        let second = slots.admit(ReadKind::Capture, true).unwrap();
        assert!(old_task.await.unwrap_err().is_cancelled());
        let new_task = tokio::spawn(std::future::pending::<()>());
        slots.started(ReadKind::Capture, new_task.abort_handle());

        slots.finished(ReadKind::Capture, first);
        assert!(slots.busy(ReadKind::Capture));
        slots.finished(ReadKind::Capture, second);
        assert!(!slots.busy(ReadKind::Capture));
        new_task.abort();
    }
}

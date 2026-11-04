//! Property tests for the event log and resume (spec §7.4).
//!
//! For random event counts, ring capacities and resume points, `replay_after`
//! returns exactly the missing suffix, or `None` (a snapshot is due): never a
//! gap, never a duplicate, never an event from before the cursor. The same
//! holds through `Core::attach`, where the ring is the real 1000 events.
mod proptest_support;

use acd_core::testing::*;
use acd_core::*;
use acd_protocol::*;
use proptest::prelude::*;
use proptest_support::check;

fn change(i: u64) -> Change {
    Change::AgentRemoved(format!("a{i}"))
}

#[test]
fn replay_is_exactly_the_missing_suffix_or_a_snapshot() {
    let case = (1usize..24, 0u64..80, 0u64..100, 0u8..8);
    check("log_replay", 600, case, |(capacity, n, cursor_seq, epoch_sel)| {
        let mut log = EventLog::new("e1".into(), capacity);
        let recorded: Vec<ServerMessage> = (1..=n).map(|i| log.record(change(i))).collect();
        prop_assert_eq!(log.seq(), n);
        // One in eight cursors carries another epoch.
        let epoch = if epoch_sel == 0 { "other" } else { "e1" };
        let cursor = Cursor { epoch: epoch.into(), seq: cursor_seq };
        let oldest = if n > capacity as u64 { n - capacity as u64 + 1 } else { 1 };
        let must_snapshot = epoch != "e1" || cursor_seq > n || cursor_seq + 1 < oldest;
        match log.replay_after(&cursor) {
            None => prop_assert!(must_snapshot, "snapshot forced although the suffix is retained"),
            Some(events) => {
                prop_assert!(!must_snapshot, "replayed from a cursor that needs a snapshot");
                let seqs: Vec<u64> = events.iter().map(|m| m.seq().unwrap()).collect();
                let want: Vec<u64> = (cursor_seq + 1..=n).collect();
                prop_assert_eq!(seqs, want, "not the exact missing suffix");
                // They are the very events that were recorded.
                prop_assert_eq!(&events[..], &recorded[cursor_seq as usize..]);
            }
        }
        Ok(())
    });
}

#[test]
fn a_cursor_at_the_edges_never_overflows_or_panics() {
    let case = (1usize..8, 0u64..20, prop_oneof![Just(u64::MAX), Just(u64::MAX - 1), 0u64..30]);
    check("log_edges", 200, case, |(capacity, n, seq)| {
        let mut log = EventLog::new("e".into(), capacity);
        for i in 0..n {
            log.record(change(i));
        }
        let got = log.replay_after(&Cursor { epoch: "e".into(), seq });
        if seq > n {
            prop_assert!(got.is_none());
        }
        Ok(())
    });
}

#[derive(Clone, Debug)]
enum Step {
    Burst(u16),
    Resume,
    Restart,
}

#[test]
fn attach_through_the_core_resumes_gap_free_across_the_real_ring() {
    let step = prop_oneof![
        6 => (1u16..40).prop_map(Step::Burst),
        1 => (900u16..1300).prop_map(Step::Burst),
        4 => Just(Step::Resume),
        1 => Just(Step::Restart),
    ];
    check("core_attach_resume", 40, prop::collection::vec(step, 1..14), |steps| {
        let mut core = Core::new(test_machine(), vec![]);
        // What the client holds: the cursor of its last applied event.
        let mut cursor: Option<Cursor> = None;
        let mut counter = 0u64;
        for step in steps {
            match step {
                Step::Burst(n) => {
                    for _ in 0..n {
                        counter += 1;
                        // Every upsert with a new title is one event.
                        let mut a = agent_obs("a1");
                        a.title = format!("t{counter}");
                        core.observe(Observation::AgentUpsert(a));
                    }
                }
                Step::Restart => core = Core::new(test_machine(), vec![]),
                Step::Resume => {
                    let seq_now = match core.welcome() {
                        ServerMessage::Welcome { seq, .. } => seq,
                        _ => unreachable!(),
                    };
                    let (initial, mut rx) = core.attach(cursor.as_ref());
                    let epoch = core.epoch();
                    match initial.as_slice() {
                        [ServerMessage::Snapshot { epoch: e, seq, .. }] => {
                            prop_assert_eq!(e, &epoch);
                            prop_assert_eq!(*seq, seq_now);
                            // A snapshot is only sent when the suffix is not available.
                            if let Some(c) = &cursor
                                && c.epoch == epoch
                            {
                                prop_assert!(c.seq > seq_now || c.seq + (RING_CAPACITY as u64) < seq_now);
                            }
                        }
                        events => {
                            let c = cursor.as_ref().expect("events are only replayed to a cursor");
                            prop_assert_eq!(&c.epoch, &epoch);
                            let seqs: Vec<u64> = events.iter().map(|m| m.seq().unwrap()).collect();
                            prop_assert_eq!(seqs, (c.seq + 1..=seq_now).collect::<Vec<_>>());
                        }
                    }
                    // The next live event follows the initial messages directly.
                    core.observe(Observation::AgentUpsert({
                        counter += 1;
                        let mut a = agent_obs("a1");
                        a.title = format!("t{counter}");
                        a
                    }));
                    let live = rx.try_recv().expect("a live event");
                    prop_assert_eq!(live.seq(), Some(seq_now + 1));
                    cursor = Some(Cursor { epoch: core.epoch(), seq: seq_now + 1 });
                }
            }
        }
        Ok(())
    });
}

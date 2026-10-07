use acd_core::*;
use acd_protocol::*;

fn removed(id: &str) -> Change { Change::AgentRemoved(id.into()) }

fn cursor(epoch: &str, seq: u64) -> Cursor { Cursor { epoch: epoch.into(), seq } }

#[test]
fn record_assigns_increasing_seq_with_epoch() {
    let mut log = EventLog::new("e1".into(), 1000);
    let m1 = log.record(removed("a"));
    let m2 = log.record(removed("b"));
    assert_eq!((m1.seq(), m2.seq(), log.seq()), (Some(1), Some(2), 2));
    assert!(matches!(m2, ServerMessage::AgentRemoved { ref epoch, .. } if epoch == "e1"));
}

#[test]
fn replay_returns_events_after_cursor() {
    let mut log = EventLog::new("e1".into(), 1000);
    for i in 0..5 { log.record(removed(&i.to_string())); }
    let replay = log.replay_after(&cursor("e1", 3)).unwrap();
    assert_eq!(replay.iter().map(|m| m.seq().unwrap()).collect::<Vec<_>>(), vec![4, 5]);
    assert_eq!(log.replay_after(&cursor("e1", 5)).unwrap().len(), 0);
}

#[test]
fn replay_refuses_other_epoch_future_seq_and_gaps() {
    let mut log = EventLog::new("e1".into(), 3);
    for i in 0..10 { log.record(removed(&i.to_string())); }
    assert!(log.replay_after(&cursor("e0", 9)).is_none(), "other epoch");
    assert!(log.replay_after(&cursor("e1", 11)).is_none(), "future seq");
    assert!(log.replay_after(&cursor("e1", 5)).is_none(), "evicted range");
    assert_eq!(log.replay_after(&cursor("e1", 7)).unwrap().len(), 3);
}

#[test]
fn empty_log_replays_nothing_for_zero_cursor() {
    let log = EventLog::new("e1".into(), 1000);
    assert_eq!(log.replay_after(&cursor("e1", 0)).unwrap().len(), 0);
}

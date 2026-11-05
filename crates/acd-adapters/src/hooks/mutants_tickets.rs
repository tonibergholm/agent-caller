//! Tests added from a cargo-mutants run (docs/research/2026-11-04-mutation-testing.md).
//! A child module of `tickets`, for the crate-private `ReadSlots`.
use super::*;
use tokio::sync::oneshot;

/// A task that never ends on its own; the receiver sees the sender dropped
/// exactly when the task is aborted.
fn parked() -> (AbortHandle, oneshot::Receiver<()>) {
    let (held, watcher) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let _held = held;
        std::future::pending::<()>().await;
    });
    (task.abort_handle(), watcher)
}

async fn aborted(watcher: oneshot::Receiver<()>) -> bool {
    // No deadline: a task that was not aborted keeps its sender alive and this hangs.
    watcher.await.is_err()
}

/// tickets.rs:28/29 `admit`: a refused read is remembered as wanted and gets no
/// sequence number; an admitted one gets the next number, clears the wish and
/// aborts the read it replaces.
#[tokio::test]
async fn admit_numbers_reads_and_remembers_refused_ones() {
    let mut slots = ReadSlots::default();
    assert_eq!(slots.admit(ReadKind::Capture, false), None);
    assert_eq!(slots.wanted(), [ReadKind::Capture]);
    assert_eq!(slots.admit(ReadKind::Capture, true), Some(1));
    assert!(slots.wanted().is_empty(), "admitting clears the wish");
    let (task, watcher) = parked();
    slots.started(ReadKind::Capture, task);
    assert!(slots.busy(ReadKind::Capture));
    assert_eq!(slots.admit(ReadKind::Capture, true), Some(2), "numbers only grow");
    assert!(aborted(watcher).await, "the replaced read was aborted");
    assert_eq!(slots.latest(ReadKind::Capture), 2);
    assert_eq!(slots.latest(ReadKind::Presence), 0);
    // Kinds are independent.
    assert_eq!(slots.admit(ReadKind::Presence, true), Some(1));
}

/// tickets.rs:41 `started -> ()`, :65 `unwant -> ()`: a started task is the slot's task (so
/// it is aborted when replaced), and a wish can be withdrawn.
#[tokio::test]
async fn started_tasks_are_tracked_and_wishes_can_be_withdrawn() {
    let mut slots = ReadSlots::default();
    slots.admit(ReadKind::HoldEnded, false);
    slots.unwant(ReadKind::HoldEnded);
    assert!(slots.wanted().is_empty());
    slots.unwant(ReadKind::Capture); // unknown kind: nothing to do
    let seq = slots.admit(ReadKind::Capture, true).unwrap();
    let (task, watcher) = parked();
    slots.started(ReadKind::Capture, task);
    assert!(slots.busy(ReadKind::Capture));
    slots.finished(ReadKind::Capture, seq + 1);
    assert!(slots.busy(ReadKind::Capture), "another read's end leaves this one running");
    slots.finished(ReadKind::Capture, seq);
    assert!(!slots.busy(ReadKind::Capture));
    drop(watcher);
}

/// tickets.rs:70 `abort_all -> ()` and :81 `Drop for ReadSlots -> ()`: every running read ends
/// and every wish is dropped, on request and when the slots go.
#[tokio::test]
async fn abort_all_and_drop_end_every_read() {
    let mut slots = ReadSlots::default();
    let mut watchers = Vec::new();
    for kind in [ReadKind::Capture, ReadKind::Presence] {
        slots.admit(kind, true);
        let (task, watcher) = parked();
        slots.started(kind, task);
        watchers.push(watcher);
    }
    slots.admit(ReadKind::HoldEnded, false);
    slots.abort_all();
    for w in watchers.drain(..) {
        assert!(aborted(w).await);
    }
    assert!(slots.wanted().is_empty());
    assert!(!slots.busy(ReadKind::Capture));

    let mut slots = ReadSlots::default();
    slots.admit(ReadKind::Capture, true);
    let (task, watcher) = parked();
    slots.started(ReadKind::Capture, task);
    drop(slots);
    assert!(aborted(watcher).await, "dropping the slots aborts their reads");
}

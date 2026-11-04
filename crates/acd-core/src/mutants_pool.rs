//! Test added from the second cargo-mutants run
//! (docs/research/2026-11-04-mutation-testing.md).
use super::*;

/// server.rs:270 `*n == 0` -> `!=` in `SourceSlot::drop`: releasing one of two
/// slots leaves the other counted, and releasing the last forgets the source.
#[test]
fn a_released_slot_leaves_the_others_counted() {
    let pool = Arc::new(SourcePool::new(8));
    let ip: std::net::IpAddr = "10.0.0.1".parse().unwrap();
    let first = pool.try_acquire(ip).unwrap();
    let second = pool.try_acquire(ip).unwrap();
    assert_eq!(pool.held.lock().unwrap().get(&ip), Some(&2));
    drop(first);
    assert_eq!(pool.held.lock().unwrap().get(&ip), Some(&1), "one slot is still held");
    drop(second);
    assert!(pool.held.lock().unwrap().is_empty(), "the last release forgets the source");
}

//! Growth checks: tables that must stay bounded by live state, not by how
//! many requests an agent or client has ever made (load-test findings,
//! docs/research/2026-11-04-soak-load.md).
use acd_core::testing::*;
use acd_core::{Core, Observation, RequestObs};

fn unique(agent: &str, key: &str) -> RequestObs {
    // Claude prompt ids, T3 request ids and external keys are unique per
    // occurrence: the key never identifies a later dialog.
    RequestObs { reusable_key: false, ..permission_obs(agent, key) }
}

#[test]
fn sizes_report_every_table() {
    let core = Core::new(test_machine(), vec![FakeAdapter::new()]);
    assert_eq!(core.sizes().agents, 0);
    core.observe(Observation::AgentUpsert(agent_obs("a1")));
    core.observe(Observation::RequestSeen(unique("a1", "k1")));
    let s = core.sizes();
    assert_eq!((s.agents, s.requests, s.keys), (1, 1, 1));
    assert_eq!(s.ring, 3, "agent.updated, request.opened, agent.updated");
}

use acd_core::{CorrelationKey, RETIRED_KEYS_CAP};
use acd_protocol::ResolvedBy;

fn cycle(core: &Core, agent: &str, key: &str) {
    core.observe(Observation::RequestSeen(unique(agent, key)));
    core.observe(Observation::RequestEnded {
        agent_id: agent.into(),
        key: CorrelationKey(key.into()),
        by: ResolvedBy::Inbox,
    });
}

#[test]
fn a_long_lived_agent_does_not_accumulate_key_states() {
    let core = Core::new(test_machine(), vec![FakeAdapter::new()]);
    core.observe(Observation::AgentUpsert(agent_obs("a1")));
    for i in 0..RETIRED_KEYS_CAP * 2 {
        cycle(&core, "a1", &format!("occ-{i}"));
    }
    let s = core.sizes();
    assert_eq!(s.requests, 0, "every request resolved");
    assert!(s.keys <= RETIRED_KEYS_CAP, "key states grew with history: {}", s.keys);
}

#[test]
fn a_recently_resolved_unique_key_still_cannot_reopen() {
    let core = Core::new(test_machine(), vec![FakeAdapter::new()]);
    core.observe(Observation::AgentUpsert(agent_obs("a1")));
    for i in 0..RETIRED_KEYS_CAP + 10 {
        cycle(&core, "a1", &format!("occ-{i}"));
    }
    // Replay of the newest occurrence: ignored, as before.
    let last = format!("occ-{}", RETIRED_KEYS_CAP + 9);
    core.observe(Observation::RequestSeen(unique("a1", &last)));
    assert_eq!(core.sizes().requests, 0, "a replayed resolved key reopened");
}

#[test]
fn migration_keeps_the_eviction_order_for_the_moved_keys() {
    let core = Core::new(test_machine(), vec![FakeAdapter::new()]);
    core.observe(Observation::AgentUpsert(agent_obs("a")));
    core.observe(Observation::AgentUpsert(agent_obs("b")));
    for i in 0..RETIRED_KEYS_CAP {
        cycle(&core, "a", &format!("a-{i}"));
    }
    core.observe(Observation::RequestSeen(unique("a", "open")));
    core.observe(Observation::AgentMigrated { from: "a".into(), to: "b".into() });
    core.observe(Observation::RequestEnded { agent_id: "b".into(), key: CorrelationKey("open".into()), by: ResolvedBy::Inbox });
    for i in 0..RETIRED_KEYS_CAP {
        cycle(&core, "b", &format!("b-{i}"));
    }
    let s = core.sizes();
    assert!(s.keys <= RETIRED_KEYS_CAP, "moved keys escaped eviction: {}", s.keys);
}

#[test]
fn a_replay_after_eviction_opens_a_new_request_and_a_newer_key_does_not() {
    let core = Core::new(test_machine(), vec![FakeAdapter::new()]);
    core.observe(Observation::AgentUpsert(agent_obs("a1")));
    core.observe(Observation::AgentUpsert(agent_obs("a2")));
    for i in 0..RETIRED_KEYS_CAP + 1 {
        cycle(&core, "a1", &format!("occ-{i}"));
    }
    // Cross-agent traffic shares the cap: two more resolutions push out a1's oldest tombstones.
    cycle(&core, "a2", "occ-0");
    cycle(&core, "a2", "occ-1");
    // Newest keys of both agents stay tombstoned: their replay is ignored.
    core.observe(Observation::RequestSeen(unique("a2", "occ-1")));
    core.observe(Observation::RequestSeen(unique("a1", &format!("occ-{}", RETIRED_KEYS_CAP))));
    assert_eq!(core.sizes().requests, 0, "a recent tombstone was lost");
    // The oldest key of a1 was evicted: its replay opens a new request (documented residual).
    core.observe(Observation::RequestSeen(unique("a1", "occ-0")));
    assert_eq!(core.sizes().requests, 1, "an evicted key must be treated as new");
}

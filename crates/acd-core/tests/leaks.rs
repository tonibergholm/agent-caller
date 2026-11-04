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

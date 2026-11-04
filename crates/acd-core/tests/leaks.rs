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

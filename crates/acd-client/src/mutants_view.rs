//! Tests added from a cargo-mutants run (docs/research/2026-11-04-mutation-testing.md).
//! A child module of `view`.
use super::*;
use crate::{conn::MachineState, session::Session, settings::MachineEntry};
use acd_protocol::Agent;

fn entry(endpoint: EndpointConfig) -> MachineEntry {
    MachineEntry { key: "k".into(), name: "n".into(), endpoint }
}

fn tcp() -> EndpointConfig {
    EndpointConfig::Tcp { host: "127.0.0.1".into(), port: 1 }
}

/// `from_entry` `||` -> `&&` (line 185): a local machine is paired without a
/// secret, a network machine only with one.
#[test]
fn local_machines_need_no_secret_and_network_machines_do() {
    let secret = MachineSecret { token: "t".into(), pin: "p".into() };
    let paired = |e: EndpointConfig, s: Option<&MachineSecret>| MachineEntryView::from_entry(&entry(e), s).pairing;
    assert_eq!(paired(EndpointConfig::Local, None), PairingState::Paired);
    assert_eq!(paired(EndpointConfig::Local, Some(&secret)), PairingState::Paired);
    assert_eq!(paired(tcp(), Some(&secret)), PairingState::Paired);
    assert_eq!(paired(tcp(), None), PairingState::NotPaired);
}

fn agent(state: &str, pending: bool) -> Agent {
    let p = serde_json::json!({"id":"r","revision":1,"agent_id":"a","kind":"permission","status":"open","answer_path":"hook","title":"t","detail":"d","options":[],"questions":null,"free_text":false,"raw_screen":null,"opened_at":"2026-10-08T00:00:00Z"});
    serde_json::from_value(serde_json::json!({"id":"a","machine_id":"m","source":"herdr","kind":"claude","title":"agent","cwd":null,"project":null,"state":state,"state_since":"2026-10-08T00:00:00Z","failed":false,"last_message":null,"can_send":true,"pending": if pending { p } else { serde_json::Value::Null }})).unwrap()
}

/// `group` `||` -> `&&` (line 332): a pending request puts the card in the
/// request group whatever the agent's state, and so does needing the user.
#[test]
fn a_pending_request_or_a_waiting_agent_is_in_the_request_group() {
    assert_eq!(group(&agent("working", true)), CardGroup::Request);
    assert_eq!(group(&agent("needs_you", false)), CardGroup::Request);
    assert_eq!(group(&agent("working", false)), CardGroup::Working);
    assert_eq!(group(&agent("done", false)), CardGroup::Done);
    assert_eq!(group(&agent("idle", false)), CardGroup::Idle);
}

/// `build_view` `==` -> `!=` (lines 361, 378): only the local machine is marked local.
#[test]
fn only_the_local_endpoint_is_marked_local() {
    let local = entry(EndpointConfig::Local);
    let net = entry(tcp());
    let state = MachineState {
        instance: 1,
        status: crate::conn::MachineStatus::Connecting,
        session: Session::default(),
        conn_gen: 1,
    };
    let busy = HashSet::new();
    let notices = HashMap::new();
    for with_state in [false, true] {
        let input = ViewInput {
            machines: vec![
                MachineInput { entry: &local, state: with_state.then_some(&state) },
                MachineInput { entry: &net, state: with_state.then_some(&state) },
            ],
            actions: &[],
            busy: &busy,
            notices: &notices,
            show_idle: false,
            show_subagents: false,
            show_hook_only: false,
        };
        let v = build_view(&input);
        assert_eq!(v.machines.len(), 2);
        assert!(v.machines[0].is_local, "with_state={with_state}");
        assert!(!v.machines[1].is_local, "with_state={with_state}");
    }
}

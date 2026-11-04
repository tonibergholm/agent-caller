//! Tests added from a cargo-mutants run (docs/research/2026-11-04-mutation-testing.md).
//! Private items of `server`, so this is a child module of it.
use super::*;
use crate::testing::*;

/// `LogLimiter::pass -> Some((0, 0)) ...`: the first lines through report no
/// suppressed lines and a running total.
#[test]
fn log_limiter_pass_counts_events() {
    let l = LogLimiter::new();
    assert_eq!(l.pass(), Some((0, 1)));
    assert_eq!(l.pass(), Some((0, 2)));
    // Past the per-window maximum, lines are held back and later reported.
    let mut held = 0;
    for _ in 0..LogLimiter::MAX {
        if l.pass().is_none() {
            held += 1;
        }
    }
    assert!(held > 0);
}

/// `revoked`: `!=` -> `==`. It stays pending while the generation is unchanged
/// and completes once it moves.
#[tokio::test]
async fn revoked_waits_for_a_rotation() {
    let auth = Auth::new("t".into());
    let mut rx = auth.subscribe();
    tokio::select! {
        biased;
        () = revoked(&mut rx, 0) => panic!("completed with no rotation"),
        () = std::future::ready(()) => {}
    }
    auth.rotate("u".into());
    revoked(&mut rx, 0).await;
}

fn answer(id: &str, request_id: &str) -> ClientMessage {
    ClientMessage::RequestAnswer {
        id: id.into(), request_id: request_id.into(), revision: None,
        option_id: Some("allow".into()), text: None,
    }
}

/// `run_command -> ()` and `!=` -> `==`: a current command runs and replies;
/// one authorized under an older token never touches the core.
#[tokio::test]
async fn run_command_replies_unless_revoked_first() {
    let fake = FakeAdapter::new();
    let core = Core::new(test_machine(), vec![fake.clone()]);
    core.observe(crate::registry::Observation::AgentUpsert(agent_obs("a1")));
    core.observe(crate::registry::Observation::RequestSeen(permission_obs("a1", "k1")));
    let rid = core.pending_key("a1").unwrap().0;
    let auth = Auth::new("t".into());

    // Revoked before it ran: no reply, nothing delivered.
    let stale_rx = auth.subscribe();
    auth.rotate("u".into());
    let (tx, mut out) = mpsc::channel(4);
    run_command(core.clone(), "cl".into(), Origin::Local, answer("c0", &rid), 0, stale_rx, tx, ()).await;
    assert!(out.recv().await.is_none(), "a revoked command sends no reply");
    assert!(fake.delivered().is_empty());

    // Current generation: delivered and answered.
    let rx = auth.subscribe();
    let (tx, mut out) = mpsc::channel(4);
    run_command(core.clone(), "cl".into(), Origin::Local, answer("c1", &rid), auth.generation(), rx, tx, ()).await;
    let Some(Message::Text(t)) = out.recv().await else { panic!("expected a reply") };
    assert!(t.as_str().contains("\"c1\"") && t.as_str().contains("delivered"), "{t}");
    assert_eq!(fake.delivered().len(), 1);
}

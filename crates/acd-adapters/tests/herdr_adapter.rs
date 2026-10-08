use acd_adapters::herdr::{
    HerdrAdapter, HerdrConfig,
    fake::{FakeHerdr, fake_agent},
    reconciler::Timing,
};
use acd_core::{Adapter, Core, DeliverError, testing::test_machine};
use acd_protocol::*;
use std::{sync::Arc, time::Duration};

async fn until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    for _ in 0..200 {
        if let Some(v) = f() {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

struct Rig {
    fake: FakeHerdr,
    core: Arc<Core>,
    adapter: Arc<HerdrAdapter>,
    _home: tempfile::TempDir,
}

async fn rig(agents: &[(&str, &str, &str)]) -> Rig {
    let home = tempfile::Builder::new()
        .prefix("acd")
        .tempdir_in("/tmp")
        .unwrap(); // short: macOS socket paths max 104 bytes
    let sock = home.path().join(".config/herdr/herdr.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
    let fake = FakeHerdr::start_at(&sock).await;
    for (p, a, s) in agents {
        fake.set_agent(fake_agent(p, a, s));
    }
    let timing = Timing {
        list_idle: Duration::from_millis(300),
        list_blocked: Duration::from_millis(100),
        backoff_max: Duration::from_millis(200),
        fallback_after: Duration::from_millis(200),
        poll: Duration::from_millis(50),
    };
    let cfg = HerdrConfig {
        enabled: true,
        home: home.path().into(),
        env_socket: None,
        extra_sockets: vec![],
        scan_interval: Duration::from_millis(100),
        timing,
    };
    let adapter = HerdrAdapter::new(cfg);
    let core = Core::new(test_machine(), vec![adapter.clone()]);
    adapter.start(core.clone());
    until("agents", || {
        (core.attach(None).0.len() == 1 && adapter.panes().read().unwrap().len() == agents.len())
            .then_some(())
    })
    .await;
    Rig {
        fake,
        core,
        adapter,
        _home: home,
    }
}

fn agent(r: &Rig) -> Agent {
    r.core
        .agent(r.adapter.panes().read().unwrap().keys().next().unwrap())
        .unwrap()
}

#[tokio::test]
async fn follow_up_reaches_an_idle_agent() {
    let r = rig(&[("w1:p1", "claude", "idle")]).await;
    r.adapter.send(&agent(&r), "run the tests").await.unwrap();
    assert_eq!(
        r.fake.prompts(),
        vec![("w1:p1".to_string(), "run the tests".to_string())]
    );
    until("working", || {
        (agent(&r).state == AgentState::Working).then_some(())
    })
    .await;
}

#[tokio::test]
async fn follow_up_errors_map_to_not_delivered() {
    let r = rig(&[("w1:p1", "claude", "blocked")]).await;
    let a = agent(&r);
    assert!(matches!(
        r.adapter.send(&a, "hi").await,
        Err(DeliverError::NotDelivered(ErrorCode::AgentBusy))
    ));
    assert!(matches!(
        r.adapter.send(&a, "a\u{202E}b").await,
        Err(DeliverError::NotDelivered(ErrorCode::BadRequest))
    ));
    assert!(matches!(
        r.adapter.send(&a, &"x".repeat(16 * 1024 + 1)).await,
        Err(DeliverError::NotDelivered(ErrorCode::BadRequest))
    ));
    let mut gone = a.clone();
    gone.id = "herdr:000000000000:w9:p9".into();
    assert!(matches!(
        r.adapter.send(&gone, "hi").await,
        Err(DeliverError::NotDelivered(ErrorCode::AgentGone))
    ));
    assert!(r.fake.prompts().is_empty());
}

#[tokio::test]
async fn prompt_reply_lost_after_write_is_uncertain() {
    let r = rig(&[("w1:p1", "claude", "idle")]).await;
    r.fake.hang_next("agent.prompt");
    let res = tokio::time::timeout(Duration::from_secs(11), r.adapter.send(&agent(&r), "hi"))
        .await
        .unwrap();
    assert!(matches!(res, Err(DeliverError::Uncertain(_))), "{res:?}");
    assert_eq!(r.fake.prompts().len(), 1);
}

#[tokio::test]
async fn mark_seen_never_touches_herdr() {
    let r = rig(&[("w1:p1", "claude", "done")]).await;
    let before = r.fake.calls().len();
    r.core
        .handle(
            "c",
            ClientMessage::AgentMarkSeen {
                id: "m1".into(),
                agent_id: agent(&r).id,
            },
        )
        .await;
    assert_eq!(agent(&r).state, AgentState::Idle);
    assert!(
        r.fake.calls()[before..]
            .iter()
            .all(|(m, _)| m != "agent.focus")
    );
}

#[tokio::test]
async fn disabled_config_reports_disabled_and_starts_nothing() {
    let home = tempfile::tempdir().unwrap();
    let cfg = HerdrConfig {
        enabled: false,
        ..HerdrConfig::from_env(false, vec![])
    };
    let cfg = HerdrConfig {
        home: home.path().into(),
        ..cfg
    };
    let adapter = HerdrAdapter::new(cfg);
    let core = Core::new(test_machine(), vec![adapter.clone()]);
    adapter.start(core.clone());
    let ServerMessage::Welcome { machine, .. } = core.welcome() else {
        panic!()
    };
    assert_eq!(machine.sources.herdr.state, SourceState::Disabled);
}

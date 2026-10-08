mod common;

use acd_client::{
    conn::MachineStatus,
    inbox::{Inbox, InboxOptions, PickError},
    settings::{
        DEFAULT_HOTKEY, EndpointConfig, MachineEntry, MachineSecret, MemorySecrets, SecretStore,
        Settings,
    },
    view::TrayColor,
};
use acd_core::{Observation, registry::CorrelationKey, testing::*};
use acd_protocol::{AgentState, ResolvedBy};
use std::{sync::Arc, time::Duration};

fn inbox(d: &common::Daemon) -> Arc<Inbox> {
    let secrets = Arc::new(MemorySecrets::default());
    secrets
        .set(
            "m1",
            &MachineSecret {
                token: common::TOKEN.into(),
                pin: d.pin.clone(),
            }
            .to_json(),
        )
        .unwrap();
    let settings = Settings {
        client_id: "11111111-1111-4111-8111-111111111111".into(),
        hotkey: DEFAULT_HOTKEY.into(),
        show_idle: false,
        machines: vec![MachineEntry {
            key: "m1".into(),
            name: "Test machine".into(),
            endpoint: EndpointConfig::Tcp {
                host: "127.0.0.1".into(),
                port: d.port,
            },
        }],
    };
    let inbox = Inbox::new(
        settings,
        secrets,
        InboxOptions {
            timing: common::fast(),
            settings_path: None,
            client_name: "test".into(),
            client_version: "0".into(),
        },
    );
    inbox.start();
    inbox
}

async fn wait_view(
    inbox: &Inbox,
    what: &str,
    predicate: impl Fn(&acd_client::view::InboxView) -> bool,
) -> acd_client::view::InboxView {
    let mut rx = inbox.view();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let view = rx.borrow().clone();
            if predicate(&view) {
                return view;
            }
            rx.changed().await.unwrap();
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

async fn wait_machine_status(
    inbox: &Inbox,
    what: &str,
    predicate: impl Fn(&MachineStatus) -> bool,
) {
    wait_view(inbox, what, |v| {
        v.machines
            .iter()
            .any(|m| m.key == "m1" && predicate(&m.status))
    })
    .await;
}

async fn wait_until(what: &str, predicate: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if predicate() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

fn observe_pending(core: &acd_core::Core, agent_id: &str, request_key: &str) -> String {
    core.observe(Observation::AgentUpsert(agent_obs(agent_id)));
    core.observe(Observation::RequestSeen(permission_obs(
        agent_id,
        request_key,
    )));
    core.request_id_for(agent_id, &CorrelationKey(request_key.into()))
        .expect("request observation follows an existing agent")
}

fn card<'a>(
    view: &'a acd_client::view::InboxView,
    key: &str,
) -> Option<&'a acd_client::view::CardView> {
    view.cards.iter().find(|c| c.key == key)
}

#[tokio::test]
async fn request_reaches_the_view_within_two_seconds() {
    let d = common::daemon().await;
    let inbox = inbox(&d);
    wait_view(&inbox, "machine online", |v| {
        v.machines
            .iter()
            .any(|m| m.key == "m1" && m.status == MachineStatus::Online)
    })
    .await;

    let request_id = observe_pending(&d.core, "a1", "k1");
    let view = wait_view(&inbox, "answerable request", |v| {
        v.cards.iter().any(|c| {
            c.key == "m1/a1"
                && c.answerable
                && c.agent.pending.as_ref().is_some_and(|r| r.id == request_id)
        })
    })
    .await;
    assert_eq!(view.tray.color, TrayColor::Amber);
    assert_eq!(view.tray.needs_you, 1);
}

#[tokio::test]
async fn answered_elsewhere_resolves_the_card() {
    let d = common::daemon().await;
    let inbox = inbox(&d);
    wait_view(&inbox, "machine online", |v| {
        v.machines.iter().any(|m| m.status == MachineStatus::Online)
    })
    .await;
    let request_id = observe_pending(&d.core, "a1", "k1");
    wait_view(&inbox, "answerable request", |v| {
        v.cards.iter().any(|c| c.key == "m1/a1" && c.answerable)
    })
    .await;

    d.core.observe(Observation::RequestEndedById {
        request_id: request_id.clone(),
        by: ResolvedBy::Terminal,
    });
    let view = wait_view(&inbox, "terminal resolution notice", |v| {
        v.cards.iter().any(|c| {
            c.key == "m1/a1"
                && c.agent.pending.is_none()
                && c.notice
                    .as_ref()
                    .is_some_and(|n| n.text == "Answered in the terminal")
        })
    })
    .await;
    assert!(matches!(
        inbox.pick("m1/a1", &request_id, "allow"),
        Err(PickError::StaleRequest)
    ));
    assert!(card(&view, "m1/a1").unwrap().agent.pending.is_none());
}

#[tokio::test]
async fn retry_after_drop_delivers_once_end_to_end() {
    let fake = FakeAdapter::gated();
    let mut d = common::daemon_with(fake.clone()).await;
    let inbox = inbox(&d);
    wait_view(&inbox, "machine online", |v| {
        v.machines.iter().any(|m| m.status == MachineStatus::Online)
    })
    .await;
    let request_id = observe_pending(&d.core, "a1", "k1");
    wait_view(&inbox, "answerable request", |v| {
        card(v, "m1/a1").is_some_and(|c| c.answerable)
    })
    .await;
    inbox.pick("m1/a1", &request_id, "allow").unwrap();
    wait_view(&inbox, "delivery in progress", |v| {
        card(v, "m1/a1").is_some_and(|c| c.busy)
    })
    .await;
    inbox.drop_connections_for_test();
    tokio::time::sleep(Duration::from_millis(50)).await;
    fake.open_gate();

    wait_until("adapter delivery", || fake.delivered().len() == 1).await;
    assert_eq!(fake.delivered().len(), 1);
    common::restart(&mut d).await;
}

#[tokio::test]
async fn follow_up_retry_sends_once() {
    let fake = FakeAdapter::new();
    let d = common::daemon_with(fake.clone()).await;
    let inbox = inbox(&d);
    wait_view(&inbox, "machine online", |v| {
        v.machines.iter().any(|m| m.status == MachineStatus::Online)
    })
    .await;
    let mut idle = agent_obs("a1");
    idle.state = AgentState::Working;
    d.core.observe(Observation::AgentUpsert(idle));
    wait_view(&inbox, "working card", |v| {
        v.cards.iter().any(|c| c.key == "m1/a1" && c.can_reply)
    })
    .await;
    inbox.reply("m1/a1", "Follow up".into()).unwrap();
    wait_until("follow-up send", || !fake.sent().is_empty()).await;
    assert_eq!(fake.sent().len(), 1);
}

#[tokio::test]
async fn restart_into_new_epoch_refreshes_cards() {
    let mut d = common::daemon().await;
    let inbox = inbox(&d);
    wait_view(&inbox, "machine online", |v| {
        v.machines.iter().any(|m| m.status == MachineStatus::Online)
    })
    .await;
    let a1_request = observe_pending(&d.core, "a1", "k1");
    wait_view(&inbox, "a1 card", |v| {
        v.cards
            .iter()
            .any(|c| c.key == "m1/a1" && c.agent.pending.is_some())
    })
    .await;

    common::restart(&mut d).await;
    d.core.observe(Observation::AgentUpsert(agent_obs("a2")));
    observe_pending(&d.core, "a2", "k2");
    let view = wait_view(&inbox, "new epoch cards", |v| {
        v.machines
            .iter()
            .any(|m| m.key == "m1" && m.status == MachineStatus::Online)
            && v.cards.iter().any(|c| c.key == "m1/a2")
            && !v.cards.iter().any(|c| c.key == "m1/a1")
    })
    .await;
    assert!(view.cards.iter().all(|c| !c.busy));
    assert!(matches!(
        inbox.pick("m1/a1", &a1_request, "allow"),
        Err(PickError::UnknownCard)
    ));
    assert!(
        view.machines
            .iter()
            .any(|m| m.status == MachineStatus::Online)
    );
}

#[tokio::test]
async fn restart_mid_command_shows_check_the_terminal() {
    let fake = FakeAdapter::gated();
    let mut d = common::daemon_with(fake.clone()).await;
    let inbox = inbox(&d);
    wait_view(&inbox, "machine online", |v| {
        v.machines.iter().any(|m| m.status == MachineStatus::Online)
    })
    .await;
    let request_id = observe_pending(&d.core, "a1", "k1");
    wait_view(&inbox, "answerable request", |v| {
        card(v, "m1/a1").is_some_and(|c| c.answerable)
    })
    .await;
    inbox.pick("m1/a1", &request_id, "allow").unwrap();
    wait_view(&inbox, "delivery in progress", |v| {
        card(v, "m1/a1").is_some_and(|c| c.busy)
    })
    .await;
    common::restart(&mut d).await;
    wait_machine_status(&inbox, "new epoch online", |s| *s == MachineStatus::Online).await;
    assert!(d.fake.delivered().is_empty());
    assert!(d.fake.delivered().is_empty());
}

#[tokio::test]
async fn acd_down_greys_cards_then_recovers() {
    let mut d = common::daemon().await;
    let inbox = inbox(&d);
    wait_view(&inbox, "machine online", |v| {
        v.machines.iter().any(|m| m.status == MachineStatus::Online)
    })
    .await;
    observe_pending(&d.core, "a1", "k1");
    wait_view(&inbox, "request card", |v| {
        v.cards.iter().any(|c| c.key == "m1/a1")
    })
    .await;
    common::restart_after(&mut d, Duration::from_millis(500)).await;
    let offline = wait_view(&inbox, "offline cards", |v| {
        v.machines.iter().any(|m| m.status != MachineStatus::Online)
            && v.cards.iter().any(|c| c.key == "m1/a1" && !c.live)
            && v.tray.color == TrayColor::Grey
    })
    .await;
    assert!(!card(&offline, "m1/a1").unwrap().live);
    wait_view(&inbox, "machine recovery", |v| {
        v.machines
            .iter()
            .any(|m| m.key == "m1" && m.status == MachineStatus::Online)
    })
    .await;
}

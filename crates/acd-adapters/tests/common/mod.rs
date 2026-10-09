#![allow(dead_code)]
use std::sync::Arc;
use std::time::Duration;

use acd_adapters::herdr::{
    HerdrAdapter, HerdrConfig, fake::FakeHerdr, keys::KeysTiming, reconciler::Timing,
};
use acd_adapters::hooks::claude_sessions::{ProcCheck, ProcVerdict};
use acd_adapters::hooks::endpoint::{Credential, HookAuth};
use acd_adapters::hooks::testing::{post, send};
use acd_adapters::hooks::{HooksAdapter, HooksConfig};
use acd_adapters::t3::sessions::SessionMap;
use acd_core::{Core, testing::test_machine};
use acd_protocol::Agent;
use serde_json::Value;

pub const TOKEN: &str = "tok-hooks-test-0123456789";
pub struct Live;
impl ProcCheck for Live {
    fn check(&self, _: i32, _: &str) -> ProcVerdict {
        ProcVerdict::Live
    }
}
pub struct Rig {
    pub core: Arc<Core>,
    pub hooks: Arc<HooksAdapter>,
    pub herdr: Option<Arc<HerdrAdapter>>,
    pub fake: Option<FakeHerdr>,
    pub t3: Arc<SessionMap>,
    pub port: u16,
    pub home: tempfile::TempDir,
}
pub fn fast(cfg: &mut HooksConfig) {
    cfg.tick = Duration::from_millis(20);
    cfg.scan_interval = Duration::from_millis(100);
    cfg.evidence_window = Duration::from_millis(400);
    cfg.capture_window = Duration::from_millis(600);
    cfg.capture_poll = Duration::from_millis(50);
    cfg.codex_window = Duration::from_millis(600);
    cfg.held_dialog_poll = Duration::from_millis(100);
    cfg.end_grace = Duration::from_millis(200);
}
pub async fn rig(panes: &[Value], tweak: impl FnOnce(&mut HooksConfig)) -> Rig {
    rig_with(panes, tweak, |_| {}).await
}
/// `rig` with a tweak of the herdr adapter's config too.
pub async fn rig_with(
    panes: &[Value],
    tweak: impl FnOnce(&mut HooksConfig),
    herdr_tweak: impl FnOnce(&mut HerdrConfig),
) -> Rig {
    let home = tempfile::Builder::new()
        .prefix("acd")
        .tempdir_in("/tmp")
        .unwrap();
    let (herdr, fake) = if panes.is_empty() {
        (None, None)
    } else {
        let sock = home.path().join(".config/herdr/herdr.sock");
        std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
        let fake = FakeHerdr::start_at(&sock).await;
        for p in panes {
            fake.set_agent(p.clone());
        }
        let mut cfg = HerdrConfig {
            enabled: true,
            home: home.path().into(),
            env_socket: None,
            extra_sockets: vec![],
            scan_interval: Duration::from_millis(100),
            timing: Timing {
                list_idle: Duration::from_millis(300),
                list_blocked: Duration::from_millis(100),
                backoff_max: Duration::from_millis(200),
                fallback_after: Duration::from_millis(200),
                poll: Duration::from_millis(50),
            },
            keys: KeysTiming {
                evidence_timeout: Duration::from_millis(800),
                ..KeysTiming::default()
            },
        };
        herdr_tweak(&mut cfg);
        (Some(HerdrAdapter::new(cfg)), Some(fake))
    };
    let sessions = home.path().join(".claude/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let mut cfg = HooksConfig::new(
        Duration::from_secs(3600),
        vec!["sdk-ts".into()],
        acd_adapters::hooks::SessionRoots::fixed(vec![sessions]),
    );
    fast(&mut cfg);
    tweak(&mut cfg);
    let t3 = Arc::new(SessionMap::default());
    let hooks = HooksAdapter::new(cfg, herdr.clone(), Some(t3.clone()), Arc::new(Live));
    let mut adapters: Vec<Arc<dyn acd_core::Adapter>> = vec![hooks.clone()];
    if let Some(h) = &herdr {
        adapters.push(h.clone());
    }
    let core = Core::new(test_machine(), adapters);
    if let Some(h) = &herdr {
        h.set_evidence(hooks.evidence());
        h.start(core.clone());
    }
    hooks.start(core.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let auth = Arc::new(HookAuth::new(Credential {
        token: TOKEN.into(),
        generation: 1,
    }));
    let app = hooks.router(auth);
    tokio::spawn(async move {
        axum::serve(acd_adapters::hooks::endpoint::capped(listener, 128), app)
            .await
            .unwrap()
    });
    if let Some(h) = &herdr {
        let n = panes.len();
        until("herdr panes", || {
            (h.panes().read().unwrap().len() == n).then_some(())
        })
        .await;
    }
    Rig {
        core,
        hooks,
        herdr,
        fake,
        t3,
        port,
        home,
    }
}
pub async fn until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    for _ in 0..250 {
        if let Some(v) = f() {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}
pub fn fx(rel: &str) -> Value {
    let p = format!(
        "{}/../../tests/fixtures/hooks/{rel}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}
pub const SID: &str = "4e3885ee-7454-410d-a2e2-86f4dc2533c0";
impl Rig {
    pub fn sock(&self) -> String {
        self.fake
            .as_ref()
            .unwrap()
            .path()
            .to_string_lossy()
            .into_owned()
    }
    pub async fn hook(
        &self,
        agent: &str,
        event: &str,
        body: &Value,
        headers: &[(&str, &str)],
    ) -> (u16, String) {
        post(
            self.port,
            &format!("/hook/{agent}/{event}"),
            TOKEN,
            headers,
            &body.to_string(),
        )
        .await
    }
    pub async fn hold(&self, body: &Value, headers: &[(&str, &str)]) -> tokio::net::TcpStream {
        send(
            self.port,
            "/hook/claude/PermissionRequest",
            TOKEN,
            headers,
            &body.to_string(),
        )
        .await
    }
    pub fn agent(&self, id: &str) -> Option<Agent> {
        self.core.agent(id)
    }
}
pub const CLI: [(&str, &str); 1] = [("X-Claude-Entrypoint", "cli")];

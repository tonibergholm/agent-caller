#![allow(dead_code)]
use std::sync::Arc;
use std::time::Duration;

use acd_client::Timing;
use acd_client::fake::{FakeConn, FakeServer, Identity};
use acd_client::inbox::{Inbox, InboxOptions};
use acd_client::settings::{
    DEFAULT_HOTKEY, EndpointConfig, MachineEntry, MachineSecret, MemorySecrets, SecretStore, Settings,
};
use acd_client::view::InboxView;
use acd_voice::snapshot::Presentation;
use serde_json::{Value, json};

pub const TOKEN: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";

pub fn fast() -> Timing {
    let ms = Duration::from_millis;
    Timing {
        backoff_base: ms(20),
        backoff_max: ms(100),
        identity_retry: ms(200),
        idle_ping: ms(500),
        ping_timeout: ms(300),
        undo: ms(200),
        not_ready_interval: ms(50),
        connect_total: ms(3000),
        ..Timing::default()
    }
}

/// Machine "Box" (key m1), Claude agent a1 with request r1: Yes / No / Yes, always.
pub fn pending_agent() -> Value {
    json!({"id":"a1","machine_id":"m","source":"herdr","kind":"claude","title":"agent","cwd":null,"project":null,"state":"needs_you","state_since":"2026-10-08T00:00:00.000Z","failed":false,"last_message":null,"can_send":true,"pending":{"id":"r1","revision":1,"agent_id":"a1","kind":"permission","status":"open","answer_path":"hook","title":"Bash","detail":"npm test","options":[{"id":"allow","label":"Yes","description":null,"effect":"allow"},{"id":"deny","label":"No","description":null,"effect":"deny"},{"id":"always","label":"Yes, always","description":"Bash(npm test:*)","effect":"allow_always"}],"questions":null,"free_text":false,"raw_screen":null,"opened_at":"2026-10-08T00:00:00.000Z"}})
}

/// A working Claude agent a2 that takes a reply.
pub fn working_agent() -> Value {
    let mut a = pending_agent();
    a["id"] = "a2".into();
    a["state"] = "working".into();
    a["pending"] = Value::Null;
    a
}

/// An idle Codex agent a3; the panel hides it ("Show idle" is off).
pub fn idle_agent() -> Value {
    let mut a = working_agent();
    a["id"] = "a3".into();
    a["kind"] = "codex".into();
    a["state"] = "idle".into();
    a
}

pub struct InboxRig {
    pub f: FakeServer,
    pub c: FakeConn,
    pub inbox: Arc<Inbox>,
}

pub async fn inbox_rig(agents: Vec<Value>) -> InboxRig {
    inbox_rig_timed(agents, fast()).await
}

pub async fn inbox_rig_timed(agents: Vec<Value>, timing: Timing) -> InboxRig {
    let f = FakeServer::start(TOKEN, Identity::Paired).await;
    let secrets = Arc::new(MemorySecrets::default());
    secrets
        .set("m1", &MachineSecret { token: TOKEN.into(), pin: f.pin() }.to_json())
        .unwrap();
    let settings = Settings {
        client_id: "00000000-0000-4000-8000-000000000001".into(),
        hotkey: DEFAULT_HOTKEY.into(),
        hotkey_migrated: true,
        show_idle: false,
        show_subagents: false,
        show_hook_only: false,
        machines: vec![MachineEntry {
            key: "m1".into(),
            name: "Box".into(),
            endpoint: EndpointConfig::Tcp { host: "127.0.0.1".into(), port: f.port() },
        }],
    };
    let inbox = Inbox::new(
        settings,
        secrets,
        InboxOptions { timing, settings_path: None, client_name: "t".into(), client_version: "0".into() },
    );
    inbox.start();
    let c = f.next_connection().await;
    let n = agents.len();
    c.send(json!({"type":"snapshot","epoch":"e1","seq":1,"agents":agents})).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let s = inbox.stamped_view();
            if s.view.cards.len() + s.hidden_idle_cards.len() == n && !s.stamps.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the inbox shows the agents");
    InboxRig { f, c, inbox }
}

pub async fn view_until(inbox: &Inbox, what: &str, pred: impl Fn(&InboxView) -> bool) {
    let mut rx = inbox.view();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if pred(&rx.borrow()) {
                break;
            }
            rx.changed().await.unwrap();
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

pub fn shown(selected: Option<&str>, visible: &[&str]) -> Presentation {
    Presentation {
        epoch: "e".into(),
        version: 1,
        selected: selected.map(str::to_owned),
        visible: visible.iter().map(|s| (*s).to_owned()).collect(),
        machine_filter: None,
        done_collapsed: true,
    }
}

pub fn frames(f: &FakeServer, kind: &str) -> Vec<Value> {
    f.received().into_iter().filter(|v| v["type"] == kind).collect()
}

pub async fn frame(f: &FakeServer, kind: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(v) = frames(f, kind).into_iter().next() {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no {kind} frame"))
}

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use acd_voice::hud::Hud;
use acd_voice::router::model::IntentModel;
use acd_voice_client::{Controller, HudSink};
use tokio::sync::mpsc;

pub type HudHook = Box<dyn Fn(&Hud) + Send + Sync>;

/// Records what the controller publishes. With `acks`, it plays the HUD
/// webview: every state awaiting display is acknowledged, after the
/// publish returns (never from inside it).
#[derive(Default)]
pub struct Sink {
    pub huds: Mutex<Vec<(u64, Hud)>>,
    pub selects: Mutex<Vec<String>>,
    acks: Mutex<Option<mpsc::UnboundedSender<u64>>>,
    /// Refuse every state that awaits display, as a failed emit would.
    pub refuse_display: AtomicBool,
    /// Refuse a spoken-reply state, as a HUD that cannot show it would.
    pub refuse_speaking: AtomicBool,
    /// Runs for every state published, before it is acknowledged.
    pub on_hud: Mutex<Option<HudHook>>,
    pub mics: Mutex<Vec<bool>>,
    pub acked: Mutex<Vec<u64>>,
    /// States whose spoken reply ended (plan V11).
    pub ended: Mutex<Vec<u64>>,
}

impl HudSink for Sink {
    fn speech_ended(&self, seq: u64) {
        self.ended.lock().unwrap().push(seq);
    }

    fn listening(&self, on: bool) {
        self.mics.lock().unwrap().push(on);
    }

    fn hud(&self, seq: u64, hud: &Hud) -> bool {
        if hud.awaiting_display().is_some() && self.refuse_display.load(Ordering::SeqCst) {
            return false;
        }
        if matches!(hud, Hud::Speaking { .. }) && self.refuse_speaking.load(Ordering::SeqCst) {
            return false;
        }
        self.huds.lock().unwrap().push((seq, hud.clone()));
        if let Some(f) = self.on_hud.lock().unwrap().as_ref() {
            f(hud);
        }
        if let (Some(id), Some(tx)) = (hud.awaiting_display(), self.acks.lock().unwrap().as_ref()) {
            let _ = tx.send(id);
        }
        true
    }
    fn select(&self, card_key: &str) {
        self.selects.lock().unwrap().push(card_key.to_owned());
    }
}

impl Sink {
    pub fn last(&self) -> Option<Hud> {
        self.huds.lock().unwrap().last().map(|(_, h)| h.clone())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Display {
    /// The HUD acknowledges everything it is shown.
    Acks,
    /// The HUD never answers.
    Silent,
}

pub struct Rig {
    pub f: acd_client::fake::FakeServer,
    pub c: acd_client::fake::FakeConn,
    pub inbox: Arc<acd_client::inbox::Inbox>,
    pub voice: Arc<Controller>,
    pub sink: Arc<Sink>,
}

pub async fn rig(agents: Vec<Value>, model: Option<Arc<dyn IntentModel>>, display: Display) -> Rig {
    rig_timed(agents, model, display, fast()).await
}

pub async fn rig_timed(agents: Vec<Value>, model: Option<Arc<dyn IntentModel>>, display: Display, timing: Timing) -> Rig {
    let InboxRig { f, c, inbox } = inbox_rig_timed(agents, timing).await;
    let sink = Arc::new(Sink::default());
    let voice = Controller::start(inbox.clone(), model, sink.clone(), true);
    if display == Display::Acks {
        let (tx, mut rx) = mpsc::unbounded_channel();
        *sink.acks.lock().unwrap() = Some(tx);
        let weak = Arc::downgrade(&voice);
        let acks = sink.clone();
        tokio::spawn(async move {
            while let Some(id) = rx.recv().await {
                let Some(voice) = weak.upgrade() else { return };
                voice.shown(id);
                acks.acked.lock().unwrap().push(id);
            }
        });
    }
    Rig { f, c, inbox, voice, sink }
}

pub async fn hud_until(sink: &Sink, what: &str, pred: impl Fn(&Hud) -> bool) -> Hud {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(h) = sink.last().filter(|h| pred(h)) {
                return h;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

use acd_voice::listen::ListenEvent;
use acd_voice::listen::fake::FakeSpeechInput;
use acd_voice::text::VoiceText;

/// `rig` with a scripted microphone and transcriber attached.
pub async fn speech_rig(agents: Vec<Value>) -> (Rig, Arc<FakeSpeechInput>) {
    speech_rig_with(agents, None, Display::Acks).await
}

/// `speech_rig` with a 30 s undo window, so a test can act on a countdown
/// before it fires without racing the clock.
pub async fn speech_rig_slow(agents: Vec<Value>, model: Option<Arc<dyn IntentModel>>) -> (Rig, Arc<FakeSpeechInput>) {
    let r = rig_timed(agents, model, Display::Acks, Timing { undo: Duration::from_secs(30), ..fast() }).await;
    let speech = Arc::new(FakeSpeechInput::default());
    r.voice.attach_speech(speech.clone());
    (r, speech)
}

pub async fn speech_rig_with(
    agents: Vec<Value>,
    model: Option<Arc<dyn IntentModel>>,
    display: Display,
) -> (Rig, Arc<FakeSpeechInput>) {
    let r = rig(agents, model, display).await;
    let speech = Arc::new(FakeSpeechInput::default());
    r.voice.attach_speech(speech.clone());
    (r, speech)
}

/// Waits until the HUD acknowledged proposal `id` (no fixed sleep).
pub async fn acked_until(sink: &Sink, id: u64) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !sink.acked.lock().unwrap().contains(&id) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the HUD acknowledged the proposal");
}

/// A final transcript of 900 ms with 600 ms of voice.
pub fn final_(generation: u64, text: &str) -> ListenEvent {
    ListenEvent::Final {
        generation,
        text: VoiceText::new(text),
        audio: Duration::from_millis(900),
        voiced: Duration::from_millis(600),
    }
}

use acd_voice::speak::fake::FakeSpeaker;

/// `rig` with a recording speaker attached.
pub async fn speaking_rig(agents: Vec<Value>) -> (Rig, Arc<FakeSpeaker>) {
    let r = rig(agents, None, Display::Acks).await;
    let speaker = Arc::new(FakeSpeaker::default());
    r.voice.attach_speaker(speaker.clone());
    (r, speaker)
}

/// The seq of the newest published state.
pub fn last_seq(sink: &Sink) -> u64 {
    sink.huds.lock().unwrap().last().map(|(s, _)| *s).unwrap_or(0)
}

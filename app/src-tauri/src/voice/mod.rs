//! Voice step 1 (voice spec §11 item 1): the typed command that stands in
//! for audio, the HUD, and the presentation snapshot.
pub mod commands;
#[cfg(target_os = "macos")]
pub mod bridge;
pub mod hud;
pub mod permissions;
pub mod settings;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use acd_voice::hud::Hud;
use acd_voice_client::{Controller, HudSink};
use tauri::{AppHandle, Emitter, Manager};

pub struct VoiceState {
    pub controller: Arc<Controller>,
    pub settings: Mutex<settings::VoiceSettings>,
    pub path: Option<PathBuf>,
    #[cfg(target_os = "macos")]
    pub speech: bridge::Speech,
}

/// HUD states go to the HUD webview; selections go to the inbox panel.
/// It only emits: it runs under the controller's executor lock.
pub struct TauriSink {
    app: AppHandle,
}

impl TauriSink {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl HudSink for TauriSink {
    fn hud(&self, seq: u64, state: &Hud) -> bool {
        hud::present(&self.app, seq, state)
    }

    fn select(&self, card_key: &str) {
        crate::panel::show(&self.app);
        // Broadcast; only the inbox panel listens.
        if let Err(error) = self.app.emit("voice://select", card_key) {
            tracing::error!(%error, "could not emit the voice selection");
        }
    }
}

/// Handles one event from the Swift bridge, on the async runtime.
#[cfg(target_os = "macos")]
pub fn dispatch(app: &AppHandle, ev: bridge::BridgeEvent) {
    use bridge::BridgeEvent as E;
    let voice = app.state::<VoiceState>();
    match ev {
        E::Listen(e) => voice.controller.speech_event(e),
        E::Capabilities { caps, locale } => {
            voice.speech.set_capabilities(caps, locale);
            emit_status(app);
        }
        E::Permission { .. } | E::Assets { .. } => emit_status(app),
        E::System => voice.controller.system_stop(),
        E::Capture { generation, on } => voice.controller.capture(generation, on),
        E::Overflow => voice.controller.overflow(),
        // Task 7B wires the hold key.
        E::Hold { .. } | E::OtherKey { .. } | E::TapDisabled { .. } => {}
        E::Probe(report) => {
            // Only the settings window shows the probe report.
            if let Err(error) = app.emit_to("settings", "voice://probe", report) {
                tracing::error!(%error, "could not emit the probe report");
            }
        }
    }
}

/// Tells Settings to fetch the voice status again (Task 9).
#[cfg(target_os = "macos")]
pub fn emit_status(app: &AppHandle) {
    if let Err(error) = app.emit("voice://status", ()) {
        tracing::error!(%error, "could not emit the voice status");
    }
}

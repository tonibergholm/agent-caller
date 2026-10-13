//! Voice step 1 (voice spec §11 item 1): the typed command that stands in
//! for audio, the HUD, and the presentation snapshot.
pub mod commands;
#[cfg(target_os = "macos")]
pub mod bridge;
#[cfg(target_os = "macos")]
pub mod model;
#[cfg(target_os = "macos")]
pub mod speaker;
pub mod hud;
pub mod permissions;
pub mod settings;
pub mod status;
#[cfg(target_os = "macos")]
pub mod trigger;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use acd_voice::hud::Hud;
use acd_voice_client::{Controller, HudSink};
use tauri::{AppHandle, Emitter, Manager};

pub struct VoiceState {
    pub controller: Arc<Controller>,
    pub settings: Mutex<settings::VoiceSettings>,
    pub path: Option<PathBuf>,
    /// The toggle shortcut's last registration error, for Settings.
    pub trigger_error: Mutex<Option<String>>,
    #[cfg(target_os = "macos")]
    pub speech: bridge::Speech,
    #[cfg(target_os = "macos")]
    pub model: Arc<model::MacIntentModel>,
    /// The English voices, as Swift last reported them (plan V16).
    #[cfg(target_os = "macos")]
    pub voices: Mutex<Vec<status::VoiceOption>>,
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

    /// Runs under the controller's lock: only schedules (plan V11).
    fn speech_ended(&self, seq: u64) {
        hud::fade_after_speech(&self.app, seq);
    }

    fn select(&self, card_key: &str) {
        crate::panel::show(&self.app);
        // Broadcast; only the inbox panel listens.
        if let Err(error) = self.app.emit("voice://select", card_key) {
            tracing::error!(%error, "could not emit the voice selection");
        }
    }

    /// Runs under the controller's lock: only dispatches (tray::render).
    fn listening(&self, on: bool) {
        crate::tray::set_listening(&self.app, on);
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
        E::Permission { which, .. } => {
            trigger::refresh(app);
            emit_status(app);
            if permissions::answered_in_place(which) {
                crate::settings_window::raise(app);
            }
        }
        E::Assets { .. } => emit_status(app),
        E::System => voice.controller.system_stop(),
        E::Capture { generation, on } => voice.controller.capture(generation, on),
        E::Overflow => {
            voice.controller.overflow();
            // The lost events may include the last availability report, and
            // the last voices list.
            voice.model.overflowed();
            speaker::query_voices(true);
        }
        E::Spoken { generation } => voice.controller.spoken(generation),
        E::Voices(list) => {
            *voice.voices.lock().unwrap() = list;
            emit_status(app);
        }
        E::ModelReply { id, reply } => {
            voice.model.deliver(id, reply);
        }
        E::ModelCaps(state) => {
            voice.model.set_state(state);
            emit_status(app);
        }
        // Only the current tap's events count, checked and delivered under
        // one lock with tap replacement (S26).
        E::Hold { tap, down } => {
            trigger::TAP.deliver(tap, || voice.controller.hold(down, crate::panel::is_visible()));
        }
        E::OtherKey { tap } => {
            trigger::TAP.deliver(tap, || voice.controller.other_key());
        }
        E::TapDisabled { tap, reenabled } => {
            trigger::TAP.deliver(tap, || {
                voice.controller.hold_lost();
                if !reenabled {
                    trigger::hold_off();
                    voice.controller.notice(acd_voice::refusal::Refusal::HoldKeyOff);
                    emit_status(app);
                }
            });
        }
        E::Probe(report) => {
            // Only the settings window shows the probe report.
            if let Err(error) = app.emit_to("settings", "voice://probe", report) {
                tracing::error!(%error, "could not emit the probe report");
            }
        }
    }
}

/// Tells Settings to fetch the voice status again.
#[cfg(target_os = "macos")]
pub fn emit_status(app: &AppHandle) {
    if let Err(error) = app.emit("voice://status", ()) {
        tracing::error!(%error, "could not emit the voice status");
    }
}

/// The app is quitting (spec §6.3): stop speech and drop model work.
#[cfg(target_os = "macos")]
pub fn shutdown(app: &AppHandle) {
    let voice = app.state::<VoiceState>();
    acd_voice::router::model::IntentModel::discard(voice.model.as_ref());
    acd_voice::speak::Speaker::stop(&speaker::MacSpeaker);
}

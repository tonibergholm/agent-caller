//! The voice HUD's state, published to the HUD webview (spec §4.4).
use acd_voice::hud::{Hud, HudEvent};
use tauri::{AppHandle, Emitter};

/// Broadcast like `inbox://view`; only the HUD webview listens. False if
/// the event could not be sent: a step waiting for display then fails
/// closed (D22).
pub fn present(app: &AppHandle, seq: u64, hud: &Hud) -> bool {
    // `HudEvent` is not `Clone`, which `emit` requires, so emit its JSON.
    let payload = match serde_json::to_value(HudEvent::new(seq, hud)) {
        Ok(payload) => payload,
        Err(_) => {
            tracing::error!("could not serialize the HUD state");
            return false;
        }
    };
    match app.emit("voice://hud", payload) {
        Ok(()) => true,
        Err(error) => {
            tracing::error!(%error, "could not emit the HUD state");
            false
        }
    }
}

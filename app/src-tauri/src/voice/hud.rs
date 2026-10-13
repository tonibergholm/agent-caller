//! The voice HUD (voice spec §4.4): a second non-activating panel, top
//! centre of the active screen, 18% from the top. It takes key focus only
//! while it waits for input (confirm, which-one, countdown); see D17.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use acd_voice::hud::{Hud, HudEvent, OutcomeView};
use acd_voice::refusal::Refusal;
use tauri::{AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Size, WebviewUrl, WebviewWindow};
use tauri_nspanel::{CollectionBehavior, ManagerExt, PanelBuilder, PanelLevel, StyleMask, tauri_panel};

use crate::frame::Frame;

pub const LABEL: &str = "hud";
/// The width, and the height before the HUD measured its content.
pub const SIZE: (f64, f64) = (560.0, 260.0);
/// The HUD's height follows its content (`fit_height`), within these.
pub const MIN_HEIGHT: f64 = 48.0;
pub const MAX_HEIGHT: f64 = 420.0;
/// Thinking shows only if the step is still thinking after this, so a
/// step that finishes at once does not flash it.
pub const THINKING_AFTER: Duration = Duration::from_millis(150);
/// Spec §4.4: the HUD fades 1.5 s after the step fires or speech ends.
pub const FADE_AFTER: Duration = Duration::from_millis(1500);
pub const PERMISSION_ERROR_FADE: Duration = Duration::from_secs(10);

/// Intentional key handoff (D17): set only when `apply` makes the HUD key
/// for input; cleared by `release_key`, or when the HUD resigns key on its
/// own because the user clicked elsewhere.
static HANDOFF: AtomicBool = AtomicBool::new(false);
/// The inbox panel was open when the handoff began; it gets key back.
static INBOX_WAS_OPEN: AtomicBool = AtomicBool::new(false);
/// The last state shown; a fade runs only if nothing newer arrived.
static LAST_SEQ: AtomicU64 = AtomicU64::new(0);

tauri_panel! {
    panel!(HudPanel {
        config: {
            can_become_key_window: true,
            can_become_main_window: false,
            is_floating_panel: true
        }
    })
    panel_event!(HudPanelEvents {
        window_did_resign_key(notification: &NSNotification) -> bool
    })
}

/// Main thread only; call once from `setup`, after `panel::create`.
pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let panel = PanelBuilder::<_, HudPanel>::new(app, LABEL)
        .url(WebviewUrl::App("index.html".into()))
        .title("Voice")
        .size(Size::Logical(LogicalSize::new(SIZE.0, SIZE.1)))
        .resizable(false)
        // Above the inbox panel, which is Floating.
        .level(PanelLevel::ModalPanel)
        .with_window(|w| w.decorations(false))
        .add_style_mask(StyleMask::empty().nonactivating_panel())
        .collection_behavior(
            CollectionBehavior::new()
                .can_join_all_spaces()
                .full_screen_auxiliary(),
        )
        .hides_on_deactivate(false)
        .corner_radius(8.0)
        .build()?;
    let events = HudPanelEvents::new();
    let handle = app.clone();
    events.window_did_resign_key(move |_| {
        // Our own release clears HANDOFF first, so this fires with it set
        // only when the user moved focus elsewhere: end the handoff and do
        // not pull the inbox panel back to key later.
        if HANDOFF.swap(false, Ordering::SeqCst) {
            INBOX_WAS_OPEN.store(false, Ordering::SeqCst);
        }
        // Clicking away from the HUD closes the inbox as clicking away from
        // the inbox does, also when a click, not a handoff, made it key.
        crate::panel::hide_if_key_left(&handle);
        false
    });
    panel.set_event_handler(Some(events.as_ref()));
    panel.hide();
    Ok(())
}

/// Whether the HUD holds key on purpose; the inbox panel then must not
/// hide on resigning key.
pub fn holds_key() -> bool {
    HANDOFF.load(Ordering::SeqCst)
}

/// Shows state `seq` (D22). One main-thread closure presents the panel,
/// checks that it is on screen, and only then emits the state, so the
/// webview can never render, and acknowledge, a step whose panel failed
/// to appear. A failure inside the closure emits nothing; the step then
/// fails closed on the controller's display timeout. False if the closure
/// could not be dispatched at all. Any thread; never blocks.
pub fn present(app: &AppHandle, seq: u64, hud: &Hud) -> bool {
    LAST_SEQ.store(seq, Ordering::SeqCst);
    let nothing = Hud::Hidden;
    let hud = if shows_nothing(hud, crate::panel::is_visible()) { &nothing } else { hud };
    let (hidden, wants_key) = (matches!(hud, Hud::Hidden), hud.wants_key());
    let event = match serde_json::to_value(HudEvent::new(seq, hud)) {
        Ok(v) => v,
        Err(error) => {
            tracing::error!(%error, "could not serialize the HUD state");
            return false;
        }
    };
    let handle = app.clone();
    let show = move || {
        let presented = apply(&handle, hidden, wants_key);
        if !should_emit(hidden, presented) {
            tracing::error!("the HUD panel is not on screen; the step is not shown");
            return;
        }
        // Broadcast like `inbox://view`; only the HUD webview listens.
        if let Err(error) = handle.emit("voice://hud", event) {
            tracing::error!(%error, "could not emit the HUD state");
        }
    };
    if let Some(after) = show_after(hud) {
        // Thinking needs no acknowledgement: shown later, and only if no
        // newer state arrived by then (checked in order on the main thread).
        let handle = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(after).await;
            if let Err(error) = handle.run_on_main_thread(move || {
                if fade_applies(LAST_SEQ.load(Ordering::SeqCst), seq) {
                    show();
                }
            }) {
                tracing::error!(%error, "could not dispatch the HUD update");
            }
        });
        return true;
    }
    if let Err(error) = app.run_on_main_thread(show) {
        tracing::error!(%error, "could not dispatch the HUD update");
        return false;
    }
    if hud.fades() {
        schedule_fade(app, seq, fade_delay(hud));
    }
    true
}

/// Hides the HUD `after` from now, unless a newer state replaced `seq`.
pub fn schedule_fade(app: &AppHandle, seq: u64, after: Duration) {
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(after).await;
        let inner = handle.clone();
        // The check runs on the main thread, in order with `present`'s
        // closures, so a newer state cannot be hidden by a late fade.
        if let Err(error) = handle.run_on_main_thread(move || {
            if !fade_applies(LAST_SEQ.load(Ordering::SeqCst), seq) {
                return;
            }
            apply(&inner, true, false);
            if let Err(error) = inner.emit("voice://hud-clear", seq) {
                tracing::error!(%error, "could not emit the HUD fade");
            }
        }) {
            tracing::error!(%error, "could not dispatch the HUD fade");
        }
    });
}

/// Spec §4.4: 1.5 s after speech ends.
pub fn fade_after_speech(app: &AppHandle, seq: u64) {
    schedule_fade(app, seq, FADE_AFTER);
}

/// D18: spoken text stays 1.5 s plus 50 ms per character, within 3–15 s.
pub fn fade_delay(hud: &Hud) -> Duration {
    match hud {
        Hud::Speaking { text, .. } => Duration::from_millis(1500 + 50 * text.char_len() as u64)
            .clamp(Duration::from_secs(3), Duration::from_secs(15)),
        // The recovery button sits on these; give time to read and click it.
        Hud::Error { refusal: Refusal::MicrophoneDenied | Refusal::SpeechDenied | Refusal::HoldKeyOff, .. } => {
            PERMISSION_ERROR_FADE
        }
        _ => FADE_AFTER,
    }
}

/// A selection needs no HUD while the inbox panel is open: the inbox
/// shows it.
fn shows_nothing(hud: &Hud, inbox_open: bool) -> bool {
    inbox_open && matches!(hud, Hud::Done { outcome: OutcomeView::Selected, .. })
}

/// How long to wait before showing `hud`, if at all.
fn show_after(hud: &Hud) -> Option<Duration> {
    matches!(hud, Hud::Thinking { .. }).then_some(THINKING_AFTER)
}

/// The HUD panel's height for content `measured` px tall.
pub fn fit_height(measured: f64) -> f64 {
    if measured.is_finite() { measured.ceil().clamp(MIN_HEIGHT, MAX_HEIGHT) } else { SIZE.1 }
}

/// Main thread. Fits the HUD to state `seq`'s content, unless a newer
/// state replaced it: a late fit must not shrink a step that waits for
/// input after `apply` gave it the full size.
pub fn fit_state(app: &AppHandle, seq: u64, height: f64) {
    if fade_applies(LAST_SEQ.load(Ordering::SeqCst), seq) {
        fit(app, height);
    }
}

/// Main thread. Resizes the HUD to `height`, keeping its top edge.
pub fn fit(app: &AppHandle, height: f64) {
    if let Some(window) = app.get_webview_window(LABEL)
        && let Err(error) = window.set_size(Size::Logical(LogicalSize::new(SIZE.0, fit_height(height))))
    {
        tracing::warn!(%error, "could not resize the HUD");
    }
}

/// Only the HUD webview may drive the executor (confirm, choose, cancel,
/// shown); a call from any other window is ignored.
pub fn is_hud(label: &str) -> bool {
    label == LABEL
}

/// Top centre of `screen`, 18% from its top (design doc, floating overlays).
pub fn place_on(screen: Frame, size: (f64, f64)) -> Frame {
    Frame {
        x: screen.x + (screen.w - size.0) / 2.0,
        y: screen.y + screen.h * 0.18,
        w: size.0,
        h: size.1,
    }
}

/// A fade hides the HUD only if no newer state arrived since it was set.
fn fade_applies(last: u64, seq: u64) -> bool {
    last == seq
}

/// A state is emitted only once its panel is on screen; hiding needs no
/// panel.
fn should_emit(hidden: bool, presented: bool) -> bool {
    hidden || presented
}

/// Main thread. True when the panel ended up as asked: hidden, or on
/// screen.
fn apply(app: &AppHandle, hidden: bool, wants_key: bool) -> bool {
    let Ok(panel) = app.get_webview_panel(LABEL) else {
        return false;
    };
    if hidden {
        release_key(app);
        return_key(app);
        panel.hide();
        return true;
    }
    // Placed once per appearance, so it does not jump screens mid-interaction.
    if !panel.is_visible()
        && let Some(window) = app.get_webview_window(LABEL)
    {
        position(&window);
    }
    if wants_key {
        // A step that waits for input is acknowledged as shown right after
        // it renders (D22), before the content fit can grow the panel: give
        // it the full default size first. The fit then shrinks or grows it.
        fit(app, SIZE.1);
        if !HANDOFF.swap(true, Ordering::SeqCst) {
            INBOX_WAS_OPEN.store(crate::panel::is_visible(), Ordering::SeqCst);
            panel.show_and_make_key();
            crate::panel::focus_webview(app, LABEL);
        }
    } else {
        release_key(app);
        return_key(app);
        // Front again if `return_key` ordered it out, in this same turn.
        panel.order_front_regardless();
    }
    panel.is_visible()
}

/// Ends the handoff: key goes back to the inbox panel if it was open.
/// `return_key` covers the rest.
fn release_key(app: &AppHandle) {
    if !HANDOFF.swap(false, Ordering::SeqCst) {
        return;
    }
    if INBOX_WAS_OPEN.swap(false, Ordering::SeqCst)
        && crate::panel::is_visible()
        && let Ok(inbox) = app.get_webview_panel("panel")
    {
        inbox.make_key_window();
    }
}

/// The HUD still has key when no handoff gave it back (inbox closed) or
/// when a click made it key. Key goes to the inbox panel if it is open;
/// else the HUD orders out, since AppKit has no reliable resign, and macOS
/// returns key to the frontmost app.
fn return_key(app: &AppHandle) {
    let Ok(hud) = app.get_webview_panel(LABEL) else {
        return;
    };
    if !hud.as_panel().isKeyWindow() {
        return;
    }
    if crate::panel::is_visible()
        && let Ok(inbox) = app.get_webview_panel("panel")
    {
        inbox.make_key_window();
    } else {
        hud.hide();
    }
}

/// On the screen under the cursor, else the primary.
fn position(window: &WebviewWindow) {
    let (full, _) = crate::panel::screen_frames(window);
    let Some(screen) = full.get(crate::panel::fallback_screen(window, &full)).copied() else {
        return;
    };
    let f = place_on(screen, SIZE);
    if let Err(error) = window.set_position(LogicalPosition::new(f.x, f.y)) {
        tracing::warn!(%error, "could not position the HUD");
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use acd_voice::text::VoiceText;

    #[test]
    fn a_state_is_emitted_only_after_its_panel_is_on_screen() {
        assert!(should_emit(false, true));
        assert!(!should_emit(false, false), "a panel that failed to appear: emit nothing, fail closed");
        assert!(should_emit(true, false), "hiding needs no panel");
    }

    #[test]
    fn a_late_fade_never_hides_a_newer_state() {
        assert!(fade_applies(7, 7));
        assert!(!fade_applies(8, 7));
    }

    #[test]
    fn only_the_hud_window_may_drive_the_executor() {
        assert!(is_hud("hud"));
        assert!(!is_hud("panel"));
        assert!(!is_hud("settings"));
        assert!(!is_hud(""));
    }

    #[test]
    fn the_hud_sits_top_centre_18_percent_down() {
        let f = place_on(Frame { x: 100.0, y: 0.0, w: 1600.0, h: 1000.0 }, (560.0, 260.0));
        assert_eq!(f, Frame { x: 620.0, y: 180.0, w: 560.0, h: 260.0 });
    }

    #[test]
    fn a_selection_shows_nothing_while_the_inbox_shows_it() {
        let selected = Hud::Done { proposal: None, outcome: OutcomeView::Selected };
        assert!(shows_nothing(&selected, true));
        assert!(!shows_nothing(&selected, false), "with the inbox closed the HUD says what was selected");
        assert!(!shows_nothing(&Hud::Done { proposal: None, outcome: OutcomeView::Cancelled }, true));
    }

    #[test]
    fn thinking_waits_so_quick_steps_do_not_flash_it() {
        assert_eq!(show_after(&Hud::Thinking { transcript: VoiceText::new("x"), id: 1 }), Some(THINKING_AFTER));
        assert_eq!(show_after(&Hud::Done { proposal: None, outcome: OutcomeView::Cancelled }), None);
        assert_eq!(show_after(&Hud::Hidden), None);
    }

    #[test]
    fn a_step_that_waits_for_input_starts_at_the_full_default_size() {
        // `apply` resets to SIZE.1 for these, which `fit_height` keeps.
        assert_eq!(fit_height(SIZE.1), SIZE.1);
        assert!(SIZE.1 > MIN_HEIGHT && SIZE.1 < MAX_HEIGHT);
    }

    #[test]
    fn the_hud_fits_its_content_within_bounds() {
        assert_eq!(fit_height(20.0), MIN_HEIGHT);
        assert_eq!(fit_height(90.5), 91.0);
        assert_eq!(fit_height(5000.0), MAX_HEIGHT);
        assert_eq!(fit_height(f64::NAN), SIZE.1, "a bad measurement keeps the default");
    }

    #[test]
    fn steps_fade_after_1_5_s_and_spoken_text_stays_long_enough_to_read() {
        assert_eq!(fade_delay(&Hud::Done { proposal: None, outcome: OutcomeView::Selected }), FADE_AFTER);
        assert_eq!(fade_delay(&Hud::Speaking { text: VoiceText::new("short"), audio: false }), Duration::from_secs(3));
        for refusal in [Refusal::MicrophoneDenied, Refusal::SpeechDenied, Refusal::HoldKeyOff] {
            assert_eq!(fade_delay(&Hud::Error { refusal, transcript: None }), PERMISSION_ERROR_FADE, "{refusal:?} has a button to click");
        }
        assert_eq!(fade_delay(&Hud::Error { refusal: Refusal::StillSending, transcript: None }), FADE_AFTER);
        assert_eq!(fade_delay(&Hud::Speaking { text: VoiceText::new("x".repeat(100)), audio: false }), Duration::from_millis(6500));
        assert_eq!(fade_delay(&Hud::Speaking { text: VoiceText::new("x".repeat(1000)), audio: false }), Duration::from_secs(15));
    }
}

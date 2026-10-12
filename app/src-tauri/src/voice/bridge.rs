//! The C contract with the Swift voice bridge
//! (`swift/Sources/VoiceBridgeC/include/voice_bridge.h`). Plan S1–S5: one
//! `'static` callback, a bounded queue, text copied before the callback
//! returns. Nothing here logs event text.
// The full contract is mirrored here; Tasks 5B to 7A use the constants and
// fields that Task 1A's probe does not.
#![allow(dead_code)]

use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::mpsc;

pub const VB_OK: i32 = 0;
pub const VB_ERR_MIC_DENIED: i32 = 1;
pub const VB_ERR_MIC_UNAVAILABLE: i32 = 2;
pub const VB_ERR_SPEECH_DENIED: i32 = 3;
pub const VB_ERR_SPEECH_UNAVAILABLE: i32 = 4;
pub const VB_ERR_ASSETS_MISSING: i32 = 5;
pub const VB_ERR_BUSY: i32 = 6;
pub const VB_ERR_FAILED: i32 = 7;
pub const VB_ERR_INPUT_MONITORING_DENIED: i32 = 8;
pub const VB_ERR_TAP_FAILED: i32 = 9;
pub const VB_ERR_NOT_INITIALIZED: i32 = 10;

pub const VB_EV_PARTIAL: u32 = 1;
pub const VB_EV_FINAL: u32 = 2;
pub const VB_EV_ERROR: u32 = 3;
pub const VB_EV_CAPS: u32 = 4;
pub const VB_EV_PERMISSION: u32 = 5;
pub const VB_EV_HOLD: u32 = 6;
pub const VB_EV_OTHER_KEY: u32 = 7;
pub const VB_EV_TAP_DISABLED: u32 = 8;
pub const VB_EV_SYSTEM: u32 = 9;
pub const VB_EV_ASSETS: u32 = 10;
pub const VB_EV_PROBE: u32 = 11;
pub const VB_EV_CAPTURE: u32 = 12;
pub const VB_EV_OVERFLOW: u32 = 13;

pub const VB_PERM_MICROPHONE: u32 = 1;
pub const VB_PERM_SPEECH: u32 = 2;
pub const VB_PERM_INPUT_MONITORING: u32 = 3;

pub const VB_PS_NOT_DETERMINED: u32 = 0;
pub const VB_PS_GRANTED: u32 = 1;
pub const VB_PS_DENIED: u32 = 2;
pub const VB_PS_RESTRICTED: u32 = 3;

pub const VB_UA_NONE: u32 = 0;
pub const VB_UA_OS_TOO_OLD: u32 = 1;
pub const VB_UA_SDK_TOO_OLD: u32 = 2;
pub const VB_UA_TRANSCRIBER_UNSUPPORTED: u32 = 3;
pub const VB_UA_LOCALE_UNSUPPORTED: u32 = 4;
pub const VB_UA_ASSETS_MISSING: u32 = 5;

pub const VB_SYS_SCREEN_LOCKED: u32 = 1;
pub const VB_SYS_WILL_SLEEP: u32 = 2;
pub const VB_SYS_SCREENS_SLEEP: u32 = 3;

/// Mirrors `struct vb_event`; the layout is pinned by tests on both sides.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VbEvent {
    pub generation: u64,
    pub kind: u32,
    pub code: u32,
    pub value: u32,
    pub value2: u32,
    pub text: *const u8,
    pub text_len: usize,
    pub audio_ms: u64,
    pub voiced_ms: u64,
}

impl VbEvent {
    pub const EMPTY: Self = Self {
        generation: 0,
        kind: 0,
        code: 0,
        value: 0,
        value2: 0,
        text: std::ptr::null(),
        text_len: 0,
        audio_ms: 0,
        voiced_ms: 0,
    };
}

pub type VbEventFn = unsafe extern "C" fn(ctx: *mut c_void, event: *const VbEvent);

mod ffi {
    use super::{VbEventFn, c_void};

    unsafe extern "C" {
        pub fn vb_init(ctx: *mut c_void, on_event: VbEventFn) -> i32;
        pub fn vb_probe_run(step: u32);
    }
}

/// What the bridge reported, copied out of the callback. `Debug` shows no
/// voice text: transcripts are `VoiceText` (Task 6).
#[derive(Debug, PartialEq)]
pub enum BridgeEvent {
    /// A spike report: statuses, counts and timings, no transcript.
    Probe(String),
}

/// The queue from the callback to the app (plan S5).
pub const QUEUE: usize = 64;
static TX: OnceLock<mpsc::Sender<BridgeEvent>> = OnceLock::new();
/// Events dropped because the queue was full.
pub static DROPPED: AtomicU64 = AtomicU64::new(0);

/// Copies `ev` into an owned event; `None` for kinds the app ignores.
///
/// # Safety
/// `ev.text` is null, or points to `ev.text_len` readable bytes for the
/// duration of this call (header contract).
pub unsafe fn decode(ev: &VbEvent) -> Option<BridgeEvent> {
    let text = || {
        if ev.text.is_null() || ev.text_len == 0 {
            return String::new();
        }
        // SAFETY: the caller guarantees `text_len` readable bytes.
        let bytes = unsafe { std::slice::from_raw_parts(ev.text, ev.text_len) };
        String::from_utf8_lossy(bytes).into_owned()
    };
    match ev.kind {
        VB_EV_PROBE => Some(BridgeEvent::Probe(text())),
        _ => None,
    }
}

/// Swift calls this from its event queue. It copies and never blocks.
unsafe extern "C" fn on_event(_ctx: *mut c_void, event: *const VbEvent) {
    // SAFETY: Swift passes a valid event for the duration of this call.
    let Some(ev) = (unsafe { event.as_ref() }) else {
        return;
    };
    // SAFETY: same contract for the text it points to.
    let Some(decoded) = (unsafe { decode(ev) }) else {
        return;
    };
    if let Some(tx) = TX.get()
        && tx.try_send(decoded).is_err()
    {
        DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Starts the bridge once. `None` if it was started before or failed.
pub fn start() -> Option<mpsc::Receiver<BridgeEvent>> {
    let (tx, rx) = mpsc::channel(QUEUE);
    TX.set(tx).ok()?;
    // SAFETY: `on_event` is a 'static function and the context is unused (S3).
    let code = unsafe { ffi::vb_init(std::ptr::null_mut(), on_event) };
    if code != VB_OK {
        tracing::error!(code, "the voice bridge did not start");
        return None;
    }
    Some(rx)
}

/// Runs spike probe `step` (Task 1A); the report arrives as `Probe`.
pub fn probe(step: u32) {
    // SAFETY: takes a plain integer; returns at once.
    unsafe { ffi::vb_probe_run(step) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    fn the_event_layout_matches_the_header() {
        assert_eq!(size_of::<VbEvent>(), 56);
        assert_eq!(offset_of!(VbEvent, kind), 8);
        assert_eq!(offset_of!(VbEvent, text), 24);
        assert_eq!(offset_of!(VbEvent, text_len), 32);
        assert_eq!(offset_of!(VbEvent, audio_ms), 40);
        assert_eq!(offset_of!(VbEvent, voiced_ms), 48);
    }

    #[test]
    fn a_probe_report_is_copied_out_of_the_callback() {
        let json = br#"{"step":1,"microphone":"granted"}"#.to_vec();
        let ev = VbEvent { kind: VB_EV_PROBE, text: json.as_ptr(), text_len: json.len(), ..VbEvent::EMPTY };
        let decoded = unsafe { decode(&ev) };
        drop(json); // the borrowed bytes may go away once the callback returns
        assert_eq!(decoded, Some(BridgeEvent::Probe(r#"{"step":1,"microphone":"granted"}"#.into())));
    }

    #[test]
    fn null_text_and_unknown_kinds_are_safe() {
        assert_eq!(unsafe { decode(&VbEvent { kind: VB_EV_PROBE, ..VbEvent::EMPTY }) }, Some(BridgeEvent::Probe(String::new())));
        assert_eq!(unsafe { decode(&VbEvent { kind: 999, ..VbEvent::EMPTY }) }, None);
    }
}

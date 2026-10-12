//! The C contract with the Swift voice bridge
//! (`swift/Sources/VoiceBridgeC/include/voice_bridge.h`). Plan S1–S5: one
//! `'static` callback, a bounded queue, text copied before the callback
//! returns. Nothing here logs event text.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use acd_voice::listen::{Capabilities, ListenError, ListenEvent, SpeechInput, Unavailable};
use acd_voice::text::VoiceText;
use tokio::sync::mpsc;

use super::permissions::{Permission, PermissionState, SPEECH_PERMISSION_REQUIRED};

pub const VB_OK: i32 = 0;
pub const VB_ERR_MIC_DENIED: i32 = 1;
pub const VB_ERR_MIC_UNAVAILABLE: i32 = 2;
pub const VB_ERR_SPEECH_DENIED: i32 = 3;
pub const VB_ERR_SPEECH_UNAVAILABLE: i32 = 4;
pub const VB_ERR_ASSETS_MISSING: i32 = 5;
#[allow(dead_code)]
pub const VB_ERR_BUSY: i32 = 6;
#[allow(dead_code)]
pub const VB_ERR_FAILED: i32 = 7;
pub const VB_ERR_INPUT_MONITORING_DENIED: i32 = 8;
// Any other code maps to it in `trigger::tap_result`; tests name it.
#[allow(dead_code)]
pub const VB_ERR_TAP_FAILED: i32 = 9;
#[allow(dead_code)]
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

#[allow(dead_code)]
pub const VB_PERM_MICROPHONE: u32 = 1;
#[allow(dead_code)]
pub const VB_PERM_SPEECH: u32 = 2;
#[allow(dead_code)]
pub const VB_PERM_INPUT_MONITORING: u32 = 3;

#[allow(dead_code)]
pub const VB_PS_NOT_DETERMINED: u32 = 0;
#[allow(dead_code)]
pub const VB_PS_GRANTED: u32 = 1;
#[allow(dead_code)]
pub const VB_PS_DENIED: u32 = 2;
#[allow(dead_code)]
pub const VB_PS_RESTRICTED: u32 = 3;

pub const VB_UA_NONE: u32 = 0;
pub const VB_UA_OS_TOO_OLD: u32 = 1;
pub const VB_UA_SDK_TOO_OLD: u32 = 2;
#[allow(dead_code)]
pub const VB_UA_TRANSCRIBER_UNSUPPORTED: u32 = 3;
pub const VB_UA_LOCALE_UNSUPPORTED: u32 = 4;
pub const VB_UA_ASSETS_MISSING: u32 = 5;

#[allow(dead_code)]
pub const VB_SYS_SCREEN_LOCKED: u32 = 1;
#[allow(dead_code)]
pub const VB_SYS_WILL_SLEEP: u32 = 2;
#[allow(dead_code)]
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
    // Test fixtures build events from it.
    #[cfg(test)]
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

pub(crate) mod ffi {
    use super::{VbEventFn, c_void};

    unsafe extern "C" {
        pub fn vb_init(ctx: *mut c_void, on_event: VbEventFn) -> i32;
        pub fn vb_probe_run(step: u32);
        pub fn vb_permission_status(permission: u32) -> u32;
        pub fn vb_permission_request(permission: u32);
        pub fn vb_query_capabilities();
        pub fn vb_session_start(generation: u64, max_ms: u32, speech_required: u32) -> i32;
        pub fn vb_session_stop(generation: u64);
        pub fn vb_session_cancel(generation: u64);
        pub fn vb_install_assets();
        pub fn vb_tap_start(keycode: u16, down_mask: u64, out_tap: *mut u64) -> i32;
        pub fn vb_tap_stop();
    }
}

/// What the bridge reported, copied out of the callback. `Debug` never
/// shows a transcript: those are `VoiceText`.
#[derive(Debug, PartialEq)]
pub enum BridgeEvent {
    Listen(ListenEvent),
    Capabilities { caps: Capabilities, locale: Option<String> },
    Permission { which: Permission, state: PermissionState },
    /// The hold key went down or up.
    Hold { tap: u64, down: bool },
    /// Another key went down; which one is never sent (S11).
    OtherKey { tap: u64 },
    TapDisabled { tap: u64, reenabled: bool },
    /// The screen locked or the Mac is going to sleep; Swift already
    /// dropped the session.
    System,
    Assets { ok: bool },
    /// Swift's microphone started or stopped (S18).
    Capture { generation: u64, on: bool },
    /// Events were lost (S5); the controller fails closed.
    Overflow,
    /// A spike report: statuses, counts and timings, no transcript.
    Probe(String),
}

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
    let g = ev.generation;
    match ev.kind {
        VB_EV_PARTIAL => Some(BridgeEvent::Listen(ListenEvent::Partial { generation: g, text: VoiceText::new(text()) })),
        VB_EV_FINAL => Some(BridgeEvent::Listen(ListenEvent::Final {
            generation: g,
            text: VoiceText::new(text()),
            audio: Duration::from_millis(ev.audio_ms),
            voiced: Duration::from_millis(ev.voiced_ms),
        })),
        VB_EV_ERROR => Some(BridgeEvent::Listen(ListenEvent::Error { generation: g, error: listen_error(ev.code as i32) })),
        VB_EV_CAPS => {
            let locale = text();
            Some(BridgeEvent::Capabilities {
                caps: capabilities(ev.code, ev.value, ev.value2),
                locale: (!locale.is_empty()).then_some(locale),
            })
        }
        VB_EV_PERMISSION => Permission::from_code(ev.code)
            .map(|which| BridgeEvent::Permission { which, state: PermissionState::from_code(ev.value) }),
        VB_EV_HOLD => Some(BridgeEvent::Hold { tap: g, down: ev.value != 0 }),
        VB_EV_OTHER_KEY => Some(BridgeEvent::OtherKey { tap: g }),
        VB_EV_TAP_DISABLED => Some(BridgeEvent::TapDisabled { tap: g, reenabled: ev.value != 0 }),
        VB_EV_SYSTEM => Some(BridgeEvent::System),
        VB_EV_ASSETS => Some(BridgeEvent::Assets { ok: ev.code == VB_OK as u32 }),
        VB_EV_CAPTURE => Some(BridgeEvent::Capture { generation: g, on: ev.value != 0 }),
        VB_EV_OVERFLOW => Some(BridgeEvent::Overflow),
        VB_EV_PROBE => Some(BridgeEvent::Probe(text())),
        _ => None,
    }
}

/// A Swift error code as a classified error (spec §6.3).
pub fn listen_error(code: i32) -> ListenError {
    match code {
        VB_ERR_MIC_DENIED => ListenError::MicrophoneDenied,
        VB_ERR_MIC_UNAVAILABLE => ListenError::MicrophoneUnavailable,
        VB_ERR_SPEECH_DENIED => ListenError::SpeechDenied,
        VB_ERR_SPEECH_UNAVAILABLE => ListenError::SpeechUnavailable,
        VB_ERR_ASSETS_MISSING => ListenError::AssetsMissing,
        _ => ListenError::Failed,
    }
}

pub fn start_result(code: i32) -> Result<(), ListenError> {
    if code == VB_OK { Ok(()) } else { Err(listen_error(code)) }
}

pub fn capabilities(code: u32, partials: u32, sample_rate: u32) -> Capabilities {
    let unavailable = match code {
        VB_UA_NONE => None,
        VB_UA_OS_TOO_OLD => Some(Unavailable::OsTooOld),
        VB_UA_SDK_TOO_OLD => Some(Unavailable::SdkTooOld),
        VB_UA_LOCALE_UNSUPPORTED => Some(Unavailable::LocaleUnsupported),
        VB_UA_ASSETS_MISSING => Some(Unavailable::AssetsMissing),
        _ => Some(Unavailable::TranscriberUnsupported),
    };
    Capabilities { unavailable, streams_partials: partials != 0, sample_rate }
}

/// The queue from the callback to the app (plan S5).
pub const QUEUE: usize = 64;

/// The link from the callback to the app (plan S5): a bounded queue plus
/// an overflow latch. The callback never blocks, and a lost event is never
/// silent.
pub struct Link {
    tx: mpsc::Sender<BridgeEvent>,
    overflow: AtomicBool,
    wake: tokio::sync::Notify,
}

impl Link {
    pub fn new() -> (Arc<Link>, mpsc::Receiver<BridgeEvent>) {
        Self::with_capacity(QUEUE)
    }

    pub fn with_capacity(n: usize) -> (Arc<Link>, mpsc::Receiver<BridgeEvent>) {
        let (tx, rx) = mpsc::channel(n);
        (Arc::new(Link { tx, overflow: AtomicBool::new(false), wake: tokio::sync::Notify::new() }), rx)
    }
}

static LINK: OnceLock<Arc<Link>> = OnceLock::new();

/// Queues `ev` without waiting. A full queue latches an overflow and wakes
/// the pump (S5).
pub fn offer(link: &Link, ev: BridgeEvent) -> bool {
    if link.tx.try_send(ev).is_ok() {
        return true;
    }
    link.overflow.store(true, Ordering::SeqCst);
    link.wake.notify_one();
    false
}

/// Hands events to `f` in order. A latched overflow goes first, before the
/// next queued event: cancelling a little early is the safe side (S5).
pub async fn pump(link: Arc<Link>, mut rx: mpsc::Receiver<BridgeEvent>, mut f: impl FnMut(BridgeEvent)) {
    loop {
        let next = tokio::select! {
            ev = rx.recv() => ev,
            _ = link.wake.notified() => {
                if link.overflow.swap(false, Ordering::SeqCst) {
                    f(BridgeEvent::Overflow);
                }
                continue;
            }
        };
        if link.overflow.swap(false, Ordering::SeqCst) {
            f(BridgeEvent::Overflow);
        }
        match next {
            Some(ev) => f(ev),
            None => return,
        }
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
    if let Some(link) = LINK.get() {
        offer(link, decoded);
    }
}

/// Starts the bridge once, on the main thread. `None` if it was started
/// before or failed.
pub fn start() -> Option<(Arc<Link>, mpsc::Receiver<BridgeEvent>)> {
    let (link, rx) = Link::new();
    LINK.set(link.clone()).ok()?;
    // SAFETY: `on_event` is a 'static function and the context is unused (S3).
    let code = unsafe { ffi::vb_init(std::ptr::null_mut(), on_event) };
    if code != VB_OK {
        tracing::error!(code, "the voice bridge did not start");
        return None;
    }
    Some((link, rx))
}

/// Runs spike probe `step` (Task 1A); the report arrives as `Probe`.
pub fn probe(step: u32) {
    // SAFETY: takes a plain integer; returns at once.
    unsafe { ffi::vb_probe_run(step) }
}

/// `SpeechInput` over the Swift bridge (spec §4.1, plan S2). Every call
/// returns at once and never calls back into the controller (it runs under
/// the controller's lock); events come back through `start`'s receiver.
pub struct MacSpeech {
    caps: Mutex<(Capabilities, Option<String>)>,
}

impl Default for MacSpeech {
    fn default() -> Self {
        Self::new()
    }
}

impl MacSpeech {
    pub fn new() -> Self {
        Self { caps: Mutex::new((Capabilities::CHECKING, None)) }
    }

    pub fn set_capabilities(&self, caps: Capabilities, locale: Option<String>) {
        *self.caps.lock().unwrap() = (caps, locale);
    }

    /// The resolved speech locale, for Settings.
    // Read by Settings in Task 9.
    #[allow(dead_code)]
    pub fn locale(&self) -> Option<String> {
        self.caps.lock().unwrap().1.clone()
    }

    // Called by Settings commands in Task 9.
    #[allow(dead_code)]
    pub fn refresh(&self) {
        // SAFETY: no arguments; returns at once.
        unsafe { ffi::vb_query_capabilities() }
    }

    // Called by Settings commands in Task 9.
    #[allow(dead_code)]
    pub fn install_assets(&self) {
        // SAFETY: no arguments; returns at once.
        unsafe { ffi::vb_install_assets() }
    }
}

impl SpeechInput for MacSpeech {
    fn capabilities(&self) -> Capabilities {
        self.caps.lock().unwrap().0
    }

    fn start(&self, generation: u64, max: Duration) -> Result<(), ListenError> {
        let max_ms = u32::try_from(max.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: plain integers; returns at once (header contract).
        start_result(unsafe { ffi::vb_session_start(generation, max_ms, u32::from(SPEECH_PERMISSION_REQUIRED)) })
    }

    fn stop(&self, generation: u64) {
        // SAFETY: a plain integer; returns at once.
        unsafe { ffi::vb_session_stop(generation) }
    }

    fn cancel(&self, generation: u64) {
        // SAFETY: a plain integer; returns at once.
        unsafe { ffi::vb_session_cancel(generation) }
    }
}

/// Keeps the `Arc` type in one place for `VoiceState`.
pub type Speech = Arc<MacSpeech>;

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


    fn with_text(kind: u32, generation: u64, text: &[u8]) -> VbEvent {
        VbEvent { kind, generation, text: text.as_ptr(), text_len: text.len(), ..VbEvent::EMPTY }
    }

    #[test]
    fn session_events_keep_their_generation_and_copy_their_text() {
        let partial = unsafe { decode(&with_text(VB_EV_PARTIAL, 4, b"appr")) };
        assert_eq!(partial, Some(BridgeEvent::Listen(ListenEvent::Partial { generation: 4, text: VoiceText::new("appr") })));
        let fin = VbEvent { audio_ms: 900, voiced_ms: 600, value: 120, ..with_text(VB_EV_FINAL, 4, b"approve") };
        assert_eq!(
            unsafe { decode(&fin) },
            Some(BridgeEvent::Listen(ListenEvent::Final {
                generation: 4,
                text: VoiceText::new("approve"),
                audio: Duration::from_millis(900),
                voiced: Duration::from_millis(600),
            }))
        );
        let err = VbEvent { kind: VB_EV_ERROR, generation: 4, code: VB_ERR_MIC_UNAVAILABLE as u32, ..VbEvent::EMPTY };
        assert_eq!(
            unsafe { decode(&err) },
            Some(BridgeEvent::Listen(ListenEvent::Error { generation: 4, error: ListenError::MicrophoneUnavailable }))
        );
        let shown = format!("{:?}", unsafe { decode(&with_text(VB_EV_FINAL, 4, b"SECRET")) });
        assert!(!shown.contains("SECRET"), "{shown}");
    }

    #[test]
    fn capabilities_map_every_reason() {
        let ev = VbEvent { kind: VB_EV_CAPS, code: VB_UA_NONE, value: 1, value2: 16_000, ..with_text(VB_EV_CAPS, 0, b"en_GB") };
        assert_eq!(
            unsafe { decode(&ev) },
            Some(BridgeEvent::Capabilities {
                caps: Capabilities { unavailable: None, streams_partials: true, sample_rate: 16_000 },
                locale: Some("en_GB".into()),
            })
        );
        for (code, why) in [
            (VB_UA_OS_TOO_OLD, Unavailable::OsTooOld),
            (VB_UA_SDK_TOO_OLD, Unavailable::SdkTooOld),
            (VB_UA_TRANSCRIBER_UNSUPPORTED, Unavailable::TranscriberUnsupported),
            (VB_UA_LOCALE_UNSUPPORTED, Unavailable::LocaleUnsupported),
            (VB_UA_ASSETS_MISSING, Unavailable::AssetsMissing),
            (99, Unavailable::TranscriberUnsupported),
        ] {
            assert_eq!(capabilities(code, 0, 0).unavailable, Some(why), "code {code}");
        }
    }

    #[test]
    fn the_other_kinds_decode() {
        use super::super::permissions::{Permission, PermissionState};
        let e = |kind, code, value| VbEvent { kind, code, value, ..VbEvent::EMPTY };
        assert_eq!(
            unsafe { decode(&e(VB_EV_PERMISSION, VB_PERM_MICROPHONE, VB_PS_DENIED)) },
            Some(BridgeEvent::Permission { which: Permission::Microphone, state: PermissionState::Denied })
        );
        assert_eq!(unsafe { decode(&e(VB_EV_PERMISSION, 77, VB_PS_DENIED)) }, None, "an unknown permission");
        // Tap events carry the tap's generation in `generation` (S26).
        let tap = |kind, value| VbEvent { kind, value, generation: 9, ..VbEvent::EMPTY };
        assert_eq!(unsafe { decode(&tap(VB_EV_HOLD, 1)) }, Some(BridgeEvent::Hold { tap: 9, down: true }));
        assert_eq!(unsafe { decode(&tap(VB_EV_HOLD, 0)) }, Some(BridgeEvent::Hold { tap: 9, down: false }));
        assert_eq!(unsafe { decode(&tap(VB_EV_OTHER_KEY, 0)) }, Some(BridgeEvent::OtherKey { tap: 9 }));
        assert_eq!(
            unsafe { decode(&VbEvent { code: 1, ..tap(VB_EV_TAP_DISABLED, 0) }) },
            Some(BridgeEvent::TapDisabled { tap: 9, reenabled: false })
        );
        assert_eq!(unsafe { decode(&e(VB_EV_SYSTEM, VB_SYS_SCREEN_LOCKED, 0)) }, Some(BridgeEvent::System));
        assert_eq!(unsafe { decode(&e(VB_EV_ASSETS, 0, 0)) }, Some(BridgeEvent::Assets { ok: true }));
        assert_eq!(unsafe { decode(&e(VB_EV_ASSETS, VB_ERR_FAILED as u32, 0)) }, Some(BridgeEvent::Assets { ok: false }));
    }

    #[test]
    fn start_codes_map_to_listen_errors() {
        assert_eq!(start_result(VB_OK), Ok(()));
        assert_eq!(start_result(VB_ERR_MIC_DENIED), Err(ListenError::MicrophoneDenied));
        assert_eq!(start_result(VB_ERR_SPEECH_DENIED), Err(ListenError::SpeechDenied));
        assert_eq!(start_result(VB_ERR_ASSETS_MISSING), Err(ListenError::AssetsMissing));
        assert_eq!(start_result(VB_ERR_SPEECH_UNAVAILABLE), Err(ListenError::SpeechUnavailable));
        assert_eq!(start_result(VB_ERR_BUSY), Err(ListenError::Failed));
        assert_eq!(start_result(VB_ERR_NOT_INITIALIZED), Err(ListenError::Failed));
    }

    #[test]
    fn capture_and_overflow_decode() {
        let on = VbEvent { kind: VB_EV_CAPTURE, generation: 7, value: 1, ..VbEvent::EMPTY };
        assert_eq!(unsafe { decode(&on) }, Some(BridgeEvent::Capture { generation: 7, on: true }));
        let off = VbEvent { value: 0, ..on };
        assert_eq!(unsafe { decode(&off) }, Some(BridgeEvent::Capture { generation: 7, on: false }));
        assert_eq!(unsafe { decode(&VbEvent { kind: VB_EV_OVERFLOW, ..VbEvent::EMPTY }) }, Some(BridgeEvent::Overflow));
    }

    /// Review Focus 2, spec §4.2, plan S5: the queue is bounded, the
    /// callback never waits, and a lost event reaches the controller as
    /// `Overflow` before the next event.
    #[tokio::test]
    async fn an_overflow_is_reported_before_the_next_event() {
        use std::sync::{Arc, Mutex};
        let (link, rx) = Link::with_capacity(1);
        assert!(offer(&link, BridgeEvent::OtherKey { tap: 1 }));
        assert!(!offer(&link, BridgeEvent::Hold { tap: 1, down: false }), "full: dropped and latched");
        let got = Arc::new(Mutex::new(Vec::new()));
        let sink = got.clone();
        let task = tokio::spawn(pump(link.clone(), rx, move |ev| sink.lock().unwrap().push(ev)));
        tokio::time::timeout(Duration::from_secs(2), async {
            while got.lock().unwrap().len() < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the pump delivers");
        task.abort();
        assert_eq!(*got.lock().unwrap(), [BridgeEvent::Overflow, BridgeEvent::OtherKey { tap: 1 }]);
    }

    /// A partial can follow its own final in one outbox batch. The pump
    /// keeps the order and the recorder drops the late partial.
    #[tokio::test]
    async fn a_partial_after_its_final_is_ignored() {
        use acd_voice::listen::{Progress, Recorder};
        use std::sync::{Arc, Mutex};
        use std::time::Instant;
        let (link, rx) = Link::new();
        let ms = Duration::from_millis;
        let final_ev = ListenEvent::Final { generation: 1, text: VoiceText::new("approve"), audio: ms(900), voiced: ms(600) };
        assert!(offer(&link, BridgeEvent::Listen(final_ev)));
        assert!(offer(&link, BridgeEvent::Listen(ListenEvent::Partial { generation: 1, text: VoiceText::new("appr") })));
        let t0 = Instant::now();
        let recorder = Arc::new(Mutex::new(Recorder::default()));
        recorder.lock().unwrap().start(1, t0);
        recorder.lock().unwrap().stop(t0);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (r, s) = (recorder.clone(), seen.clone());
        let task = tokio::spawn(pump(link.clone(), rx, move |ev| {
            if let BridgeEvent::Listen(ev) = ev {
                s.lock().unwrap().push(r.lock().unwrap().event(ev, t0));
            }
        }));
        tokio::time::timeout(Duration::from_secs(2), async {
            while seen.lock().unwrap().len() < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the pump delivers");
        task.abort();
        let seen = seen.lock().unwrap();
        assert!(matches!(seen[0], Some(Progress::Done(_))));
        assert_eq!(seen[1], None, "a partial after the final of its generation is dropped");
    }
}

//! The voice triggers (spec §4.1, §6.2): the listen-only hold-key tap and
//! the toggle shortcut. Both run only while voice is on (plan S12).
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

use tauri::{AppHandle, Manager};

use super::VoiceState;
use super::permissions::{self, Permission, PermissionState, TAP_NEEDS_RELAUNCH};
use super::settings::{HoldKey, VoiceSettings};

static HOLD_ACTIVE: AtomicBool = AtomicBool::new(false);

/// The generation of the tap whose events count (plan S26). Swift tags every
/// tap event with its tap's generation. One mutex covers both the check and
/// the delivery of an event (`deliver`), and the whole replacement: retire,
/// reset the hold state, swap the tap, install (`replace`). So an old event
/// is either delivered completely before a replacement, whose reset then
/// cancels what it started, or rejected after it. It can neither slip in
/// between the reset and the retirement, nor pass the check and deliver after
/// the replacement. Lock order: this gate, then the controller's core lock.
#[derive(Debug)]
pub struct TapGate {
    current: Mutex<u64>,
    waiting: AtomicUsize,
}

impl TapGate {
    pub const fn new() -> Self {
        Self { current: Mutex::new(0), waiting: AtomicUsize::new(0) }
    }

    /// Runs `deliver` under the gate if `tap` is the installed tap.
    pub fn deliver(&self, tap: u64, deliver: impl FnOnce()) -> bool {
        let current = self.enter();
        if tap == 0 || *current != tap {
            return false;
        }
        deliver();
        true
    }

    /// Retires the old tap, runs `reset` (the controller's `hold_lost`), then
    /// `swap` (stop the old tap, start the new one; it returns the new
    /// generation, or 0 when no tap runs), and installs that generation.
    pub fn replace(&self, reset: impl FnOnce(), swap: impl FnOnce() -> u64) {
        let mut current = self.enter();
        *current = 0;
        reset();
        *current = swap();
    }

    /// Callers blocked on the gate right now, for barrier tests.
    #[cfg(test)]
    pub fn waiting_for_test(&self) -> usize {
        self.waiting.load(Ordering::SeqCst)
    }

    fn enter(&self) -> MutexGuard<'_, u64> {
        self.waiting.fetch_add(1, Ordering::SeqCst);
        let guard = self.current.lock().unwrap();
        self.waiting.fetch_sub(1, Ordering::SeqCst);
        guard
    }
}

impl Default for TapGate {
    fn default() -> Self {
        Self::new()
    }
}

pub static TAP: TapGate = TapGate::new();

/// macOS virtual key code, and the flag bit that is set while that key is down.
pub fn mac_key(key: HoldKey) -> Option<(u16, u64)> {
    match key {
        HoldKey::RightOption => Some((61, 0x0000_0040)),  // NX_DEVICERALTKEYMASK
        HoldKey::LeftOption => Some((58, 0x0000_0020)),   // NX_DEVICELALTKEYMASK
        HoldKey::RightCommand => Some((54, 0x0000_0010)), // NX_DEVICERCMDKEYMASK
        HoldKey::RightControl => Some((62, 0x0000_2000)), // NX_DEVICERCTLKEYMASK
        HoldKey::Fn => Some((63, 0x0080_0000)),           // NX_SECONDARYFNMASK
        HoldKey::Off => None,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum TapError {
    InputMonitoringDenied,
    Failed,
}

pub fn tap_result(code: i32) -> Result<(), TapError> {
    match code {
        super::bridge::VB_OK => Ok(()),
        super::bridge::VB_ERR_INPUT_MONITORING_DENIED => Err(TapError::InputMonitoringDenied),
        _ => Err(TapError::Failed),
    }
}

pub fn should_start_hold(enabled: bool, key: HoldKey, granted: bool, active: bool) -> bool {
    enabled && key != HoldKey::Off && granted && !active
}

pub fn hold_active() -> bool {
    HOLD_ACTIVE.load(Ordering::SeqCst)
}

/// The system disabled the tap and it did not come back.
pub fn hold_off() {
    HOLD_ACTIVE.store(false, Ordering::SeqCst);
}

/// Starts the tap; its generation, or 0 if it did not start. Call only
/// inside `TAP.replace`.
fn start_hold(key: HoldKey) -> u64 {
    let Some((code, mask)) = mac_key(key) else {
        return 0;
    };
    let mut tap = 0u64;
    // SAFETY: plain integers and a valid out pointer; returns within a
    // second (Tap.swift).
    let result = tap_result(unsafe { super::bridge::ffi::vb_tap_start(code, mask, &mut tap) });
    if let Err(error) = &result {
        tracing::info!(?error, "the hold key is off");
    }
    HOLD_ACTIVE.store(result.is_ok(), Ordering::SeqCst);
    if result.is_ok() { tap } else { 0 }
}

/// Call only inside `TAP.replace`.
fn stop_hold() {
    // SAFETY: no arguments; returns after the tap thread exited (Task 7A).
    unsafe { super::bridge::ffi::vb_tap_stop() }
    HOLD_ACTIVE.store(false, Ordering::SeqCst);
}

/// Makes both triggers match `settings`. Returns the toggle shortcut's
/// registration error, for Settings. The tap is replaced under `TAP`: the
/// old generation is retired, a held recording is cancelled (the old tap's
/// key-up will never be seen), the old tap thread exits, and only then does
/// the new tap start and its generation count (S26).
pub fn apply(app: &AppHandle, settings: &VoiceSettings) -> Option<String> {
    let voice = app.state::<VoiceState>();
    let toggle = settings.toggle_shortcut.as_deref().filter(|_| settings.enabled);
    let error = crate::hotkey::apply_voice(app, toggle).err();
    let granted = permissions::status(Permission::InputMonitoring) == PermissionState::Granted;
    let start = should_start_hold(settings.enabled, settings.hold_key, granted, false);
    TAP.replace(
        || voice.controller.hold_lost(),
        || {
            stop_hold();
            if start { start_hold(settings.hold_key) } else { 0 }
        },
    );
    error
}

/// Starts the hold key if it should run and does not, for example right
/// after Input Monitoring was granted (plan S17).
pub fn refresh(app: &AppHandle) {
    if TAP_NEEDS_RELAUNCH {
        return;
    }
    let voice = app.state::<VoiceState>();
    let settings = voice.settings.lock().unwrap().clone();
    let granted = permissions::status(Permission::InputMonitoring) == PermissionState::Granted;
    if should_start_hold(settings.enabled, settings.hold_key, granted, hold_active()) {
        TAP.replace(
            || voice.controller.hold_lost(),
            || {
                stop_hold();
                start_hold(settings.hold_key)
            },
        );
    }
}

/// The toggle shortcut fired.
pub fn toggle(app: &AppHandle) {
    let voice = app.state::<VoiceState>();
    voice.controller.toggle(crate::panel::is_visible());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec §10: both Option keys, and the device bits that tell them apart.
    #[test]
    fn hold_keys_map_to_key_codes_and_device_bits() {
        assert_eq!(mac_key(HoldKey::RightOption), Some((61, 0x40)));
        assert_eq!(mac_key(HoldKey::LeftOption), Some((58, 0x20)));
        assert_eq!(mac_key(HoldKey::RightCommand), Some((54, 0x10)));
        assert_eq!(mac_key(HoldKey::RightControl), Some((62, 0x2000)));
        assert_eq!(mac_key(HoldKey::Fn), Some((63, 0x80_0000)));
        assert_eq!(mac_key(HoldKey::Off), None);
    }

    #[test]
    fn tap_codes_map_to_errors() {
        use super::super::bridge::{VB_ERR_INPUT_MONITORING_DENIED, VB_ERR_TAP_FAILED, VB_OK};
        assert_eq!(tap_result(VB_OK), Ok(()));
        assert_eq!(tap_result(VB_ERR_INPUT_MONITORING_DENIED), Err(TapError::InputMonitoringDenied));
        assert_eq!(tap_result(VB_ERR_TAP_FAILED), Err(TapError::Failed));
    }

    /// Plan S12, S17: the hold key runs only while voice is on, with a key,
    /// with Input Monitoring, and not twice.
    #[test]
    fn the_hold_key_starts_only_when_it_should() {
        assert!(should_start_hold(true, HoldKey::RightOption, true, false));
        assert!(!should_start_hold(false, HoldKey::RightOption, true, false), "voice off");
        assert!(!should_start_hold(true, HoldKey::Off, true, false), "no hold key");
        assert!(!should_start_hold(true, HoldKey::RightOption, false, false), "no Input Monitoring");
        assert!(!should_start_hold(true, HoldKey::RightOption, true, true), "already running");
    }

    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::thread;

    type Log = Arc<Mutex<Vec<&'static str>>>;

    fn push(log: &Log, what: &'static str) {
        log.lock().unwrap().push(what);
    }

    /// Waits, without sleeping, until `n` callers are blocked on the gate.
    fn until_waiting(gate: &TapGate, n: usize) {
        while gate.waiting_for_test() < n {
            thread::yield_now();
        }
    }

    /// Plan S26: only the installed tap's events are delivered.
    #[test]
    fn an_old_tap_event_queued_across_a_replacement_is_rejected() {
        let gate = TapGate::new();
        let log: Log = Arc::default();
        assert!(!gate.deliver(0, || push(&log, "no tap")));
        gate.replace(|| {}, || 5);
        assert!(gate.deliver(5, || push(&log, "down 5")));
        gate.replace(|| push(&log, "reset"), || 6);
        assert!(!gate.deliver(5, || push(&log, "late down 5")), "the queued hold-down of tap 5 is dropped");
        assert!(gate.deliver(6, || push(&log, "down 6")));
        assert_eq!(*log.lock().unwrap(), ["down 5", "reset", "down 6"]);
    }

    /// Interleaving 1 (S26): an old event arrives while a replacement is
    /// under way, after the reset and before the new tap is installed. It
    /// waits for the gate, then is rejected; it never starts a recording the
    /// reset can no longer cancel.
    #[test]
    fn an_old_event_during_a_replacement_waits_and_is_rejected() {
        let gate = Arc::new(TapGate::new());
        let log: Log = Arc::default();
        gate.replace(|| {}, || 5);
        let (in_swap_tx, in_swap_rx) = mpsc::channel::<()>();
        let (finish_tx, finish_rx) = mpsc::channel::<()>();
        let replacing = {
            let (gate, log) = (gate.clone(), log.clone());
            thread::spawn(move || {
                gate.replace(
                    || push(&log, "reset"),
                    || {
                        in_swap_tx.send(()).unwrap();
                        finish_rx.recv().unwrap();
                        6
                    },
                )
            })
        };
        in_swap_rx.recv().unwrap(); // barrier: reset done, taps being swapped
        let delivering = {
            let (gate, log) = (gate.clone(), log.clone());
            thread::spawn(move || gate.deliver(5, || push(&log, "down 5")))
        };
        until_waiting(&gate, 1); // the old event is blocked on the gate
        assert_eq!(*log.lock().unwrap(), ["reset"]);
        finish_tx.send(()).unwrap();
        replacing.join().unwrap();
        assert!(!delivering.join().unwrap(), "rejected once tap 6 is installed");
        assert_eq!(*log.lock().unwrap(), ["reset"]);
    }

    /// Interleaving 2 (S26): an old event already passed the check and is
    /// being delivered when a replacement starts. The replacement waits for
    /// the delivery to finish, so its reset (`hold_lost`) runs after the old
    /// hold-down and cancels the recording it started.
    #[test]
    fn a_replacement_waits_for_an_old_event_already_past_the_check() {
        let gate = Arc::new(TapGate::new());
        let log: Log = Arc::default();
        gate.replace(|| {}, || 5);
        let (entered_tx, entered_rx) = mpsc::channel::<()>();
        let (resume_tx, resume_rx) = mpsc::channel::<()>();
        let delivering = {
            let (gate, log) = (gate.clone(), log.clone());
            thread::spawn(move || {
                gate.deliver(5, || {
                    entered_tx.send(()).unwrap();
                    resume_rx.recv().unwrap();
                    push(&log, "down 5");
                })
            })
        };
        entered_rx.recv().unwrap(); // barrier: past the check, inside delivery
        let replacing = {
            let (gate, log) = (gate.clone(), log.clone());
            thread::spawn(move || gate.replace(|| push(&log, "reset"), || 6))
        };
        until_waiting(&gate, 1); // the replacement is blocked on the gate
        assert!(log.lock().unwrap().is_empty(), "no reset while the old event is delivered");
        resume_tx.send(()).unwrap();
        assert!(delivering.join().unwrap());
        replacing.join().unwrap();
        assert_eq!(*log.lock().unwrap(), ["down 5", "reset"]);
        assert!(!gate.deliver(5, || push(&log, "late down 5")));
    }
}

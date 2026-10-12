//! Triggers and recording rules (voice spec §4.1, §6.2). Plain Rust with
//! the time passed in, so every rule is tested on Linux. The platform sits
//! behind `SpeechInput`; on macOS the app implements it over the Swift
//! bridge, where the audio stays (step-2 plan S2).
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::text::VoiceText;

/// Spec §6.2: a lost key-up must not leave the microphone on.
pub const MAX_RECORDING: Duration = Duration::from_secs(60);
/// Spec §6.2: shorter utterances are ignored.
pub const MIN_UTTERANCE: Duration = Duration::from_millis(300);
/// Less voiced audio than this is silent (plan S10).
pub const MIN_VOICED: Duration = Duration::from_millis(150);
/// After a stop the final text must arrive within this, or the recording
/// fails closed (plan S5).
pub const FINAL_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max: Duration,
    pub final_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max: MAX_RECORDING, final_timeout: FINAL_TIMEOUT }
    }
}

/// What a trigger asks for (spec §4.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerEvent {
    Start,
    Stop,
    Cancel,
}

/// The hold key (spec §6.2, plan S11). Down starts, up stops. Another key
/// while it is down is a chord (Option+e types "é"): that cancels, and the
/// following key-up does nothing.
#[derive(Debug, Default)]
pub struct Hold {
    down: bool,
    chorded: bool,
}

impl Hold {
    pub fn key(&mut self, down: bool) -> Option<TriggerEvent> {
        match (self.down, down) {
            (false, true) => {
                self.down = true;
                self.chorded = false;
                Some(TriggerEvent::Start)
            }
            (true, false) => {
                self.down = false;
                (!std::mem::take(&mut self.chorded)).then_some(TriggerEvent::Stop)
            }
            // Auto-repeat, or a release we never saw pressed.
            _ => None,
        }
    }

    pub fn other_key(&mut self) -> Option<TriggerEvent> {
        if self.down && !self.chorded {
            self.chorded = true;
            return Some(TriggerEvent::Cancel);
        }
        None
    }

    /// The tap was disabled or restarted, so a key-up may be lost.
    pub fn reset(&mut self) -> Option<TriggerEvent> {
        let held = self.down && !self.chorded;
        *self = Self::default();
        held.then_some(TriggerEvent::Cancel)
    }
}

/// The toggle shortcut: the first press starts, the second stops.
pub fn toggle(listening: bool) -> TriggerEvent {
    if listening { TriggerEvent::Stop } else { TriggerEvent::Start }
}

/// Why a recording could not start or went wrong. Classified only: no
/// platform error text crosses into Rust (spec §6.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ListenError {
    MicrophoneDenied,
    MicrophoneUnavailable,
    SpeechDenied,
    SpeechUnavailable,
    AssetsMissing,
    Failed,
}

/// Why voice cannot listen on this machine (spec §9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Unavailable {
    /// The backend has not answered yet.
    Checking,
    NotMac,
    OsTooOld,
    /// Built without the macOS 26 SDK (plan S16).
    SdkTooOld,
    TranscriberUnsupported,
    LocaleUnsupported,
    AssetsMissing,
}

/// What a backend reports (spec §4.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Capabilities {
    /// `None` when it can listen.
    pub unavailable: Option<Unavailable>,
    /// Without partials the HUD shows "Listening…" with no live text.
    pub streams_partials: bool,
    /// The analyzer's audio format, for information.
    pub sample_rate: u32,
}

impl Capabilities {
    pub const CHECKING: Self = Self { unavailable: Some(Unavailable::Checking), streams_partials: false, sample_rate: 0 };
}

/// From the platform. `generation` is the id `SpeechInput::start` got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListenEvent {
    Partial { generation: u64, text: VoiceText },
    Final { generation: u64, text: VoiceText, audio: Duration, voiced: Duration },
    Error { generation: u64, error: ListenError },
}

impl ListenEvent {
    pub fn generation(&self) -> u64 {
        match self {
            ListenEvent::Partial { generation, .. }
            | ListenEvent::Final { generation, .. }
            | ListenEvent::Error { generation, .. } => *generation,
        }
    }
}

/// Capture and transcription (spec §4.1, one seam: plan S2). Every method
/// returns at once and never calls back synchronously; events arrive
/// later through `Controller::speech_event`. The controller calls `start`,
/// `stop` and `cancel` under its core lock, so an implementation must not
/// call back into the controller synchronously (as `HudSink` documents).
pub trait SpeechInput: Send + Sync {
    fn capabilities(&self) -> Capabilities;
    /// Starts recording `generation`, stopping on its own after `max`
    /// (plus a margin). A live session of another generation is cancelled.
    fn start(&self, generation: u64, max: Duration) -> Result<(), ListenError>;
    /// Stops capture and finishes the transcript: a `Final` or an `Error`
    /// for `generation` follows.
    fn stop(&self, generation: u64);
    /// Stops capture and drops the session; nothing follows.
    fn cancel(&self, generation: u64);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ignored {
    TooShort,
    Silent,
    Empty,
}

/// How a recording ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Heard {
    Transcript(VoiceText),
    Ignored(Ignored),
    Failed(ListenError),
    /// The 60 s cap: discarded (plan S9).
    TooLong,
    /// No final within `FINAL_TIMEOUT` after the stop.
    TimedOut,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Progress {
    Partial(VoiceText),
    Done(Heard),
}

/// What a release did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stopped {
    /// Capture stopped; the final text follows.
    Finishing(u64),
    /// The release came at or after the cap: discard it and cancel (S9).
    TooLong(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    Started,
    /// A recording that was still finishing was dropped; cancel it.
    Replaced(u64),
    /// Already listening; the press changes nothing.
    Busy,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Idle,
    Listening { generation: u64, since: Instant },
    Finishing { generation: u64, since: Instant },
}

/// One recording at a time (spec §6.2). Events of any other generation
/// are dropped (spec §4.2).
#[derive(Debug, Default)]
pub struct Recorder {
    phase: Phase,
    limits: Limits,
}

impl Recorder {
    pub fn new(limits: Limits) -> Self {
        Self { phase: Phase::Idle, limits }
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// The generation whose microphone is live.
    pub fn listening(&self) -> Option<u64> {
        match self.phase {
            Phase::Listening { generation, .. } => Some(generation),
            _ => None,
        }
    }

    /// Listening, or stopped and waiting for the final text.
    pub fn active(&self) -> Option<u64> {
        match self.phase {
            Phase::Listening { generation, .. } | Phase::Finishing { generation, .. } => Some(generation),
            Phase::Idle => None,
        }
    }

    pub fn start(&mut self, generation: u64, now: Instant) -> Start {
        let result = match self.phase {
            Phase::Listening { .. } => return Start::Busy,
            Phase::Finishing { generation: old, .. } => Start::Replaced(old),
            Phase::Idle => Start::Started,
        };
        self.phase = Phase::Listening { generation, since: now };
        result
    }

    /// The key went up or the toggle was pressed again. At or after the cap
    /// the recording is too long, whether or not the timer ran yet (S9).
    pub fn stop(&mut self, now: Instant) -> Option<Stopped> {
        let Phase::Listening { generation, since } = self.phase else {
            return None;
        };
        if now.duration_since(since) >= self.limits.max {
            self.phase = Phase::Idle;
            return Some(Stopped::TooLong(generation));
        }
        self.phase = Phase::Finishing { generation, since: now };
        Some(Stopped::Finishing(generation))
    }

    /// Drops the recording; nothing it produces is routed.
    pub fn cancel(&mut self) -> Option<u64> {
        let generation = self.active()?;
        self.phase = Phase::Idle;
        Some(generation)
    }

    /// The 60 s cap and the final timeout. The caller cancels the returned
    /// generation on the platform.
    pub fn tick(&mut self, now: Instant) -> Option<(u64, Heard)> {
        let (generation, heard) = match self.phase {
            Phase::Listening { generation, since } if now.duration_since(since) >= self.limits.max => (generation, Heard::TooLong),
            Phase::Finishing { generation, since } if now.duration_since(since) >= self.limits.final_timeout => {
                (generation, Heard::TimedOut)
            }
            _ => return None,
        };
        self.phase = Phase::Idle;
        Some((generation, heard))
    }

    pub fn event(&mut self, ev: ListenEvent, now: Instant) -> Option<Progress> {
        if self.active() != Some(ev.generation()) {
            return None;
        }
        // The cap holds whatever order the timer and the events run in (S9).
        if let Phase::Listening { since, .. } = self.phase
            && now.duration_since(since) >= self.limits.max
        {
            self.phase = Phase::Idle;
            return Some(Progress::Done(Heard::TooLong));
        }
        match ev {
            ListenEvent::Partial { text, .. } => self.listening().map(|_| Progress::Partial(text)),
            ListenEvent::Final { text, audio, voiced, .. } => {
                self.phase = Phase::Idle;
                let heard = if audio >= self.limits.max { Heard::TooLong } else { judge(text, audio, voiced) };
                Some(Progress::Done(heard))
            }
            ListenEvent::Error { error, .. } => {
                self.phase = Phase::Idle;
                Some(Progress::Done(Heard::Failed(error)))
            }
        }
    }
}

/// Spec §6.2: under 0.3 s, silent (plan S10) or empty is ignored.
pub fn judge(text: VoiceText, audio: Duration, voiced: Duration) -> Heard {
    if audio < MIN_UTTERANCE {
        Heard::Ignored(Ignored::TooShort)
    } else if voiced < MIN_VOICED {
        Heard::Ignored(Ignored::Silent)
    } else if text.expose().trim().is_empty() {
        Heard::Ignored(Ignored::Empty)
    } else {
        Heard::Transcript(text)
    }
}

/// A scripted `SpeechInput` for tests: records calls, can refuse to start.
#[cfg(any(test, feature = "testing"))]
pub mod fake {
    use std::sync::Mutex;
    use std::time::Duration;

    use super::{Capabilities, ListenError, SpeechInput};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Call {
        Start(u64),
        Stop(u64),
        Cancel(u64),
    }

    #[derive(Default)]
    pub struct FakeSpeechInput {
        pub calls: Mutex<Vec<Call>>,
        pub refuse: Mutex<Option<ListenError>>,
        /// `None` means available, with partials.
        pub caps: Mutex<Option<Capabilities>>,
    }

    impl FakeSpeechInput {
        pub fn calls(&self) -> Vec<Call> {
            self.calls.lock().unwrap().clone()
        }

        pub fn last_start(&self) -> Option<u64> {
            self.calls().iter().rev().find_map(|c| match c {
                Call::Start(g) => Some(*g),
                _ => None,
            })
        }
    }

    impl SpeechInput for FakeSpeechInput {
        fn capabilities(&self) -> Capabilities {
            self.caps.lock().unwrap().unwrap_or(Capabilities { unavailable: None, streams_partials: true, sample_rate: 16_000 })
        }
        fn start(&self, generation: u64, _max: Duration) -> Result<(), ListenError> {
            if let Some(e) = *self.refuse.lock().unwrap() {
                return Err(e);
            }
            self.calls.lock().unwrap().push(Call::Start(generation));
            Ok(())
        }
        fn stop(&self, generation: u64) {
            self.calls.lock().unwrap().push(Call::Stop(generation));
        }
        fn cancel(&self, generation: u64) {
            self.calls.lock().unwrap().push(Call::Cancel(generation));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn said(t: &str) -> VoiceText {
        VoiceText::new(t)
    }

    #[test]
    fn hold_down_starts_up_stops_and_repeats_do_nothing() {
        let mut h = Hold::default();
        assert_eq!(h.key(true), Some(TriggerEvent::Start));
        assert_eq!(h.key(true), None, "auto-repeat of a modifier");
        assert_eq!(h.key(false), Some(TriggerEvent::Stop));
        assert_eq!(h.key(false), None);
    }

    /// Review Focus 2: Right Option + e types "é"; that is not a recording.
    #[test]
    fn a_chord_cancels_and_the_release_does_nothing() {
        let mut h = Hold::default();
        assert_eq!(h.key(true), Some(TriggerEvent::Start));
        assert_eq!(h.other_key(), Some(TriggerEvent::Cancel));
        assert_eq!(h.other_key(), None, "one cancel per press");
        assert_eq!(h.key(false), None, "the key-up after a chord neither stops nor routes");
        assert_eq!(h.key(true), Some(TriggerEvent::Start), "the next press works");
        assert_eq!(Hold::default().other_key(), None, "typing without the hold key is nothing");
    }

    /// Review Focus 5: a disabled tap may have lost the key-up.
    #[test]
    fn hold_reset_while_held_cancels() {
        let mut h = Hold::default();
        h.key(true);
        assert_eq!(h.reset(), Some(TriggerEvent::Cancel));
        assert_eq!(h.key(false), None);
        assert_eq!(Hold::default().reset(), None);
    }

    #[test]
    fn the_toggle_starts_then_stops() {
        assert_eq!(toggle(false), TriggerEvent::Start);
        assert_eq!(toggle(true), TriggerEvent::Stop);
    }

    #[test]
    fn short_silent_and_empty_utterances_are_ignored() {
        assert_eq!(judge(said("approve"), ms(299), ms(299)), Heard::Ignored(Ignored::TooShort));
        assert_eq!(judge(said("approve"), ms(300), ms(149)), Heard::Ignored(Ignored::Silent));
        assert_eq!(judge(said("  \n"), ms(900), ms(600)), Heard::Ignored(Ignored::Empty));
        assert_eq!(judge(said("approve"), ms(300), ms(150)), Heard::Transcript(said("approve")));
    }

    /// Review Focus 1: a lost key-up never leaves the microphone on, and
    /// what was recorded is thrown away (S9).
    #[test]
    fn a_recording_past_60_s_is_cut_and_discarded() {
        let t0 = Instant::now();
        let mut r = Recorder::default();
        assert_eq!(r.start(1, t0), Start::Started);
        assert_eq!(r.tick(t0 + MAX_RECORDING - ms(1)), None);
        assert_eq!(r.tick(t0 + MAX_RECORDING), Some((1, Heard::TooLong)));
        assert_eq!(r.active(), None);
        let late = ListenEvent::Final { generation: 1, text: said("approve"), audio: ms(60_000), voiced: ms(30_000) };
        assert_eq!(r.event(late, t0 + MAX_RECORDING), None, "nothing routes after the cap");
    }

    /// Review Focus 4.
    #[test]
    fn events_from_an_older_session_are_dropped() {
        let t0 = Instant::now();
        let mut r = Recorder::default();
        r.start(1, t0);
        assert_eq!(r.cancel(), Some(1));
        r.start(2, t0);
        assert_eq!(r.event(ListenEvent::Partial { generation: 1, text: said("app") }, t0), None);
        assert_eq!(r.event(ListenEvent::Final { generation: 1, text: said("approve"), audio: ms(900), voiced: ms(600) }, t0), None);
        assert_eq!(r.event(ListenEvent::Error { generation: 1, error: ListenError::Failed }, t0), None);
        assert_eq!(r.listening(), Some(2), "an old event does not end the new recording");
        assert_eq!(r.event(ListenEvent::Partial { generation: 2, text: said("de") }, t0), Some(Progress::Partial(said("de"))));
        r.stop(t0 + ms(800));
        let done = r.event(ListenEvent::Final { generation: 2, text: said("deny"), audio: ms(800), voiced: ms(500) }, t0);
        assert_eq!(done, Some(Progress::Done(Heard::Transcript(said("deny")))));
        assert_eq!(r.active(), None);
    }

    /// Review Focus 1, plan S9: a release processed after 60 s, before the
    /// timer ran, is too long. The order of timer and release does not matter.
    #[test]
    fn a_release_after_the_cap_is_too_long_even_before_the_timer() {
        let t0 = Instant::now();
        let mut r = Recorder::default();
        r.start(1, t0);
        assert_eq!(r.stop(t0 + MAX_RECORDING), Some(Stopped::TooLong(1)));
        assert_eq!(r.active(), None, "nothing waits for a final");
        assert_eq!(r.tick(t0 + MAX_RECORDING + ms(1)), None, "the late timer finds nothing");
        let mut r = Recorder::default();
        r.start(2, t0);
        assert_eq!(r.stop(t0 + MAX_RECORDING - ms(1)), Some(Stopped::Finishing(2)));
    }

    /// Plan S9: a final is too long if it arrives after the cap while still
    /// listening, or if it reports 60 s of audio or more.
    #[test]
    fn a_final_after_the_cap_is_too_long() {
        let t0 = Instant::now();
        let mut r = Recorder::default();
        r.start(1, t0);
        let fin = |audio| ListenEvent::Final { generation: 1, text: said("approve"), audio, voiced: ms(600) };
        assert_eq!(r.event(fin(ms(900)), t0 + MAX_RECORDING), Some(Progress::Done(Heard::TooLong)));
        let mut r = Recorder::default();
        r.start(1, t0);
        r.stop(t0 + ms(500));
        assert_eq!(r.event(fin(MAX_RECORDING), t0 + ms(600)), Some(Progress::Done(Heard::TooLong)));
        let mut r = Recorder::default();
        r.start(1, t0);
        assert_eq!(r.event(ListenEvent::Partial { generation: 1, text: said("a") }, t0 + MAX_RECORDING), Some(Progress::Done(Heard::TooLong)));
    }

    #[test]
    fn a_missing_final_times_out_closed() {
        let t0 = Instant::now();
        let mut r = Recorder::default();
        r.start(1, t0);
        assert_eq!(r.stop(t0 + ms(500)), Some(Stopped::Finishing(1)));
        assert_eq!(r.listening(), None);
        assert_eq!(r.active(), Some(1), "finishing");
        assert_eq!(r.tick(t0 + ms(500) + FINAL_TIMEOUT - ms(1)), None);
        assert_eq!(r.tick(t0 + ms(500) + FINAL_TIMEOUT), Some((1, Heard::TimedOut)));
    }

    #[test]
    fn partials_count_only_while_listening() {
        let t0 = Instant::now();
        let mut r = Recorder::default();
        r.start(1, t0);
        r.stop(t0 + ms(400));
        assert_eq!(r.event(ListenEvent::Partial { generation: 1, text: said("app") }, t0), None);
    }

    #[test]
    fn a_press_while_finishing_replaces_it_and_a_press_while_listening_is_busy() {
        let t0 = Instant::now();
        let mut r = Recorder::default();
        r.start(1, t0);
        assert_eq!(r.start(2, t0), Start::Busy);
        r.stop(t0 + ms(400));
        assert_eq!(r.start(3, t0 + ms(450)), Start::Replaced(1));
        assert_eq!(r.event(ListenEvent::Final { generation: 1, text: said("approve"), audio: ms(400), voiced: ms(300) }, t0), None);
        assert_eq!(r.listening(), Some(3));
    }

    #[test]
    fn errors_end_the_recording() {
        let t0 = Instant::now();
        let mut r = Recorder::default();
        r.start(4, t0);
        let e = r.event(ListenEvent::Error { generation: 4, error: ListenError::MicrophoneUnavailable }, t0);
        assert_eq!(e, Some(Progress::Done(Heard::Failed(ListenError::MicrophoneUnavailable))));
        assert_eq!(r.active(), None);
        assert_eq!(r.stop(t0), None);
        assert_eq!(r.cancel(), None);
    }

    #[test]
    fn debug_never_shows_what_was_said() {
        let e = ListenEvent::Final { generation: 1, text: said("SECRET"), audio: ms(1), voiced: ms(1) };
        let h = Heard::Transcript(said("SECRET"));
        assert!(!format!("{e:?} {h:?} {:?}", Progress::Partial(said("SECRET"))).contains("SECRET"));
    }

    #[test]
    fn the_fake_records_calls_and_can_refuse() {
        use fake::{Call, FakeSpeechInput};
        let f = FakeSpeechInput::default();
        f.start(1, MAX_RECORDING).unwrap();
        f.stop(1);
        f.cancel(2);
        assert_eq!(f.calls(), [Call::Start(1), Call::Stop(1), Call::Cancel(2)]);
        *f.refuse.lock().unwrap() = Some(ListenError::MicrophoneDenied);
        assert_eq!(f.start(3, MAX_RECORDING), Err(ListenError::MicrophoneDenied));
        assert_eq!(f.capabilities().unavailable, None);
    }
}

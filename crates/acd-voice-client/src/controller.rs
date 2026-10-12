//! One utterance at a time: freeze, route, show, execute, report.
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use acd_client::inbox::Inbox;
use acd_client::voice::{ActionEvent, ActionOutcome};
use acd_voice::hud::Hud;
use acd_voice::listen::{self, Heard, Hold, Limits, ListenEvent, Progress, Recorder, SpeechInput, Start, Stopped, TriggerEvent};
use acd_voice::plan::{Executor, Reaction};
use acd_voice::refusal::Refusal;
use acd_voice::router::model::IntentModel;
use acd_voice::router::{RouteOptions, route};
use acd_voice::snapshot::{Presentation, Snapshot};
use acd_voice::text::VoiceText;
use tokio::sync::{broadcast, watch};

use crate::freeze::freeze;
use crate::port::{AcdPort, voice_outcome};

/// How long the HUD has to acknowledge a step before it fails closed (D22).
pub const DISPLAY_TIMEOUT: Duration = Duration::from_secs(1);

/// Where the controller shows things; the app emits Tauri events. It must
/// not call back into the controller: it runs under the executor lock.
pub trait HudSink: Send + Sync {
    /// Publishes `hud` as number `seq`; false if it could not be sent.
    fn hud(&self, seq: u64, hud: &Hud) -> bool;
    fn select(&self, card_key: &str);
    /// The microphone went on or off, as Swift reported it (spec §6.2,
    /// plan S18): the menu bar indicator.
    fn listening(&self, _on: bool) {}
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum InputError {
    #[error("The command is empty")]
    Empty,
    #[error("Voice is off. Turn it on in Settings.")]
    Off,
    #[error("A recording is running. Finish or cancel it first.")]
    Recording,
}

/// Where an utterance came from. A log tag only: safety never sees it,
/// so speech gets exactly the typed path (step-2 plan S15).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Typed,
    Speech,
}

impl Source {
    fn tag(self) -> &'static str {
        match self {
            Source::Typed => "typed",
            Source::Speech => "speech",
        }
    }
}

/// When a transcript left the recorder (plan S23). Admission drops it if
/// voice was reset, locked, overflowed or pressed again since.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Admission {
    session: u64,
    epoch: u64,
}

/// Everything admission depends on, behind one lock (B4): the executor,
/// whether voice is on, and the session generation. `set_enabled` and
/// every admission take this lock, so an utterance is either admitted
/// before a reset (and dropped by it) or refused after it. There is no
/// third order.
struct Core {
    exec: Executor,
    enabled: bool,
    session: u64,
    /// Grows with every recording, lock, overflow and voice on or off (S23).
    epoch: u64,
    /// The recording, its hold key, the snapshot frozen at the press
    /// (plan S6) and the step shown under Listening (plan S7).
    recorder: Recorder,
    hold: Hold,
    frozen: Option<Snapshot>,
    over: Option<Hud>,
    /// What an outcome or a display timeout would have shown while a
    /// recording had the HUD. The Listening state keeps the screen (spec
    /// §6.2: the microphone indicator stays); this is shown when the
    /// recording ends with nothing to route.
    deferred: Option<Reaction>,
    /// The recording whose transcript left the recorder and is not yet
    /// admitted. The HUD still shows Listening, so Esc and Cancel must
    /// reach it: they bump `epoch`, which drops the transcript (S23).
    unadmitted: Option<u64>,
    /// The recording whose capture Swift reports running (S18).
    mic: Option<u64>,
    /// The highest generation Swift reported stopped (S18).
    mic_off: u64,
}

pub struct Controller {
    me: Weak<Controller>,
    runtime: tokio::runtime::Handle,
    inbox: Arc<Inbox>,
    port: AcdPort,
    core: Mutex<Core>,
    presentation: Mutex<Option<Presentation>>,
    model: Option<Arc<dyn IntentModel>>,
    sink: Arc<dyn HudSink>,
    opts: RouteOptions,
    /// Mirrors `Core::session` for `select!`; written only under the core
    /// lock, so a receiver made under that lock sees every later change.
    session_tx: watch::Sender<u64>,
    seq: AtomicU64,
    /// Test hook: runs with no lock held, just before admission.
    admission_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// The platform's capture and transcription; `None` until attached.
    speech: Mutex<Option<Arc<dyn SpeechInput>>>,
}

impl Controller {
    /// Subscribes to action outcomes before anything can be enqueued, then
    /// forwards them. Call inside a tokio runtime.
    pub fn start(
        inbox: Arc<Inbox>,
        model: Option<Arc<dyn IntentModel>>,
        sink: Arc<dyn HudSink>,
        enabled: bool,
    ) -> Arc<Self> {
        let outcomes = inbox.action_outcomes();
        let this = Arc::new_cyclic(|me| Self {
            me: me.clone(),
            runtime: tokio::runtime::Handle::current(),
            port: AcdPort::new(inbox.clone()),
            inbox,
            core: Mutex::new(Core {
                exec: Executor::new(),
                enabled,
                session: 0,
                epoch: 0,
                recorder: Recorder::default(),
                hold: Hold::default(),
                frozen: None,
                over: None,
                deferred: None,
                unadmitted: None,
                mic: None,
                mic_off: 0,
            }),
            presentation: Mutex::new(None),
            model,
            sink,
            opts: RouteOptions::default(),
            session_tx: watch::channel(0).0,
            seq: AtomicU64::new(0),
            admission_hook: Mutex::new(None),
            speech: Mutex::new(None),
        });
        this.runtime.spawn(forward_outcomes(Arc::downgrade(&this), outcomes));
        this
    }

    /// The panel's latest presentation (spec §4.3); stale publishes lose.
    pub fn publish(&self, presentation: Presentation) -> bool {
        let mut current = self.presentation.lock().unwrap();
        if !presentation.supersedes(current.as_ref()) {
            return false;
        }
        *current = Some(presentation);
        true
    }

    /// Voice on or off. A change starts a new session (no change does nothing), under the same
    /// lock as admission (B4), and the executor resets: in-flight routing
    /// is dropped, unsent work cancelled, voice text forgotten (spec §6.3)
    /// and the HUD hidden, so a bump never leaves the HUD on a state whose
    /// work was dropped.
    pub fn set_enabled(&self, on: bool) {
        let mut core = self.core.lock().unwrap();
        // Saving unchanged settings must not cancel a running countdown.
        if core.enabled == on {
            return;
        }
        core.enabled = on;
        self.invalidate(&mut core);
    }

    /// Test hook: whether a result is held back until the recording ends.
    #[doc(hidden)]
    pub fn has_deferred_for_test(&self) -> bool {
        self.core.lock().unwrap().deferred.is_some()
    }

    /// Test hook: `hook` runs just before admission, with no lock held, so
    /// a test can force "voice turned off after the input arrived but
    /// before it was admitted".
    #[doc(hidden)]
    pub fn on_admission_for_test(&self, hook: impl Fn() + Send + Sync + 'static) {
        *self.admission_hook.lock().unwrap() = Some(Arc::new(hook));
    }

    /// Typed text as the transcript (step 1's debug input).
    pub async fn text_input(&self, text: String, panel_open: bool) -> Result<(), InputError> {
        if text.trim().is_empty() {
            return Err(InputError::Empty);
        }
        if self.core.lock().unwrap().recorder.active().is_some() {
            return Err(InputError::Recording);
        }
        let snapshot = self.freeze(panel_open);
        self.utterance(VoiceText::new(text), snapshot, Source::Typed, None).await
    }

    /// One utterance, typed or spoken, through the same admission, route
    /// and safety path (plan S15). `snapshot` was frozen at submit for typed
    /// text and at the press for speech (plan S6). A spoken one carries the
    /// token taken when it left the recorder (plan S23).
    async fn utterance(
        &self,
        transcript: VoiceText,
        snapshot: Snapshot,
        source: Source,
        token: Option<Admission>,
    ) -> Result<(), InputError> {
        let started = Instant::now();
        let hook = self.admission_hook.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook();
        }
        // Admission (B4): the enabled check, the session, and reply-or-begin
        // happen under one lock, the one `set_enabled` takes.
        let admitted = {
            let mut core = self.core.lock().unwrap();
            if !core.enabled {
                return Err(InputError::Off);
            }
            // S23: a transcript from before a press, lock, overflow or
            // voice off and on never enters the executor.
            if let Some(t) = token
                && (t.session != core.session || t.epoch != core.epoch)
            {
                tracing::info!(target: "acd_voice", source = source.tag(), "stale transcript dropped");
                return Ok(());
            }
            // A typed command never runs beside a live recording; checked here,
            // under the lock, because one may have started since the first look.
            if token.is_none() && core.recorder.active().is_some() {
                return Err(InputError::Recording);
            }
            if token.is_some() {
                core.unadmitted = None;
                // A result that landed while the HUD showed Listening must
                // not vanish behind the new step.
                if let Some(late) = core.deferred.take() {
                    self.emit(&mut core.exec, late);
                }
            }
            let session = core.session;
            let changes = self.session_tx.subscribe();
            // A HUD that waits for input takes the utterance as its answer (D21).
            if let Some(r) = core.exec.reply(&self.port, &transcript, self.opts.locale) {
                self.emit(&mut core.exec, r);
                Err("reply")
            } else {
                match core.exec.begin(&self.port, snapshot.clone(), transcript.clone(), self.opts.locale) {
                    Ok((n, thinking)) => {
                        self.emit(&mut core.exec, thinking);
                        Ok((session, n, changes))
                    }
                    Err(refused) => {
                        self.emit(&mut core.exec, *refused);
                        Err("refused")
                    }
                }
            }
        };
        let (session, n, mut changes) = match admitted {
            Ok(admitted) => admitted,
            Err(route) => {
                tracing::info!(target: "acd_voice", chars = transcript.char_len(), source = source.tag(), route, "utterance routed");
                return Ok(());
            }
        };
        let routed = tokio::select! {
            routed = route(&snapshot, &transcript, self.model.as_deref(), &self.opts) => routed,
            // Voice was turned off or on: drop the work, cancelling any model call (B4).
            _ = changes.changed() => return Ok(()),
        };
        let mut core = self.core.lock().unwrap();
        if core.session != session || !core.enabled {
            return Ok(());
        }
        tracing::info!(
            target: "acd_voice",
            chars = transcript.char_len(),
            source = source.tag(),
            route = routed.tag(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "utterance routed"
        );
        if let Some(r) = core.exec.routed(n, routed) {
            self.emit(&mut core.exec, r);
        }
        Ok(())
    }

    /// Attaches the platform's capture and transcription (spec §4.1).
    pub fn attach_speech(&self, input: Arc<dyn SpeechInput>) {
        *self.speech.lock().unwrap() = Some(input);
    }

    /// Test hook: shorter limits than 60 s and 5 s.
    #[doc(hidden)]
    pub fn set_listen_limits_for_test(&self, limits: Limits) {
        self.core.lock().unwrap().recorder = Recorder::new(limits);
    }

    /// Whether a recording is listening (not finishing).
    pub fn listening(&self) -> bool {
        self.core.lock().unwrap().recorder.listening().is_some()
    }

    /// The hold key went down or up. `panel_open` is read at the press,
    /// because the snapshot freezes then (spec §4.3, plan S6).
    pub fn hold(&self, down: bool, panel_open: bool) {
        let snapshot = down.then(|| self.freeze(panel_open));
        let mut core = self.core.lock().unwrap();
        // A press while a recording is live (a toggle one, or a key repeat)
        // must not become the hold: its release would stop that recording.
        if down && core.recorder.listening().is_some() {
            return;
        }
        if let Some(ev) = core.hold.key(down) {
            self.trigger(&mut core, ev, snapshot);
        }
    }

    /// Another key went down: a chord cancels (plan S11).
    pub fn other_key(&self) {
        let mut core = self.core.lock().unwrap();
        if let Some(ev) = core.hold.other_key() {
            self.trigger(&mut core, ev, None);
        }
    }

    /// The tap was disabled or is being replaced, so a key-up may be lost
    /// (plan S26).
    pub fn hold_lost(&self) {
        let mut core = self.core.lock().unwrap();
        if let Some(ev) = core.hold.reset() {
            self.trigger(&mut core, ev, None);
        }
    }

    /// The toggle shortcut: starts, or stops a recording.
    pub fn toggle(&self, panel_open: bool) {
        let snapshot = self.freeze(panel_open);
        let mut core = self.core.lock().unwrap();
        let ev = listen::toggle(core.recorder.listening().is_some());
        self.trigger(&mut core, ev, Some(snapshot));
    }

    /// Esc in the inbox panel (plan S25). True if it cancelled a recording;
    /// false lets the inbox handle Esc as before.
    pub fn escape_listening(&self) -> bool {
        let mut core = self.core.lock().unwrap();
        if core.recorder.active().is_none() {
            return self.cancel_unadmitted(&mut core, None);
        }
        self.cancel_listening(&mut core);
        true
    }

    /// Swift reports the microphone started or stopped for `generation`
    /// (S18). Only this drives the indicator. Generations only grow, so an
    /// `on` at or below the highest `off` seen is stale and ignored, and an
    /// `off` clears any older `on` too.
    pub fn capture(&self, generation: u64, on: bool) {
        let mut core = self.core.lock().unwrap();
        if on {
            if generation <= core.mic_off {
                return;
            }
            if core.recorder.active() != Some(generation) {
                // Capture for a recording Rust no longer has: stop it.
                if let Some(input) = self.input() {
                    input.cancel(generation);
                }
                return;
            }
            if core.mic != Some(generation) {
                core.mic = Some(generation);
                self.sink.listening(true);
            }
        } else {
            core.mic_off = core.mic_off.max(generation);
            if core.mic.is_some_and(|m| m <= generation) {
                core.mic = None;
                self.sink.listening(false);
            }
        }
    }

    /// The bridge dropped an event (plan S5). It could have been a chord's
    /// other key, a key-up, a final or a lock, and a dropped lock cannot be
    /// told apart from the others. So it invalidates exactly as a lock does
    /// (S24): recording, thinking step, unsent voice countdown, transcripts,
    /// snapshots and any confirmation on screen.
    pub fn overflow(&self) {
        let mut core = self.core.lock().unwrap();
        tracing::info!(target: "acd_voice", "bridge overflow: voice invalidated");
        core.hold = Hold::default();
        self.invalidate(&mut core);
    }

    /// The screen locked or the Mac is going to sleep (spec §6.2, plan S24).
    pub fn system_stop(&self) {
        let mut core = self.core.lock().unwrap();
        tracing::info!(target: "acd_voice", "voice invalidated for lock or sleep");
        core.hold = Hold::default();
        self.invalidate(&mut core);
    }

    /// A message that is not a step, such as "the hold key stopped working"
    /// (spec §9). Shown only when no step and no recording is on the HUD, so
    /// it never hides a step that waits.
    pub fn notice(&self, refusal: Refusal) {
        let mut core = self.core.lock().unwrap();
        if core.recorder.active().is_none() && core.exec.is_idle() {
            let hud = Hud::Error { refusal, transcript: None };
            self.emit(&mut core.exec, Reaction { hud, select: None });
        }
    }

    /// An event from the platform (spec §4.2). Events of any recording but
    /// the current one are dropped by the recorder.
    pub fn speech_event(&self, ev: ListenEvent) {
        let mut core = self.core.lock().unwrap();
        let Some(id) = core.recorder.active() else {
            return;
        };
        // A final before the key went up is not the user's release: nothing
        // is routed from it and the recording goes on.
        if matches!(ev, ListenEvent::Final { .. }) && core.recorder.listening() == Some(id) {
            tracing::info!(target: "acd_voice", "final before release ignored");
            return;
        }
        match core.recorder.event(ev, Instant::now()) {
            None => {}
            Some(Progress::Partial(text)) => {
                let hud = Hud::Listening { id, partial: Some(text), over: core.over.clone().map(Box::new) };
                self.emit(&mut core.exec, Reaction { hud, select: None });
            }
            Some(Progress::Done(heard)) => {
                // Too long or failed: make sure Swift lets go too.
                if !matches!(heard, Heard::Transcript(_) | Heard::Ignored(_))
                    && let Some(input) = self.input()
                {
                    input.cancel(id);
                }
                self.heard(&mut core, id, heard);
            }
        }
    }

    fn input(&self) -> Option<Arc<dyn SpeechInput>> {
        self.speech.lock().unwrap().clone()
    }

    /// Voice off, lock or sleep (plan S24): the recording is cancelled, an
    /// in-flight route is dropped and its model call cancelled (step-1 B4),
    /// an unsent voice countdown is cancelled, and every transcript,
    /// snapshot and shown confirmation is forgotten.
    fn invalidate(&self, core: &mut Core) {
        if let Some(generation) = core.recorder.cancel()
            && let Some(input) = self.input()
        {
            input.cancel(generation);
        }
        core.frozen = None;
        core.over = None;
        core.deferred = None;
        core.unadmitted = None;
        core.epoch += 1;
        core.session += 1;
        self.session_tx.send_replace(core.session);
        core.exec.reset(&self.port);
        self.emit(&mut core.exec, Reaction { hud: Hud::Hidden, select: None });
    }

    fn trigger(&self, core: &mut Core, ev: TriggerEvent, snapshot: Option<Snapshot>) {
        match ev {
            TriggerEvent::Start => {
                if let Some(snapshot) = snapshot {
                    self.start_listening(core, snapshot);
                }
            }
            TriggerEvent::Stop => self.stop_listening(core),
            TriggerEvent::Cancel => self.cancel_listening(core),
        }
    }

    fn start_listening(&self, core: &mut Core, snapshot: Snapshot) {
        if !core.enabled || core.recorder.listening().is_some() {
            return;
        }
        let input = self.input();
        let unavailable = match &input {
            None => Some(Refusal::SpeechUnavailable),
            Some(i) => i.capabilities().unavailable.map(Refusal::from),
        };
        // A press always cancels a running voice countdown first (spec
        // §4.4), whether or not anything can listen.
        let (id, over) = core.exec.listen(&self.port);
        core.epoch += 1;
        core.unadmitted = None;
        let (Some(input), None) = (input, unavailable) else {
            let refusal = unavailable.unwrap_or(Refusal::SpeechUnavailable);
            tracing::info!(target: "acd_voice", refusal = ?refusal, "voice cannot listen");
            let r = core.exec.listen_failed(refusal);
            self.emit(&mut core.exec, r);
            return;
        };
        if let Start::Replaced(old) = core.recorder.start(id, Instant::now()) {
            input.cancel(old);
        }
        if let Err(e) = input.start(id, core.recorder.limits().max) {
            core.recorder.cancel();
            tracing::info!(target: "acd_voice", error = ?e, "recording refused");
            let r = core.exec.listen_failed(Refusal::from(e));
            self.emit(&mut core.exec, r);
            return;
        }
        core.frozen = Some(snapshot);
        core.over = over.clone();
        tracing::info!(target: "acd_voice", "listening");
        let hud = Hud::Listening { id, partial: None, over: over.map(Box::new) };
        self.emit(&mut core.exec, Reaction { hud, select: None });
        self.schedule_tick(core.recorder.limits().max);
    }

    fn stop_listening(&self, core: &mut Core) {
        match core.recorder.stop(Instant::now()) {
            None => {}
            Some(Stopped::Finishing(generation)) => {
                if let Some(input) = self.input() {
                    input.stop(generation);
                }
                self.schedule_tick(core.recorder.limits().final_timeout);
            }
            // Released at or after the cap, before the timer ran (S9).
            Some(Stopped::TooLong(generation)) => {
                if let Some(input) = self.input() {
                    input.cancel(generation);
                }
                self.heard(core, generation, Heard::TooLong);
            }
        }
    }

    fn cancel_listening(&self, core: &mut Core) {
        let Some(generation) = core.recorder.cancel() else {
            return;
        };
        if let Some(input) = self.input() {
            input.cancel(generation);
        }
        core.frozen = None;
        core.over = None;
        tracing::info!(target: "acd_voice", "recording cancelled");
        let r = core.exec.resume();
        let r = core.deferred.take().unwrap_or(r);
        self.emit(&mut core.exec, r);
    }

    /// Esc or Cancel after the final left the recorder and before admission:
    /// the new epoch makes admission drop it. `id` is the recording the HUD
    /// showed (`None`: whatever is current). True if there was one.
    fn cancel_unadmitted(&self, core: &mut Core, id: Option<u64>) -> bool {
        let Some(generation) = core.unadmitted else {
            return false;
        };
        if id.is_some_and(|i| i != generation) {
            return false;
        }
        core.unadmitted = None;
        core.epoch += 1;
        tracing::info!(target: "acd_voice", "transcript cancelled before admission");
        let r = core.exec.resume();
        let r = core.deferred.take().unwrap_or(r);
        self.emit(&mut core.exec, r);
        true
    }

    /// How the recording ended (plan S8, S9). A transcript is routed on its
    /// own task with the admission token taken here, under the lock (S23).
    fn heard(&self, core: &mut Core, generation: u64, heard: Heard) {
        let snapshot = core.frozen.take();
        core.over = None;
        let deferred = core.deferred.take();
        let refusal = match heard {
            Heard::Transcript(text) => {
                if let Some(late) = deferred {
                    self.emit(&mut core.exec, late);
                }
                let token = Admission { session: core.session, epoch: core.epoch };
                let (Some(snapshot), Some(me)) = (snapshot, self.me.upgrade()) else {
                    return;
                };
                core.unadmitted = Some(generation);
                self.runtime.spawn(async move {
                    let _ = me.utterance(text, snapshot, Source::Speech, Some(token)).await;
                });
                return;
            }
            Heard::Ignored(why) => {
                tracing::info!(target: "acd_voice", ignored = ?why, "recording ignored");
                let r = core.exec.resume();
                self.emit(&mut core.exec, deferred.unwrap_or(r));
                return;
            }
            Heard::Failed(e) => Refusal::from(e),
            Heard::TooLong => Refusal::RecordingTooLong,
            Heard::TimedOut => Refusal::SpeechUnavailable,
        };
        tracing::info!(target: "acd_voice", refusal = ?refusal, "recording failed");
        if let Some(late) = deferred {
            self.emit(&mut core.exec, late);
        }
        let r = core.exec.listen_failed(refusal);
        self.emit(&mut core.exec, r);
    }

    fn schedule_tick(&self, after: Duration) {
        let me = self.me.clone();
        self.runtime.spawn(async move {
            tokio::time::sleep(after).await;
            if let Some(this) = me.upgrade() {
                this.tick();
            }
        });
    }

    /// The 60 s cap and the final timeout (spec §6.2, plan S5, S9).
    fn tick(&self) {
        let mut core = self.core.lock().unwrap();
        let Some((generation, heard)) = core.recorder.tick(Instant::now()) else {
            return;
        };
        if let Some(input) = self.input() {
            input.cancel(generation);
        }
        self.heard(&mut core, generation, heard);
    }

    /// The HUD rendered proposal `id` (D22).
    pub fn shown(&self, id: u64) {
        self.act(|exec, port| exec.shown(port, id));
    }

    /// Enter or a click on the HUD's Confirm for proposal `id`.
    pub fn confirm(&self, id: u64) {
        self.act(|exec, port| exec.confirm(port, Some(id)));
    }

    /// A digit or click on candidate `index` of list `choice_id`, the one
    /// the HUD rendered; a list since replaced is ignored.
    pub fn choose(&self, choice_id: u64, index: usize) {
        self.act(|exec, _| exec.choose(Some(choice_id), index));
    }

    /// Esc or Cancel. `Some(id)` is the `input_id` of the state the HUD
    /// showed; a cancel for a state since replaced is ignored. `None`
    /// cancels whatever is current.
    pub fn cancel(&self, id: Option<u64>) {
        let mut core = self.core.lock().unwrap();
        let active = core.recorder.active();
        if active.is_some() && (id.is_none() || id == active) {
            self.cancel_listening(&mut core);
            return;
        }
        if self.cancel_unadmitted(&mut core, id) {
            return;
        }
        if let Some(reaction) = core.exec.cancel(&self.port, id) {
            self.emit(&mut core.exec, reaction);
        }
    }

    /// Esc in the inbox panel: cancels a step that is still thinking. False
    /// when nothing is, so the panel handles Esc itself.
    pub fn cancel_thinking(&self) -> bool {
        let mut core = self.core.lock().unwrap();
        match core.exec.cancel_thinking(&self.port) {
            Some(reaction) => {
                self.emit(&mut core.exec, reaction);
                true
            }
            None => false,
        }
    }

    /// Runs `f` on the executor and publishes its reaction while still
    /// holding the core lock, so states reach the HUD in the order they
    /// happened (B3).
    fn act(&self, f: impl FnOnce(&mut Executor, &AcdPort) -> Option<Reaction>) {
        let mut core = self.core.lock().unwrap();
        if let Some(reaction) = f(&mut core.exec, &self.port) {
            self.emit(&mut core.exec, reaction);
        }
    }

    /// Like `act`, for what happens while no one asked: an outcome or a
    /// display timeout. During a recording the executor still moves on but
    /// the HUD keeps showing Listening; the reaction waits in `deferred`.
    fn act_quiet(&self, f: impl FnOnce(&mut Executor, &AcdPort) -> Option<Reaction>) {
        let mut core = self.core.lock().unwrap();
        let Some(reaction) = f(&mut core.exec, &self.port) else {
            return;
        };
        if core.recorder.active().is_some() || core.unadmitted.is_some() {
            core.deferred = Some(reaction);
        } else {
            self.emit(&mut core.exec, reaction);
        }
    }

    fn emit(&self, exec: &mut Executor, mut reaction: Reaction) {
        loop {
            if let Some(card) = &reaction.select {
                self.sink.select(card);
            }
            let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
            let delivered = self.sink.hud(seq, &reaction.hud);
            let Some(id) = reaction.hud.awaiting_display() else {
                return;
            };
            if delivered {
                self.watch_display(id);
                return;
            }
            // The HUD cannot show it: fail closed (D22).
            match exec.display_failed(id) {
                Some(r) => reaction = r,
                None => return,
            }
        }
    }

    fn watch_display(&self, id: u64) {
        let me = self.me.clone();
        self.runtime.spawn(async move {
            tokio::time::sleep(DISPLAY_TIMEOUT).await;
            if let Some(this) = me.upgrade() {
                this.act_quiet(|exec, _| exec.display_failed(id));
            }
        });
    }

    fn freeze(&self, panel_open: bool) -> Snapshot {
        let presentation = self.presentation.lock().unwrap().clone().unwrap_or_default();
        freeze(&self.inbox.stamped_view(), presentation, panel_open)
    }
}

fn outcome_tag(o: ActionOutcome) -> &'static str {
    match o {
        ActionOutcome::Fired => "fired",
        ActionOutcome::Delivered => "delivered",
        ActionOutcome::Rejected { .. } => "rejected",
        ActionOutcome::Uncertain => "uncertain",
        ActionOutcome::Cancelled => "cancelled",
        ActionOutcome::NotSent { .. } => "not_sent",
    }
}

async fn forward_outcomes(this: Weak<Controller>, mut rx: broadcast::Receiver<ActionEvent>) {
    loop {
        let event = rx.recv().await;
        let Some(this) = this.upgrade() else {
            return;
        };
        match event {
            Ok(e) => this.act_quiet(|exec, _| {
                let r = exec.outcome(&e.action_id, voice_outcome(e.outcome));
                if r.is_some() {
                    tracing::info!(target: "acd_voice", outcome = outcome_tag(e.outcome), "voice action outcome");
                }
                r
            }),
            Err(broadcast::error::RecvError::Lagged(_)) => this.act_quiet(|exec, port| exec.outcomes_lost(port)),
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

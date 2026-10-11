//! One utterance at a time: freeze, route, show, execute, report.
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use acd_client::inbox::Inbox;
use acd_client::voice::{ActionEvent, ActionOutcome};
use acd_voice::hud::Hud;
use acd_voice::plan::{Executor, Reaction};
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
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum InputError {
    #[error("The command is empty")]
    Empty,
    #[error("Voice is off. Turn it on in Settings.")]
    Off,
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
            core: Mutex::new(Core { exec: Executor::new(), enabled, session: 0 }),
            presentation: Mutex::new(None),
            model,
            sink,
            opts: RouteOptions::default(),
            session_tx: watch::channel(0).0,
            seq: AtomicU64::new(0),
            admission_hook: Mutex::new(None),
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
        core.session += 1;
        self.session_tx.send_replace(core.session);
        core.exec.reset(&self.port);
        self.emit(&mut core.exec, Reaction { hud: Hud::Hidden, select: None });
    }

    /// Test hook: `hook` runs just before admission, with no lock held, so
    /// a test can force "voice turned off after the input arrived but
    /// before it was admitted".
    #[doc(hidden)]
    pub fn on_admission_for_test(&self, hook: impl Fn() + Send + Sync + 'static) {
        *self.admission_hook.lock().unwrap() = Some(Arc::new(hook));
    }

    /// The step-1 stand-in for a recording: typed text as the transcript.
    pub async fn text_input(&self, text: String, panel_open: bool) -> Result<(), InputError> {
        if text.trim().is_empty() {
            return Err(InputError::Empty);
        }
        let started = Instant::now();
        let transcript = VoiceText::new(text);
        let snapshot = self.freeze(panel_open);
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
            let session = core.session;
            let changes = self.session_tx.subscribe();
            // A HUD that waits for input takes the utterance as its answer (D21).
            if let Some(r) = core.exec.reply(&self.port, &transcript, self.opts.locale) {
                self.emit(&mut core.exec, r);
                None
            } else {
                let (n, thinking) = core.exec.begin(&self.port, snapshot.clone(), transcript.clone());
                self.emit(&mut core.exec, thinking);
                Some((session, n, changes))
            }
        };
        let Some((session, n, mut changes)) = admitted else {
            tracing::info!(target: "acd_voice", chars = transcript.char_len(), route = "reply", "utterance routed");
            return Ok(());
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
            route = routed.tag(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "utterance routed"
        );
        if let Some(r) = core.exec.routed(n, routed) {
            self.emit(&mut core.exec, r);
        }
        Ok(())
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
        self.act(|exec, port| exec.cancel(port, id));
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
                this.act(|exec, _| exec.display_failed(id));
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
            Ok(e) => this.act(|exec, _| {
                let r = exec.outcome(&e.action_id, voice_outcome(e.outcome));
                if r.is_some() {
                    tracing::info!(target: "acd_voice", outcome = outcome_tag(e.outcome), "voice action outcome");
                }
                r
            }),
            Err(broadcast::error::RecvError::Lagged(_)) => this.act(|exec, port| exec.outcomes_lost(port)),
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

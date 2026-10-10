//! Keys delivery engine (spec §5.1, §6.2): answers a dialog with exactly one
//! keystroke, guarded by the dialog's screen digest, then gathers
//! evidence of the outcome. This is the only place in the crate that sends
//! keys.
//!
//! P3 registers a `KeysTarget` for every keys-answerable occurrence and may
//! provide `Evidence`. A key never reaches a screen whose dialog digest
//! differs from the target's, and nothing is labelled `inbox` without
//! evidence that matches this occurrence.
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use acd_core::{Answer, Core, CorrelationKey, DeliverError, Observation};
use acd_protocol::{
    AgentKind, AnswerPath, ErrorCode, Request, RequestStatus, ResolveReason, ResolvedBy,
};
use async_trait::async_trait;
use serde_json::json;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, sleep_until, timeout, timeout_at};

use super::client::{CALL_TIMEOUT, CallError, Herdr, KNOWN_REFUSALS, ScreenRead};
use super::reconciler::{Hint, PaneMap, PaneRef};
pub use super::screen::DialogIdentity;
use super::screen::{Fingerprint, GuardDigest, find_dialog, guard_digest, prompt_line};

/// Consecutive reads without the dialog before it counts as closed or gone.
pub(crate) const ABSENT_READS: u8 = 2;
/// A prompt check that starts later than this after its slot is unavailable.
const PROMPT_CHECK_GRACE: Duration = Duration::from_millis(250);
/// How long a prompt check's read may take.
const PROMPT_CHECK_READ: Duration = Duration::from_millis(500);

/// One keys-answerable dialog occurrence. P3 registers it right after
/// opening the request: `request_id = core.request_id_for(agent_id, &key)`.
/// The request id is unique per occurrence, so a later occurrence of the
/// same reusable key can never be served or removed by mistake.
///
/// Sealed: P3 must build targets only with `KeysTarget::from_screen`, from
/// the detection snapshot the request was opened from, so the fingerprint
/// and identity always come from a parsed dialog. The fields are read-only
/// outside this crate.
#[derive(Clone, Debug)]
pub struct KeysTarget {
    pub(crate) request_id: String,
    pub(crate) agent_id: String,
    pub(crate) key: CorrelationKey,
    /// `screen::guard_fingerprint` of the detection snapshot, for display.
    pub(crate) fingerprint: Fingerprint,
    /// `screen::guard_digest` of the same snapshot: what the guard compares.
    pub(crate) digest: GuardDigest,
    /// The dialog's identity in that snapshot.
    pub(crate) identity: DialogIdentity,
    /// Option id -> herdr key name: `"1"`..`"9"` or `"esc"` only.
    pub(crate) option_keys: HashMap<String, String>,
    /// The pane incarnation this target is bound to (G1); `None` is unbound.
    pub(crate) incarnation: Option<u64>,
}

impl KeysTarget {
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub fn key(&self) -> &CorrelationKey {
        &self.key
    }

    pub fn fingerprint(&self) -> &Fingerprint {
        &self.fingerprint
    }

    pub fn identity(&self) -> &DialogIdentity {
        &self.identity
    }

    pub fn option_keys(&self) -> &HashMap<String, String> {
        &self.option_keys
    }

    /// A target for the dialog on `screen_text` (a `detection` read). Fails
    /// when the screen shows no dialog, the dialog has no fingerprint (a
    /// collapsed Codex dialog), an option key is not `"1"`..`"9"` or
    /// `"esc"`, or a digit names no option of the dialog.
    ///
    /// Register it only once the herdr agent exists (`core.agent(agent_id)`
    /// is `Some`): a target whose pane the reconciler does not know is
    /// dropped at its next poll.
    pub fn from_screen(
        request_id: String,
        agent_id: String,
        key: CorrelationKey,
        screen_text: &str,
        option_keys: HashMap<String, String>,
    ) -> Result<KeysTarget, &'static str> {
        let dialog = find_dialog(screen_text).ok_or("no dialog on screen")?;
        let digest = dialog.digest.ok_or("the dialog cannot be fingerprinted")?;
        check_keys(&option_keys)?;
        let digit_ok = |k: &str| {
            k == "esc"
                || dialog
                    .options
                    .iter()
                    .any(|o| k.parse::<u8>().is_ok_and(|n| n == o.number))
        };
        if !option_keys.values().all(|k| digit_ok(k)) {
            return Err("an option key names no option of the dialog");
        }
        Ok(KeysTarget {
            request_id,
            agent_id,
            key,
            fingerprint: digest.fingerprint(),
            digest,
            identity: dialog.identity,
            option_keys,
            incarnation: None,
        })
    }

    /// Binds the target to one pane incarnation (G1). A key is only ever
    /// sent while the pane map shows this incarnation for the agent.
    pub fn for_incarnation(mut self, incarnation: u64) -> KeysTarget {
        self.incarnation = Some(incarnation);
        self
    }

    /// The bound pane incarnation. `None` means unbound; P3 never leaves a
    /// target unbound.
    pub fn incarnation(&self) -> Option<u64> {
        self.incarnation
    }

    fn incarnation_ok(&self, pane: &PaneRef) -> bool {
        self.incarnation.is_none_or(|n| n == pane.incarnation)
    }

    /// The guard: a whole read whose dialog has exactly this target's digest,
    /// all 32 bytes.
    fn guard_passes(&self, read: &ScreenRead) -> bool {
        !read.truncated && guard_digest(&read.text) == Some(self.digest)
    }

    /// Whether `self` is still the target the guard checked: same digest,
    /// identity and key for `option_id`.
    fn unchanged_since(&self, guarded: &KeysTarget, option_id: &str) -> bool {
        self.digest == guarded.digest
            && self.identity == guarded.identity
            && self.option_keys.get(option_id) == guarded.option_keys.get(option_id)
    }

    /// Whether `text` still shows this target's dialog, possibly redrawn or
    /// rewrapped. No dialog, or a dialog with another identity, is absent.
    pub(crate) fn shown_in(&self, text: &str) -> bool {
        find_dialog(text).is_some_and(|d| d.identity == self.identity)
    }
}

fn check_keys(option_keys: &HashMap<String, String>) -> Result<(), &'static str> {
    if option_keys.values().all(|k| is_guardable_key(k)) {
        Ok(())
    } else {
        Err("option keys must be \"1\"..\"9\" or \"esc\"")
    }
}

/// Retired request ids remembered so a removed target is never re-set.
const RETIRED_CAP: usize = 4096;

#[derive(Default)]
struct TargetsInner {
    live: HashMap<String, KeysTarget>,
    retired: VecDeque<String>,
    retired_set: HashSet<String>,
    rivals: HashMap<String, Rivals>,
}

/// Whether a screen shows the command a Codex target answers. P3 builds it
/// from its dialog matcher; herdr only calls it on whole (untruncated) reads.
pub type ShowsCommand = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// A Codex target's validated rival inventory (C6) and the matcher the
/// delivery re-reads every rival with (C7).
#[derive(Clone)]
struct Rivals {
    inventory: Vec<(String, u64)>,
    shows: ShowsCommand,
}

impl TargetsInner {
    fn retire(&mut self, request_id: &str) {
        self.rivals.remove(request_id);
        if self.retired_set.insert(request_id.to_owned()) {
            self.retired.push_back(request_id.to_owned());
            if self.retired.len() > RETIRED_CAP
                && let Some(old) = self.retired.pop_front()
            {
                self.retired_set.remove(&old);
            }
        }
    }
}

/// Registered targets, by request id. A target is immutable for its
/// request id (D8): the content the user reviewed is the content the guard checks.
#[derive(Default)]
pub struct KeysTargets(Mutex<TargetsInner>);

impl KeysTarget {
    fn same_as(&self, other: &KeysTarget) -> bool {
        self.agent_id == other.agent_id
            && self.key == other.key
            && self.digest == other.digest
            && self.identity == other.identity
            && self.option_keys == other.option_keys
            && self.incarnation == other.incarnation
    }
}

/// Keys the guard can vouch for. The fingerprint blanks the cursor marker,
/// so it cannot tell which option Enter would pick: Enter is never allowed.
fn is_guardable_key(k: &str) -> bool {
    k == "esc" || matches!(k.as_bytes(), [b'1'..=b'9'])
}

impl KeysTargets {
    fn lock(&self) -> MutexGuard<'_, TargetsInner> {
        self.0.lock().expect("keys targets")
    }

    /// Registers a target once for `t.request_id`. Identical registration is
    /// idempotent; a different or retired target is rejected. Rejects any
    /// option key other than `"1"`..`"9"` or `"esc"`.
    ///
    /// Register only once the herdr agent exists (`core.agent(agent_id)` is
    /// `Some`): a target whose pane the reconciler does not know is dropped
    /// at its next poll.
    pub fn set(&self, t: KeysTarget) -> Result<(), &'static str> {
        check_keys(&t.option_keys)?;
        let mut inner = self.lock();
        if inner.retired_set.contains(&t.request_id) {
            return Err("this request's target was already retired");
        }
        match inner.live.get(&t.request_id) {
            Some(old) if old.same_as(&t) => Ok(()),
            Some(_) => Err("a different target is already registered for this request"),
            None => {
                inner.live.insert(t.request_id.clone(), t);
                Ok(())
            }
        }
    }

    pub fn get(&self, request_id: &str) -> Option<KeysTarget> {
        self.lock().live.get(request_id).cloned()
    }

    /// Stores the validated Codex rival inventory beside the immutable lease,
    /// with the matcher for the command: before the key, every rival in the
    /// inventory is read again and must not show it (C7).
    pub fn set_rivals(&self, request_id: &str, inventory: Vec<(String, u64)>, shows: ShowsCommand) {
        self.lock()
            .rivals
            .insert(request_id.to_owned(), Rivals { inventory, shows });
    }

    pub fn rivals(&self, request_id: &str) -> Option<Vec<(String, u64)>> {
        self.lock()
            .rivals
            .get(request_id)
            .map(|r| r.inventory.clone())
    }

    fn rivals_of(&self, request_id: &str) -> Option<Rivals> {
        self.lock().rivals.get(request_id).cloned()
    }

    pub fn remove(&self, request_id: &str) -> Option<KeysTarget> {
        let mut inner = self.lock();
        inner.retire(request_id);
        inner.live.remove(request_id)
    }

    pub fn for_agent(&self, agent_id: &str) -> Vec<KeysTarget> {
        self.lock()
            .live
            .values()
            .filter(|t| t.agent_id == agent_id)
            .cloned()
            .collect()
    }

    pub(crate) fn all(&self) -> Vec<KeysTarget> {
        self.lock().live.values().cloned().collect()
    }

    /// Drops every target of an agent whose pane is gone.
    pub(crate) fn remove_agent(&self, agent_id: &str) {
        let mut inner = self.lock();
        let ids: Vec<String> = inner
            .live
            .values()
            .filter(|t| t.agent_id == agent_id)
            .map(|t| t.request_id.clone())
            .collect();
        for id in ids {
            inner.retire(&id);
            inner.live.remove(&id);
        }
    }
}

/// One async lock per agent (D7). The engine holds it from before its
/// final guard read until the key write completes, so waiting for the
/// lock never ages a screen check (Sol B5). P3 holds it around every
/// revalidation, observation and target registration on a herdr agent,
/// so a lease check can never pass on a state P3 is about to change.
#[derive(Default)]
pub struct AgentLocks(Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>);

impl AgentLocks {
    pub async fn lock(&self, agent_id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let mutex = {
            let mut map = self.0.lock().expect("agent locks");
            map.retain(|_, mutex| Arc::strong_count(mutex) > 1);
            map.entry(agent_id.to_owned()).or_default().clone()
        };
        mutex.lock_owned().await
    }

    pub fn len(&self) -> usize {
        self.0.lock().expect("agent locks").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Consistent,
    Contradicts,
    NoEvidence,
}

/// Correlated outcome evidence (spec §5.1, §6.2 Attribution, §6.3 Completion).
/// The engine bounds the wait with `KeysTiming::evidence_timeout`.
///
/// Occurrence safety: `target.request_id` names the occurrence, and
/// `sent_at` is when the key's write completed. Implementations (P3) must
/// only use evidence for that request id that arrived after `sent_at`, and
/// must never reuse a verdict across request ids.
#[async_trait]
pub trait Evidence: Send + Sync {
    async fn outcome(&self, target: &KeysTarget, option_id: &str, sent_at: Instant) -> Verdict;
}

/// P2's default: no evidence source exists yet, so nothing is ever `inbox`.
pub struct NoEvidence;

#[async_trait]
impl Evidence for NoEvidence {
    async fn outcome(&self, _: &KeysTarget, _: &str, _: Instant) -> Verdict {
        Verdict::NoEvidence
    }
}

#[derive(Clone, Debug)]
pub struct KeysTiming {
    /// Guard read and send must finish inside it (adapter contract: 10 s).
    pub deliver_budget: Duration,
    /// Detection read interval after the send.
    pub poll: Duration,
    /// How long detection is polled after the send.
    pub window: Duration,
    /// Prompt-line checks, measured from the send.
    pub prompt_checks: [Duration; 3],
    /// Longest wait for `Evidence::outcome` (P3's hook correlation window is 30 s).
    pub evidence_timeout: Duration,
}

impl Default for KeysTiming {
    fn default() -> KeysTiming {
        KeysTiming {
            deliver_budget: Duration::from_secs(8),
            poll: Duration::from_millis(150),
            window: Duration::from_secs(3),
            prompt_checks: [
                Duration::from_millis(300),
                Duration::from_secs(1),
                Duration::from_secs(3),
            ],
            evidence_timeout: Duration::from_secs(35),
        }
    }
}

/// herdr's answer to the key, as handed to the confirmation task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Delivery {
    Accepted,
    Refused,
    Uncertain,
}

fn not_delivered(code: ErrorCode) -> DeliverError {
    DeliverError::NotDelivered(code)
}

/// The adapter's keys state: targets, the evidence source and timing.
pub(crate) struct Engine {
    targets: Arc<KeysTargets>,
    locks: Arc<AgentLocks>,
    evidence: Mutex<Arc<dyn Evidence>>,
    timing: KeysTiming,
}

impl Engine {
    pub(crate) fn new(timing: KeysTiming) -> Engine {
        Engine {
            targets: Arc::default(),
            locks: Arc::default(),
            evidence: Mutex::new(Arc::new(NoEvidence)),
            timing,
        }
    }

    pub(crate) fn targets(&self) -> Arc<KeysTargets> {
        self.targets.clone()
    }

    pub(crate) fn locks(&self) -> Arc<AgentLocks> {
        self.locks.clone()
    }

    pub(crate) fn set_evidence(&self, e: Arc<dyn Evidence>) {
        *self.evidence.lock().expect("evidence") = e;
    }

    /// Whether `request_id` is still ours to answer with `key`: claimed for
    /// keys in the core, and its target unchanged since the guard (same
    /// digest, identity and key for the option), and its pane still the
    /// incarnation the target is bound to (else `RequestGone`). A Codex
    /// target's rival inventory must also equal the validated one (C6, else
    /// `RequestNotReady`: the reconciler revalidates it).
    fn lease(
        &self,
        core: &Weak<Core>,
        panes: &PaneMap,
        target: &KeysTarget,
        option_id: &str,
    ) -> Result<(), ErrorCode> {
        let Some(core) = core.upgrade() else {
            return Err(ErrorCode::RequestGone);
        };
        let claimed = core.request(&target.request_id).is_some_and(|r| {
            r.status == RequestStatus::Answering && r.answer_path == AnswerPath::Keys
        });
        let same_pane = panes
            .read()
            .expect("pane map")
            .get(&target.agent_id)
            .is_some_and(|p| target.incarnation_ok(p));
        let unchanged = self
            .targets
            .get(&target.request_id)
            .is_some_and(|t| t.unchanged_since(target, option_id));
        if !(claimed && same_pane && unchanged) {
            return Err(ErrorCode::RequestGone);
        }
        let rivals_ok = self
            .targets
            .rivals(&target.request_id)
            .is_none_or(|v| v == codex_inventory(panes, &target.agent_id));
        if !rivals_ok {
            return Err(ErrorCode::RequestNotReady);
        }
        Ok(())
    }

    /// C7: reads every rival in the validated inventory again; each must be
    /// a whole read that does not show the command. A rival missing from the
    /// pane map, unreadable before `deadline`, truncated or showing the
    /// command refuses the key.
    async fn rivals_clear(
        &self,
        panes: &PaneMap,
        rivals: &Rivals,
        deadline: Instant,
    ) -> Result<(), ErrorCode> {
        for (id, incarnation) in &rivals.inventory {
            let pane = panes
                .read()
                .expect("pane map")
                .get(id)
                .filter(|p| p.incarnation == *incarnation)
                .cloned();
            let Some(pane) = pane else {
                return Err(ErrorCode::RequestNotReady);
            };
            let read = pane
                .herdr
                .read_screen(&pane.pane_id, "detection", None, deadline)
                .await;
            match read {
                Ok(r) if !r.truncated && !(rivals.shows)(&r.text) => {}
                _ => {
                    tracing::debug!(rival = %id, "a Codex rival may show the command; key refused");
                    return Err(ErrorCode::RequestNotReady);
                }
            }
        }
        Ok(())
    }

    /// Runs inline: nothing is sent from a spawned task, so no send can start
    /// after this future is dropped.
    pub(crate) async fn deliver(
        &self,
        core: Weak<Core>,
        panes: &PaneMap,
        request: &Request,
        answer: &Answer,
    ) -> Result<(), DeliverError> {
        let option_id = match answer {
            Answer::Option(o) => o.as_str(),
            // Free text on keys is deferred (spec §11 item 1).
            Answer::Text(_) => return Err(not_delivered(ErrorCode::NotSupported)),
        };
        let deadline = Instant::now() + self.timing.deliver_budget;
        let target = match self.targets.get(&request.id) {
            Some(t) if t.agent_id == request.agent_id => t,
            _ => return Err(not_delivered(ErrorCode::RequestNotReady)),
        };
        let Some(key) = target.option_keys.get(option_id).cloned() else {
            return Err(not_delivered(ErrorCode::BadRequest));
        };
        // Clone out of the map so no lock is held across an await.
        let pane = panes
            .read()
            .expect("pane map")
            .get(&request.agent_id)
            .cloned();
        let Some(pane) = pane else {
            return Err(not_delivered(ErrorCode::AgentGone));
        };
        if !target.incarnation_ok(&pane) {
            return Err(not_delivered(ErrorCode::RequestGone));
        }

        // A lock wait must never sit between the screen check and the key.
        let lock = match timeout_at(deadline, self.locks.lock(&request.agent_id)).await {
            Ok(guard) => guard,
            Err(_) => return Err(not_delivered(ErrorCode::SourceUnavailable)),
        };

        // Guard: the screen must show exactly the dialog the user saw.
        let read = pane
            .herdr
            .read_screen(&pane.pane_id, "detection", None, deadline)
            .await
            .map_err(|_| not_delivered(ErrorCode::SourceUnavailable))?;
        if !target.guard_passes(&read) {
            return Err(not_delivered(ErrorCode::ScreenMismatch));
        }
        // Always `None` today: the guard just required a parsed dialog, and
        // `prompt_line` reads no input box while a dialog is up. So the
        // spec's "read the prompt line before the key" cannot see a draft,
        // and a draft that already ends in the sent digit is reported as a
        // stray (`uncertain`, the safe side).
        let mut baseline = prompt_line(pane.kind, &read.text);
        // C7: no rival may show the command now. Read under the lock, after
        // the guard; the target is then guarded again, so only the connect
        // and the lease lie between the last target read and the key.
        if let Some(rivals) = self.targets.rivals_of(&target.request_id) {
            self.rivals_clear(panes, &rivals, deadline)
                .await
                .map_err(not_delivered)?;
            // The rival reads can take seconds: the target dialog may have
            // been replaced meanwhile, so guard it again right before the key.
            let read = pane
                .herdr
                .read_screen(&pane.pane_id, "detection", None, deadline)
                .await
                .map_err(|_| not_delivered(ErrorCode::SourceUnavailable))?;
            if !target.guard_passes(&read) {
                return Err(not_delivered(ErrorCode::ScreenMismatch));
            }
            baseline = prompt_line(pane.kind, &read.text);
        }

        // Connect first, then check the lease, so only the local write lies
        // between the check and the key reaching herdr. The screen can still
        // change in that window (spec §4 invariant 2, §11 item 8): one key,
        // never retried, `uncertain` unless evidence matches.
        let connected = pane
            .herdr
            .connect_until(deadline)
            .await
            .map_err(|_| not_delivered(ErrorCode::SourceUnavailable))?;
        if let Err(code) = self.lease(&core, panes, &target, option_id) {
            drop(lock);
            drop(connected); // unused: nothing was sent
            return Err(not_delivered(code));
        }

        tracing::debug!(pane_id = %pane.pane_id, request_id = %target.request_id, "herdr agent.send_keys");
        let written = connected
            .write(
                "agent.send_keys",
                json!({"target": pane.pane_id, "keys": [key]}),
            )
            .await
            // herdr frames by newline: a partial line is never dispatched.
            .map_err(|_| not_delivered(ErrorCode::SourceUnavailable))?;
        drop(lock);
        let t_send = written.at();

        // Start confirming now, before the reply, so the first prompt check
        // is measured from the write. The task only reads.
        let (verdict_tx, verdict_rx) = oneshot::channel();
        let evidence = self.evidence.lock().expect("evidence").clone();
        tokio::spawn(
            Confirm {
                core,
                targets: self.targets.clone(),
                evidence,
                timing: self.timing.clone(),
                herdr: pane.herdr.clone(),
                pane_id: pane.pane_id.clone(),
                kind: pane.kind,
                hints: pane.hints.clone(),
                target,
                option_id: option_id.to_owned(),
                key,
                baseline,
                t_send,
            }
            .run(verdict_rx),
        );

        let reply = timeout_at(deadline.min(t_send + CALL_TIMEOUT), written.reply()).await;
        let (delivery, result) = match reply {
            Ok(Ok(v)) if v["type"].as_str() == Some("ok") => (Delivery::Accepted, Ok(())),
            Ok(Err(CallError::Server { code, .. })) if KNOWN_REFUSALS.contains(&code.as_str()) => {
                tracing::debug!(%code, "herdr refused the key before acting");
                (
                    Delivery::Refused,
                    Err(not_delivered(ErrorCode::SourceUnavailable)),
                )
            }
            Ok(Ok(_)) => (
                Delivery::Uncertain,
                Err(DeliverError::Uncertain(
                    "herdr replied to the key with an unexpected type".into(),
                )),
            ),
            Ok(Err(e)) => (
                Delivery::Uncertain,
                Err(DeliverError::Uncertain(format!(
                    "herdr did not confirm the key: {e}"
                ))),
            ),
            Err(_) => (
                Delivery::Uncertain,
                Err(DeliverError::Uncertain(
                    "herdr did not confirm the key in time".into(),
                )),
            ),
        };
        let _ = verdict_tx.send(delivery);
        result
    }
}

/// Every Codex pane except `except`, sorted by id and incarnation.
pub fn codex_inventory(panes: &PaneMap, except: &str) -> Vec<(String, u64)> {
    let mut inventory: Vec<_> = panes
        .read()
        .expect("pane map")
        .iter()
        .filter(|(id, pane)| pane.kind == AgentKind::Codex && id.as_str() != except)
        .map(|(id, pane)| (id.clone(), pane.incarnation))
        .collect();
    inventory.sort();
    inventory
}

/// One step of the confirmation timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// Read `visible` and look for a stray digit in the input box.
    PromptCheck,
    /// Read `detection` and look for the dialog.
    Poll,
}

/// Every step, in time order; a prompt check comes first when both fall on
/// the same instant.
fn timeline(t_send: Instant, timing: &KeysTiming, digit: bool) -> Vec<(Instant, Step)> {
    let mut steps = Vec::new();
    if digit {
        steps.extend(
            timing
                .prompt_checks
                .iter()
                .map(|d| (t_send + *d, Step::PromptCheck)),
        );
    }
    let poll = timing.poll.max(Duration::from_millis(1));
    let end = t_send + timing.window;
    let mut at = t_send + poll;
    while at < end {
        steps.push((at, Step::Poll));
        at += poll;
    }
    steps.sort_by_key(|(at, step)| (*at, *step == Step::Poll));
    steps
}

/// Gathers reads and evidence after a key was written, and publishes the
/// outcome once `deliver` has handed over herdr's verdict.
struct Confirm {
    core: Weak<Core>,
    targets: Arc<KeysTargets>,
    evidence: Arc<dyn Evidence>,
    timing: KeysTiming,
    herdr: Herdr,
    pane_id: String,
    kind: AgentKind,
    hints: mpsc::Sender<Hint>,
    target: KeysTarget,
    option_id: String,
    key: String,
    /// The prompt line at the guard read; `None` while a dialog is up,
    /// which is always (see `Engine::deliver`).
    baseline: Option<String>,
    t_send: Instant,
}

/// What the reads after the send showed.
struct Observed {
    /// The last `ABSENT_READS` usable detection reads lacked the dialog
    /// (by identity: a redraw around a visible dialog is not a close).
    closed: bool,
    /// A stray digit in the input box, or a prompt check that could not run.
    stray: bool,
}

impl Confirm {
    async fn run(self, verdict: oneshot::Receiver<Delivery>) {
        let seen = self.observe().await;
        // A dropped sender means `deliver` was cancelled after the write.
        let delivery = verdict.await.unwrap_or(Delivery::Uncertain);
        match delivery {
            // herdr refused before acting; the request is open again and the
            // target watch handles it.
            Delivery::Refused => {}
            // The core resolves the request itself. The target stays for the
            // watch, which removes it once the dialog is confirmed gone.
            Delivery::Uncertain => {
                if seen.closed
                    && let Some(core) = self.core.upgrade()
                {
                    core.observe(Observation::DialogGoneById {
                        request_id: self.target.request_id.clone(),
                    });
                }
                let _ = self.hints.try_send(Hint::Pane(self.pane_id.clone()));
            }
            Delivery::Accepted => self.report(seen).await,
        }
    }

    async fn observe(&self) -> Observed {
        let digit = self.key != "esc";
        let steps = timeline(self.t_send, &self.timing, digit);
        let window_end = self.t_send + self.timing.window;
        let mut absent: u8 = 0;
        let mut stray = false;
        for (i, &(at, step)) in steps.iter().enumerate() {
            // Each read must finish before the next step is due.
            let next = steps.get(i + 1).map_or(window_end, |s| s.0).max(at);
            match step {
                Step::PromptCheck => {
                    sleep_until(at).await;
                    if Instant::now() > at + PROMPT_CHECK_GRACE {
                        stray = true; // unavailable counts like a stray
                        continue;
                    }
                    let read = self
                        .herdr
                        .read_screen(&self.pane_id, "visible", None, at + PROMPT_CHECK_READ)
                        .await;
                    stray |= match read {
                        // A truncated read is unavailable, like a failed one.
                        Ok(r) if r.truncated => true,
                        Ok(r) => prompt_line(self.kind, &r.text).is_some_and(|p| {
                            p.ends_with(self.key.as_str()) && Some(&p) != self.baseline.as_ref()
                        }),
                        Err(_) => true,
                    };
                }
                Step::Poll => {
                    if Instant::now() >= next {
                        continue; // missed its slot
                    }
                    sleep_until(at).await;
                    match self
                        .herdr
                        .read_screen(&self.pane_id, "detection", None, next)
                        .await
                    {
                        // A failed or truncated read is neither present nor absent.
                        Ok(r) if !r.truncated => {
                            if self.target.shown_in(&r.text) {
                                absent = 0;
                            } else {
                                absent = absent.saturating_add(1);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Observed {
            closed: absent >= ABSENT_READS,
            stray,
        }
    }

    /// herdr accepted the key: attribute the outcome and publish it.
    async fn report(self, seen: Observed) {
        let (by, reason) = if seen.stray {
            (ResolvedBy::Uncertain, Some(ResolveReason::StrayKeystroke))
        } else if !seen.closed {
            (ResolvedBy::Uncertain, None)
        } else {
            let outcome = self
                .evidence
                .outcome(&self.target, &self.option_id, self.t_send);
            let by = match timeout(self.timing.evidence_timeout, outcome).await {
                Ok(Verdict::Consistent) => ResolvedBy::Inbox,
                Ok(Verdict::Contradicts) => ResolvedBy::Terminal,
                Ok(Verdict::NoEvidence) | Err(_) => ResolvedBy::Uncertain,
            };
            (by, None)
        };
        let Some(core) = self.core.upgrade() else {
            return;
        };
        let request_id = self.target.request_id.clone();
        if seen.closed {
            // Removed before the resolution is published, so whoever sees
            // the resolution also sees the target gone.
            self.targets.remove(&request_id);
            core.observe(Observation::DialogGoneById {
                request_id: request_id.clone(),
            });
        }
        // Otherwise the target stays: the watch monitors the dialog until it
        // is really gone. Never another key.
        if let Some(reason) = reason {
            core.observe(Observation::RequestEndedWithReason {
                request_id,
                by,
                reason,
            });
        } else {
            core.observe(Observation::RequestEndedById { request_id, by });
        }
        let _ = self.hints.try_send(Hint::Pane(self.pane_id.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_rejects_a_target_altered_after_from_screen() {
        let dialog = std::fs::read_to_string(format!(
            "{}/../../tests/fixtures/herdr/claude-permission-bash.txt",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let mut t = KeysTarget::from_screen(
            "r1".into(),
            "a".into(),
            CorrelationKey("k".into()),
            &dialog,
            [("allow".to_string(), "1".to_string())].into(),
        )
        .unwrap();
        t.option_keys = [("allow".to_string(), "enter".to_string())].into();
        let targets = KeysTargets::default();
        assert!(targets.set(t).is_err());
        assert!(targets.get("r1").is_none());
    }

    fn claude_target() -> (KeysTarget, String) {
        let dialog = std::fs::read_to_string(format!(
            "{}/../../tests/fixtures/herdr/claude-permission-bash.txt",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let t = KeysTarget::from_screen(
            "r1".into(),
            "a".into(),
            CorrelationKey("k".into()),
            &dialog,
            [("allow".to_string(), "1".to_string())].into(),
        )
        .unwrap();
        (t, dialog)
    }

    #[test]
    fn a_target_is_set_once_per_request_id() {
        let (t, dialog) = claude_target();
        let targets = KeysTargets::default();
        targets.set(t.clone()).unwrap();
        targets.set(t.clone()).unwrap();
        let redrawn = dialog.replace("⏺ Creating empty file", "⏺ Creating an empty file");
        let other = KeysTarget::from_screen(
            "r1".into(),
            "a".into(),
            CorrelationKey("k".into()),
            &redrawn,
            [("allow".to_string(), "1".to_string())].into(),
        )
        .unwrap();
        assert!(
            targets.set(other.clone()).is_err(),
            "a different target never replaces"
        );
        assert_eq!(targets.get("r1").unwrap().fingerprint(), t.fingerprint());
        targets.remove("r1");
        assert!(
            targets.set(other).is_err(),
            "a retired request id never gets a target again"
        );
    }

    #[tokio::test]
    async fn agent_locks_serialize_per_agent_and_forget_idle_entries() {
        let locks = Arc::new(AgentLocks::default());
        let g = locks.lock("a").await;
        let l2 = locks.clone();
        let waiter = tokio::spawn(async move {
            l2.lock("a").await;
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !waiter.is_finished(),
            "a second lock on the same agent waits"
        );
        let _other = locks.lock("b").await;
        drop(g);
        waiter.await.unwrap();
        drop(_other);
        let _ = locks.lock("c").await;
        assert!(locks.len() <= 1, "idle entries are pruned");
    }

    /// The same target with one digest byte past the 16-hex prefix flipped:
    /// a screen that collides in the fingerprint but not in the digest.
    fn prefix_collision(t: &KeysTarget) -> KeysTarget {
        let mut other = t.clone();
        other.digest.0[31] ^= 1;
        assert_eq!(other.digest.fingerprint(), t.fingerprint);
        other
    }

    #[test]
    fn guard_compares_the_full_digest() {
        let (t, dialog) = claude_target();
        let read = ScreenRead {
            text: dialog,
            truncated: false,
        };
        assert!(t.guard_passes(&read));
        assert!(!prefix_collision(&t).guard_passes(&read));
        let truncated = ScreenRead {
            truncated: true,
            ..read
        };
        assert!(!t.guard_passes(&truncated));
    }

    #[test]
    fn lease_compares_the_full_digest() {
        let (t, _) = claude_target();
        assert!(t.unchanged_since(&t, "allow"));
        assert!(!prefix_collision(&t).unchanged_since(&t, "allow"));
    }

    #[test]
    fn guardable_keys() {
        for k in ["1", "5", "9", "esc"] {
            assert!(is_guardable_key(k), "{k}");
        }
        for k in ["0", "10", "enter", "Esc", "", " 1", "backspace", "١"] {
            assert!(!is_guardable_key(k), "{k}");
        }
    }

    #[test]
    fn timeline_puts_prompt_checks_first_and_stops_at_the_window() {
        let t = Instant::now();
        let timing = KeysTiming::default();
        let steps = timeline(t, &timing, true);
        let at_300: Vec<Step> = steps
            .iter()
            .filter(|(at, _)| *at == t + Duration::from_millis(300))
            .map(|(_, s)| *s)
            .collect();
        assert_eq!(at_300, vec![Step::PromptCheck, Step::Poll]);
        assert_eq!(
            steps.last(),
            Some(&(t + Duration::from_secs(3), Step::PromptCheck))
        );
        assert_eq!(
            steps.iter().filter(|(_, s)| *s == Step::Poll).count(),
            19,
            "150 ms polls strictly inside 3 s"
        );
        assert!(
            timeline(t, &timing, false)
                .iter()
                .all(|(_, s)| *s == Step::Poll)
        );
    }
}

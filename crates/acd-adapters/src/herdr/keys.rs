//! Keys delivery engine (spec §5.1, §6.2): answers a dialog with exactly one
//! keystroke, guarded by the dialog's screen fingerprint, then gathers
//! evidence of the outcome. This is the only place in the crate that sends
//! keys.
//!
//! P3 registers a `KeysTarget` for every keys-answerable occurrence and may
//! provide `Evidence`. A key never reaches a screen whose dialog fingerprint
//! differs from the target's, and nothing is labelled `inbox` without
//! evidence that matches this occurrence.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use acd_core::{Answer, Core, CorrelationKey, DeliverError, Observation};
use acd_protocol::{AgentKind, AnswerPath, ErrorCode, Request, RequestStatus, ResolvedBy};
use async_trait::async_trait;
use serde_json::json;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, sleep_until, timeout, timeout_at};

use super::client::{CALL_TIMEOUT, CallError, Herdr, KNOWN_REFUSALS};
use super::reconciler::{Hint, PaneMap};
pub use super::screen::DialogIdentity;
use super::screen::{Fingerprint, find_dialog, guard_fingerprint, prompt_line};

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
    /// `screen::guard_fingerprint` of the detection snapshot.
    pub(crate) fingerprint: Fingerprint,
    /// The dialog's identity in that snapshot.
    pub(crate) identity: DialogIdentity,
    /// Option id -> herdr key name: `"1"`..`"9"` or `"esc"` only.
    pub(crate) option_keys: HashMap<String, String>,
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
        let fingerprint = dialog
            .fingerprint
            .clone()
            .ok_or("the dialog cannot be fingerprinted")?;
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
            fingerprint,
            identity: dialog.identity,
            option_keys,
        })
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

/// Registered targets, by request id.
#[derive(Default)]
pub struct KeysTargets(Mutex<HashMap<String, KeysTarget>>);

/// Keys the guard can vouch for. The fingerprint blanks the cursor marker,
/// so it cannot tell which option Enter would pick: Enter is never allowed.
fn is_guardable_key(k: &str) -> bool {
    k == "esc" || matches!(k.as_bytes(), [b'1'..=b'9'])
}

impl KeysTargets {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, KeysTarget>> {
        self.0.lock().expect("keys targets")
    }

    /// Registers or replaces the target for `t.request_id`. Rejects any
    /// option key other than `"1"`..`"9"` or `"esc"`.
    ///
    /// Register only once the herdr agent exists (`core.agent(agent_id)` is
    /// `Some`): a target whose pane the reconciler does not know is dropped
    /// at its next poll.
    pub fn set(&self, t: KeysTarget) -> Result<(), &'static str> {
        check_keys(&t.option_keys)?;
        self.lock().insert(t.request_id.clone(), t);
        Ok(())
    }

    pub fn get(&self, request_id: &str) -> Option<KeysTarget> {
        self.lock().get(request_id).cloned()
    }

    pub fn remove(&self, request_id: &str) -> Option<KeysTarget> {
        self.lock().remove(request_id)
    }

    pub fn for_agent(&self, agent_id: &str) -> Vec<KeysTarget> {
        self.lock()
            .values()
            .filter(|t| t.agent_id == agent_id)
            .cloned()
            .collect()
    }

    pub(crate) fn all(&self) -> Vec<KeysTarget> {
        self.lock().values().cloned().collect()
    }

    /// Drops every target of an agent whose pane is gone.
    pub(crate) fn remove_agent(&self, agent_id: &str) {
        self.lock().retain(|_, t| t.agent_id != agent_id);
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
    evidence: Mutex<Arc<dyn Evidence>>,
    timing: KeysTiming,
}

impl Engine {
    pub(crate) fn new(timing: KeysTiming) -> Engine {
        Engine {
            targets: Arc::default(),
            evidence: Mutex::new(Arc::new(NoEvidence)),
            timing,
        }
    }

    pub(crate) fn targets(&self) -> Arc<KeysTargets> {
        self.targets.clone()
    }

    pub(crate) fn set_evidence(&self, e: Arc<dyn Evidence>) {
        *self.evidence.lock().expect("evidence") = e;
    }

    /// Whether `request_id` is still ours to answer with `key`: claimed for
    /// keys in the core, and its target unchanged since the guard (same
    /// fingerprint, identity and key for the option).
    fn lease_held(&self, core: &Weak<Core>, target: &KeysTarget, option_id: &str) -> bool {
        let Some(core) = core.upgrade() else {
            return false;
        };
        let claimed = core.request(&target.request_id).is_some_and(|r| {
            r.status == RequestStatus::Answering && r.answer_path == AnswerPath::Keys
        });
        claimed
            && self.targets.get(&target.request_id).is_some_and(|t| {
                t.fingerprint == target.fingerprint
                    && t.identity == target.identity
                    && t.option_keys.get(option_id) == target.option_keys.get(option_id)
            })
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

        // Guard: the screen must show exactly the dialog the user saw.
        let read = pane
            .herdr
            .read_screen(&pane.pane_id, "detection", None, deadline)
            .await
            .map_err(|_| not_delivered(ErrorCode::SourceUnavailable))?;
        if read.truncated || guard_fingerprint(&read.text).as_ref() != Some(&target.fingerprint) {
            return Err(not_delivered(ErrorCode::ScreenMismatch));
        }
        let baseline = prompt_line(pane.kind, &read.text);

        // Connect first, then check the lease, so only the local write lies
        // between the check and the key reaching herdr. The screen can still
        // change in that window (spec §4 invariant 2, §11 item 8): one key,
        // never retried, `uncertain` unless evidence matches.
        let connected = pane
            .herdr
            .connect_until(deadline)
            .await
            .map_err(|_| not_delivered(ErrorCode::SourceUnavailable))?;
        if !self.lease_held(&core, &target, option_id) {
            drop(connected); // unused: nothing was sent
            return Err(not_delivered(ErrorCode::RequestGone));
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
        let by = if seen.stray || !seen.closed {
            ResolvedBy::Uncertain
        } else {
            let outcome = self
                .evidence
                .outcome(&self.target, &self.option_id, self.t_send);
            match timeout(self.timing.evidence_timeout, outcome).await {
                Ok(Verdict::Consistent) => ResolvedBy::Inbox,
                Ok(Verdict::Contradicts) => ResolvedBy::Terminal,
                Ok(Verdict::NoEvidence) | Err(_) => ResolvedBy::Uncertain,
            }
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
        core.observe(Observation::RequestEndedById { request_id, by });
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

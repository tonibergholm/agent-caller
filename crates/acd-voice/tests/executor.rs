use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use acd_protocol::{Effect, ErrorCode};
use acd_voice::fast::Locale;
use acd_voice::hud::{Hud, HudEvent, OutcomeView};
use acd_voice::plan::*;
use acd_voice::refusal::Refusal;
use acd_voice::router::target::Provenance;
use acd_voice::router::{Proposal, RouteOptions, Routed, Step, route};
use acd_voice::safety::{Level, Origin};
use acd_voice::snapshot::fixtures::*;
use acd_voice::snapshot::{CardSnap, ConnStamp, Snapshot};
use acd_voice::text::VoiceText;

/// A scripted inbox: what each card looks like now, and what was enqueued
/// and cancelled. `enqueue` checks the guard the way acd-client does.
struct FakePort {
    live: Mutex<HashMap<String, Guard>>,
    enqueued: Mutex<Vec<Enqueue>>,
    cancelled: Mutex<Vec<String>>,
    cancel_works: AtomicBool,
    next: AtomicU64,
    refuse: Mutex<Option<EnqueueError>>,
}

fn guard_of(c: &CardSnap) -> Guard {
    Guard {
        conn: c.conn.unwrap(),
        agent_id: c.agent_id.clone(),
        request: c.pending.as_ref().map(|r| (r.id.clone(), r.revision)),
        named_send_only: false,
    }
}

impl FakePort {
    fn with(cards: &[&CardSnap]) -> Self {
        Self {
            live: Mutex::new(cards.iter().map(|c| (c.key.clone(), guard_of(c))).collect()),
            enqueued: Mutex::default(),
            cancelled: Mutex::default(),
            cancel_works: AtomicBool::new(true),
            next: AtomicU64::new(0),
            refuse: Mutex::default(),
        }
    }
    fn enqueued(&self) -> Vec<Enqueue> {
        self.enqueued.lock().unwrap().clone()
    }
    fn cancelled(&self) -> Vec<String> {
        self.cancelled.lock().unwrap().clone()
    }
    fn edit(&self, key: &str, f: impl FnOnce(&mut Guard)) {
        f(self.live.lock().unwrap().get_mut(key).unwrap());
    }
}

impl InboxPort for FakePort {
    fn enqueue(&self, op: Enqueue) -> Result<String, EnqueueError> {
        if let Some(e) = *self.refuse.lock().unwrap() {
            return Err(e);
        }
        let (card, guard) = match &op {
            Enqueue::Pick { card, guard, .. }
            | Enqueue::AnswerText { card, guard, .. }
            | Enqueue::Reply { card, guard, .. } => (card.clone(), guard.clone()),
        };
        let live = self.live.lock().unwrap().get(&card).cloned().ok_or(EnqueueError::UnknownCard)?;
        if live.conn != guard.conn {
            return Err(EnqueueError::ConnectionChanged);
        }
        // The flag is the planner's, not live state; acd-client checks it.
        if (live.agent_id, live.request) != (guard.agent_id, guard.request) {
            return Err(EnqueueError::Stale);
        }
        self.enqueued.lock().unwrap().push(op);
        Ok(format!("act-{}", self.next.fetch_add(1, Ordering::SeqCst)))
    }
    fn cancel(&self, id: &str) -> bool {
        self.cancelled.lock().unwrap().push(id.to_owned());
        self.cancel_works.load(Ordering::SeqCst)
    }
}

async fn say(exec: &mut Executor, port: &FakePort, s: &Snapshot, text: &str) -> Reaction {
    let (n, thinking) = match exec.begin(port, s.clone(), VoiceText::new(text), Locale::En) {
        Ok(admitted) => admitted,
        Err(refused) => return *refused,
    };
    assert!(matches!(thinking.hud, Hud::Thinking { .. }));
    let routed = route(s, &VoiceText::new(text), None, &RouteOptions::default()).await;
    exec.routed(n, routed).expect("the current utterance gets a reaction")
}

/// `say`, then the HUD's acknowledgement of what it showed (D22).
async fn say_shown(exec: &mut Executor, port: &FakePort, s: &Snapshot, text: &str) -> Reaction {
    let r = say(exec, port, s, text).await;
    match r.hud.awaiting_display() {
        Some(id) => exec.shown(port, id).unwrap_or(r),
        None => r,
    }
}

fn pick(card: &str, request: &str, option: &str) -> Enqueue {
    Enqueue::Pick {
        card: card.into(),
        option_id: option.into(),
        guard: Guard {
            conn: ConnStamp { instance: 1, conn_gen: 1 },
            agent_id: card.split_once('/').unwrap().1.into(),
            request: Some((request.into(), 1)),
            named_send_only: false,
        },
    }
}

fn error(refusal: Refusal) -> Hud {
    Hud::Error { refusal, transcript: None }
}

#[tokio::test]
async fn the_countdown_starts_only_after_the_hud_showed_the_step() {
    let a = pending("m", "a");
    let s = select(snapshot(vec![a.clone()]), "m/a");
    let port = FakePort::with(&[&a]);
    let mut exec = Executor::new();
    let r = say(&mut exec, &port, &s, "approve").await;
    let Hud::Countdown { proposal, proposal_id, action_id: None } = r.hud else {
        panic!("expected a countdown waiting for display, got {:?}", r.hud)
    };
    assert_eq!(proposal.option.as_deref(), Some("Yes"));
    assert_eq!(proposal.question.as_deref(), Some("npm test"));
    assert_eq!((proposal.provenance, proposal.level), (Some(Provenance::Selected), Level::Countdown));
    assert!(port.enqueued().is_empty(), "nothing before the HUD shows it");
    assert_eq!(exec.shown(&port, proposal_id + 1), None, "another proposal's ack does nothing");
    assert!(port.enqueued().is_empty());
    let r = exec.shown(&port, proposal_id).unwrap();
    assert!(matches!(r.hud, Hud::Countdown { action_id: Some(ref id), .. } if id == "act-0"));
    assert_eq!(port.enqueued(), [pick("m/a", "r-a", "allow")]);
    let fired = exec.outcome("act-0", VoiceOutcome::Fired).unwrap();
    assert!(matches!(fired.hud, Hud::Sending { too_late_to_cancel: false, .. }));
    assert!(fired.hud.fades(), "fades 1.5 s after the step fires");
    let done = exec.outcome("act-0", VoiceOutcome::Delivered).unwrap();
    assert!(matches!(done.hud, Hud::Done { outcome: OutcomeView::Delivered, .. }));
    assert_eq!(exec.current_action(), None);
}

#[tokio::test]
async fn a_display_failure_sends_nothing() {
    let a = pending("m", "a");
    let s = select(snapshot(vec![a.clone()]), "m/a");
    let port = FakePort::with(&[&a]);
    let mut exec = Executor::new();
    let id = say(&mut exec, &port, &s, "approve").await.hud.awaiting_display().unwrap();
    assert_eq!(exec.display_failed(id).unwrap().hud, error(Refusal::DisplayFailed));
    assert_eq!(exec.shown(&port, id), None, "a late ack changes nothing");
    let id = say(&mut exec, &port, &s, "yes always").await.hud.awaiting_display().unwrap();
    assert_eq!(exec.display_failed(id).unwrap().hud, error(Refusal::DisplayFailed));
    assert!(port.enqueued().is_empty());
}

#[tokio::test]
async fn only_a_shown_proposal_can_be_confirmed_and_only_by_its_id() {
    let a = pending("m", "a");
    let s = select(snapshot(vec![a.clone()]), "m/a");
    let port = FakePort::with(&[&a]);
    let mut exec = Executor::new();
    let Hud::Confirm { proposal, proposal_id } = say(&mut exec, &port, &s, "yes always").await.hud else {
        panic!("expected confirm")
    };
    assert_eq!(proposal.rule.as_deref(), Some("Bash(npm test:*)"));
    assert_eq!(proposal.effect, Some(Effect::AllowAlways));
    assert_eq!(exec.confirm(&port, Some(proposal_id)), None, "not shown yet");
    assert_eq!(exec.confirm(&port, None), None, "not even by voice");
    assert_eq!(exec.shown(&port, proposal_id), None);
    assert_eq!(exec.confirm(&port, Some(proposal_id + 1)), None, "a stale click");
    assert!(port.enqueued().is_empty());
    let r = exec.confirm(&port, Some(proposal_id)).unwrap();
    assert!(matches!(r.hud, Hud::Countdown { action_id: Some(_), .. }), "already on screen: counts down at once");
    assert_eq!(port.enqueued(), [pick("m/a", "r-a", "always")]);
}

#[tokio::test]
async fn a_revision_bump_before_enqueue_drops_the_step() {
    let a = pending("m", "a");
    let s = select(snapshot(vec![a.clone()]), "m/a");
    let port = FakePort::with(&[&a]);
    port.edit("m/a", |g| g.request = Some(("r-a".into(), 2)));
    let mut exec = Executor::new();
    assert_eq!(say_shown(&mut exec, &port, &s, "approve").await.hud, error(Refusal::SnapshotChanged));
    let port = FakePort::with(&[&a]);
    let id = say(&mut exec, &port, &s, "yes always").await.hud.awaiting_display().unwrap();
    exec.shown(&port, id);
    port.edit("m/a", |g| g.request = Some(("r-a".into(), 2)));
    assert_eq!(exec.confirm(&port, Some(id)).unwrap().hud, error(Refusal::SnapshotChanged));
    assert!(port.enqueued().is_empty());
}

#[tokio::test]
async fn a_connection_change_before_enqueue_drops_the_step() {
    let a = pending("m", "a");
    let s = select(snapshot(vec![a.clone()]), "m/a");
    let port = FakePort::with(&[&a]);
    port.edit("m/a", |g| g.conn.conn_gen = 2);
    let r = say_shown(&mut Executor::new(), &port, &s, "approve").await;
    assert_eq!(r.hud, error(Refusal::SnapshotChanged));
    assert!(port.enqueued().is_empty());
}

#[tokio::test]
async fn a_card_gone_before_enqueue_drops_the_step() {
    let (a, b) = (pending("m", "a"), pending("m", "b"));
    let s = select(snapshot(vec![a.clone(), b.clone()]), "m/a");
    let port = FakePort::with(&[&b]);
    let r = say_shown(&mut Executor::new(), &port, &s, "approve").await;
    assert_eq!(r.hud, error(Refusal::SnapshotChanged));
    assert!(port.enqueued().is_empty(), "never retargets to m/b");
}

#[test]
fn a_send_is_guarded_by_the_absence_of_a_request() {
    let w = working("m", "w");
    let s = select(snapshot(vec![w.clone()]), "m/w");
    let port = FakePort::with(&[&w]);
    let mut exec = Executor::new();
    let (n, _) = exec.begin(&port, s, VoiceText::new("tell it to go on"), Locale::En).unwrap();
    let send = Routed::Proposal(Proposal {
        step: Step::Send { card: "m/w".into(), text: VoiceText::new("go on") },
        origin: Origin::Model,
        provenance: Some(Provenance::Selected),
        off_screen: false,
        level: Level::Confirm,
    });
    let id = exec.routed(n, send).unwrap().hud.awaiting_display().unwrap();
    exec.shown(&port, id);
    // A request appeared after the snapshot froze.
    port.edit("m/w", |g| g.request = Some(("r-new".into(), 1)));
    assert_eq!(exec.confirm(&port, Some(id)).unwrap().hud, error(Refusal::SnapshotChanged));
    assert!(port.enqueued().is_empty());
}

#[tokio::test]
async fn cancel_while_presenting_or_counting_sends_nothing() {
    let a = pending("m", "a");
    let s = select(snapshot(vec![a.clone()]), "m/a");
    let port = FakePort::with(&[&a]);
    let mut exec = Executor::new();
    let id = say(&mut exec, &port, &s, "approve").await.hud.awaiting_display().unwrap();
    assert!(matches!(exec.cancel(&port, None).unwrap().hud, Hud::Done { outcome: OutcomeView::Cancelled, .. }));
    assert_eq!(exec.shown(&port, id), None, "a late ack does not start it");
    say_shown(&mut exec, &port, &s, "approve").await;
    let r = exec.cancel(&port, None).unwrap();
    assert!(matches!(r.hud, Hud::Done { outcome: OutcomeView::Cancelled, .. }));
    assert_eq!(port.cancelled(), ["act-0"]);
    assert_eq!(exec.outcome("act-0", VoiceOutcome::Cancelled), None, "already reported");
}

#[tokio::test]
async fn cancel_after_fire_reports_too_late_then_the_outcome() {
    let a = pending("m", "a");
    let s = select(snapshot(vec![a.clone()]), "m/a");
    let port = FakePort::with(&[&a]);
    let mut exec = Executor::new();
    say_shown(&mut exec, &port, &s, "approve").await;
    port.cancel_works.store(false, Ordering::SeqCst);
    let r = exec.cancel(&port, None).unwrap();
    assert!(matches!(r.hud, Hud::Sending { too_late_to_cancel: true, .. }));
    let done = exec.outcome("act-0", VoiceOutcome::Delivered).unwrap();
    assert!(matches!(done.hud, Hud::Done { outcome: OutcomeView::Delivered, .. }));
}

#[tokio::test]
async fn the_trigger_cancels_a_running_countdown_at_once() {
    let a = pending("m", "a");
    let s = select(snapshot(vec![a.clone()]), "m/a");
    let port = FakePort::with(&[&a]);
    let mut exec = Executor::new();
    say_shown(&mut exec, &port, &s, "approve").await;
    let (n, _) = exec.begin(&port, s.clone(), VoiceText::new("cancel"), Locale::En).unwrap();
    assert_eq!(port.cancelled(), ["act-0"], "cancelled before any routing");
    let routed = route(&s, &VoiceText::new("cancel"), None, &RouteOptions::default()).await;
    let r = exec.routed(n, routed).unwrap();
    assert!(matches!(r.hud, Hud::Done { outcome: OutcomeView::Cancelled, .. }));

    say_shown(&mut exec, &port, &s, "approve").await;
    port.cancel_works.store(false, Ordering::SeqCst);
    let r = say(&mut exec, &port, &s, "cancel").await;
    assert_eq!(r.hud, error(Refusal::AlreadySent));
}

/// Final review B4: while a fired step waits for its result, a new
/// utterance is refused and the step keeps its outcome. A typed "cancel"
/// still says it is too late.
#[tokio::test]
async fn a_new_utterance_while_sending_is_refused_and_the_result_still_shows() {
    let a = pending("m", "a");
    let s = select(snapshot(vec![a.clone()]), "m/a");
    let port = FakePort::with(&[&a]);
    let mut exec = Executor::new();
    say_shown(&mut exec, &port, &s, "approve").await;
    assert!(matches!(exec.outcome("act-0", VoiceOutcome::Fired).unwrap().hud, Hud::Sending { .. }));
    let r = say(&mut exec, &port, &s, "deny").await;
    assert_eq!(r.hud, error(Refusal::StillSending));
    let r = say(&mut exec, &port, &s, "cancel").await;
    assert_eq!(r.hud, error(Refusal::AlreadySent));
    assert_eq!(port.enqueued().len(), 1, "nothing new was enqueued");
    let done = exec.outcome("act-0", VoiceOutcome::Rejected { code: ErrorCode::RequestGone }).unwrap();
    let Hud::Done { proposal: Some(p), outcome: OutcomeView::Rejected { code: ErrorCode::RequestGone } } = done.hud else {
        panic!("expected the rejection, got {:?}", done.hud)
    };
    assert_eq!(p.option.as_deref(), Some("Yes"), "the rejected step is the one that fired");
}

/// A countdown that fired before its `Fired` arrived cannot be cancelled
/// by the next utterance; it is tracked like a sending step.
#[tokio::test]
async fn a_countdown_too_late_to_cancel_keeps_its_outcome() {
    let a = pending("m", "a");
    let s = select(snapshot(vec![a.clone()]), "m/a");
    let port = FakePort::with(&[&a]);
    let mut exec = Executor::new();
    say_shown(&mut exec, &port, &s, "approve").await;
    port.cancel_works.store(false, Ordering::SeqCst);
    let r = say(&mut exec, &port, &s, "deny").await;
    assert_eq!(r.hud, error(Refusal::StillSending));
    assert_eq!(exec.current_action(), Some("act-0"));
    let done = exec.outcome("act-0", VoiceOutcome::Uncertain).unwrap();
    assert!(matches!(done.hud, Hud::Done { outcome: OutcomeView::Uncertain, .. }));
}

#[tokio::test]
async fn cancel_with_nothing_running_says_so() {
    let s = snapshot(vec![pending("m", "a")]);
    let port = FakePort::with(&[]);
    let r = say(&mut Executor::new(), &port, &s, "undo").await;
    assert_eq!(r.hud, error(Refusal::NothingToCancel));
    assert!(port.cancelled().is_empty());
}

#[tokio::test]
async fn which_one_then_a_digit_presents_the_choice() {
    let (a, b) = (pending("m", "a"), pending("m", "b"));
    let s = snapshot(vec![a.clone(), b.clone()]);
    let port = FakePort::with(&[&a, &b]);
    let mut exec = Executor::new();
    let Hud::WhichOne { candidates, .. } = say(&mut exec, &port, &s, "approve").await.hud else {
        panic!("expected which-one")
    };
    assert_eq!(candidates.iter().map(|c| c.card.as_str()).collect::<Vec<_>>(), ["m/a", "m/b"]);
    assert_eq!(exec.choose(None, 5), None);
    let id = exec.choose(None, 1).unwrap().hud.awaiting_display().unwrap();
    assert!(port.enqueued().is_empty());
    exec.shown(&port, id);
    assert_eq!(port.enqueued(), [pick("m/b", "r-b", "allow")]);
}

/// Final review B5: targets show in full however long they are, and
/// targets that would look the same get a disambiguator.
#[tokio::test]
async fn targets_with_shared_prefixes_render_distinguishably() {
    let (prefix, title) = ("m".repeat(300), "t".repeat(300));
    let mut a = pending("m1", "a");
    (a.machine_name, a.title) = (format!("{prefix}-one"), format!("{title}-A"));
    let mut b = pending("m2", "b");
    (b.machine_name, b.title) = (format!("{prefix}-two"), format!("{title}-B"));
    let s = snapshot(vec![a.clone(), b.clone()]);
    let port = FakePort::with(&[&a, &b]);
    let Hud::WhichOne { candidates, .. } = say(&mut Executor::new(), &port, &s, "approve").await.hud else {
        panic!("expected which-one")
    };
    assert_eq!((candidates[0].machine.as_str(), candidates[0].title.as_str()), (a.machine_name.as_str(), a.title.as_str()));
    assert_eq!((candidates[1].machine.as_str(), candidates[1].title.as_str()), (b.machine_name.as_str(), b.title.as_str()));
    assert_eq!((candidates[0].disambiguator.as_deref(), candidates[1].disambiguator.as_deref()), (None, None));

    // The same agent, machine and title: the project tells them apart.
    let mut c = pending("m", "c");
    (c.machine_name, c.title, c.project) = ("box".into(), "build".into(), Some("api".into()));
    let mut d = pending("m", "d");
    (d.machine_name, d.title, d.project) = ("box".into(), "build".into(), Some("web".into()));
    let s = snapshot(vec![c.clone(), d.clone()]);
    let port = FakePort::with(&[&c, &d]);
    let Hud::WhichOne { candidates, .. } = say(&mut Executor::new(), &port, &s, "approve").await.hud else {
        panic!("expected which-one")
    };
    let shown: Vec<_> = candidates.iter().map(|t| t.disambiguator.as_deref()).collect();
    assert_eq!(shown, [Some("api"), Some("web")]);
    // A proposal's single target is told apart from the other card too.
    let Hud::Confirm { proposal, .. } = say(&mut Executor::new(), &port, &select(s, "m/c"), "yes always").await.hud else {
        panic!("expected confirm")
    };
    assert_eq!(proposal.target.disambiguator.as_deref(), Some("api"));
    // The same project as well: the card key.
    d.project = Some("api".into());
    let s = snapshot(vec![c.clone(), d.clone()]);
    let port = FakePort::with(&[&c, &d]);
    let Hud::WhichOne { candidates, .. } = say(&mut Executor::new(), &port, &s, "approve").await.hud else {
        panic!("expected which-one")
    };
    let shown: Vec<_> = candidates.iter().map(|t| t.disambiguator.as_deref()).collect();
    assert_eq!(shown, [Some("m/c"), Some("m/d")]);
}

#[tokio::test]
async fn a_pick_binds_against_the_frozen_snapshot_not_a_newer_one() {
    let (a, b) = (pending("m", "a"), pending("m", "b"));
    let s = snapshot(vec![a.clone(), b.clone()]);
    // The inbox has moved on: m/b's request is at revision 2 now.
    let port = FakePort::with(&[&a, &b]);
    port.edit("m/b", |g| g.request = Some(("r-b".into(), 2)));
    let mut exec = Executor::new();
    let Hud::WhichOne { .. } = say(&mut exec, &port, &s, "approve").await.hud else {
        panic!("expected which-one")
    };
    // The executor only holds the snapshot frozen for this utterance, so
    // the pick binds revision 1.
    let id = exec.choose(None, 1).unwrap().hud.awaiting_display().unwrap();
    assert_eq!(
        exec.shown(&port, id).unwrap().hud,
        error(Refusal::SnapshotChanged),
        "bound to the frozen revision 1, which the live revision 2 refuses"
    );
    assert!(port.enqueued().is_empty());
    // A new utterance with the newer snapshot replaces the choice; the old
    // candidates cannot be picked against it.
    let mut newer_b = b.clone();
    newer_b.pending.as_mut().unwrap().revision = 2;
    let newer = snapshot(vec![a.clone(), newer_b]);
    let mut exec = Executor::new();
    say(&mut exec, &port, &s, "approve").await;
    exec.begin(&port, newer, VoiceText::new("approve"), Locale::En).unwrap();
    assert_eq!(exec.choose(None, 1), None, "the old choice is gone with its snapshot");
}

#[tokio::test]
async fn spoken_replies_answer_the_waiting_hud_and_nothing_else_does() {
    let (a, b) = (pending("m", "a"), pending("m", "b"));
    let port = FakePort::with(&[&a, &b]);
    let mut exec = Executor::new();
    assert_eq!(exec.reply(&port, &VoiceText::new("confirm"), Locale::En), None, "nothing waits");
    let s = select(snapshot(vec![a.clone(), b.clone()]), "m/a");
    let id = say(&mut exec, &port, &s, "yes always").await.hud.awaiting_display().unwrap();
    let r = exec.reply(&port, &VoiceText::new("confirm"), Locale::En).unwrap();
    assert!(matches!(r.hud, Hud::Confirm { .. }), "not shown yet: still waiting");
    exec.shown(&port, id);
    let still = exec.reply(&port, &VoiceText::new("yes"), Locale::En).unwrap();
    assert!(matches!(still.hud, Hud::Confirm { .. }), "yes does not become an answer on another card");
    assert!(port.enqueued().is_empty());
    let r = exec.reply(&port, &VoiceText::new("confirm"), Locale::En).unwrap();
    assert!(matches!(r.hud, Hud::Countdown { action_id: Some(_), .. }));
    assert_eq!(port.enqueued(), [pick("m/a", "r-a", "always")]);

    let s = snapshot(vec![a, b]);
    let mut exec = Executor::new();
    say(&mut exec, &port, &s, "approve").await;
    let r = exec.reply(&port, &VoiceText::new("the ninth"), Locale::En).unwrap();
    assert!(matches!(r.hud, Hud::WhichOne { .. }), "out of range keeps waiting");
    let r = exec.reply(&port, &VoiceText::new("the second one"), Locale::En).unwrap();
    assert!(matches!(r.hud, Hud::Countdown { action_id: None, .. }));
}

#[tokio::test]
async fn a_stale_utterance_is_ignored() {
    let s = select(snapshot(vec![pending("m", "a")]), "m/a");
    let port = FakePort::with(&[]);
    let mut exec = Executor::new();
    let (first, _) = exec.begin(&port, s.clone(), VoiceText::new("approve"), Locale::En).unwrap();
    let (_second, _) = exec.begin(&port, s.clone(), VoiceText::new("deny"), Locale::En).unwrap();
    let routed = route(&s, &VoiceText::new("approve"), None, &RouteOptions::default()).await;
    assert_eq!(exec.routed(first, routed), None);
}

#[tokio::test]
async fn reset_cancels_and_nothing_from_before_matches_after() {
    let a = pending("m", "a");
    let s = select(snapshot(vec![a.clone()]), "m/a");
    let port = FakePort::with(&[&a]);
    let mut exec = Executor::new();
    let old = say(&mut exec, &port, &s, "approve").await.hud.awaiting_display().unwrap();
    exec.shown(&port, old);
    exec.reset(&port);
    assert_eq!(port.cancelled(), ["act-0"], "unsent work is cancelled");
    // An utterance still being routed when voice is turned off.
    let (old_utterance, _) = exec.begin(&port, s.clone(), VoiceText::new("yes always"), Locale::En).unwrap();
    exec.reset(&port);
    let routed = route(&s, &VoiceText::new("yes always"), None, &RouteOptions::default()).await;
    assert_eq!(exec.routed(old_utterance, routed), None, "a response from before the reset");
    let new = say(&mut exec, &port, &s, "yes always").await.hud.awaiting_display().unwrap();
    assert!(new > old, "proposal ids keep growing across a reset");
    assert_eq!(exec.shown(&port, old), None);
    assert_eq!(exec.confirm(&port, Some(old)), None);
}

#[tokio::test]
async fn outcomes_are_reported_and_others_ignored() {
    let a = pending("m", "a");
    let s = select(snapshot(vec![a.clone()]), "m/a");
    let port = FakePort::with(&[&a]);
    for (outcome, view) in [
        (VoiceOutcome::Rejected { code: ErrorCode::RequestGone }, OutcomeView::Rejected { code: ErrorCode::RequestGone }),
        (VoiceOutcome::Uncertain, OutcomeView::Uncertain),
        (VoiceOutcome::NotSent { reason: NotSentReason::Changed }, OutcomeView::NotSent { reason: NotSentReason::Changed }),
    ] {
        let mut exec = Executor::new();
        say_shown(&mut exec, &port, &s, "approve").await;
        let id = exec.current_action().unwrap().to_owned();
        assert_eq!(exec.outcome("someone-else", outcome), None);
        let r = exec.outcome(&id, outcome).unwrap();
        assert!(matches!(r.hud, Hud::Done { outcome: o, .. } if o == view));
    }
}

#[tokio::test]
async fn a_lagged_stream_cancels_unsent_work_and_marks_fired_work_uncertain() {
    let a = pending("m", "a");
    let s = select(snapshot(vec![a.clone()]), "m/a");
    let port = FakePort::with(&[&a]);
    let mut exec = Executor::new();
    say_shown(&mut exec, &port, &s, "approve").await;
    let r = exec.outcomes_lost(&port).unwrap();
    assert!(matches!(r.hud, Hud::Done { outcome: OutcomeView::Cancelled, .. }));
    assert_eq!(port.cancelled(), ["act-0"]);

    say_shown(&mut exec, &port, &s, "approve").await;
    port.cancel_works.store(false, Ordering::SeqCst);
    let r = exec.outcomes_lost(&port).unwrap();
    assert!(matches!(r.hud, Hud::Done { outcome: OutcomeView::Uncertain, .. }));
}

#[tokio::test]
async fn read_summarize_and_select_change_nothing() {
    let a = pending("m", "a");
    let s = select(snapshot(vec![a.clone(), pending("m", "b")]), "m/a");
    let port = FakePort::with(&[&a]);
    let mut exec = Executor::new();
    let r = say(&mut exec, &port, &s, "read it").await;
    assert_eq!(r.hud, Hud::Speaking { text: VoiceText::new("Bash approval. npm test") });
    let r = say(&mut exec, &port, &s, "next").await;
    assert_eq!(r.select.as_deref(), Some("m/b"));
    assert!(port.enqueued().is_empty());
}

#[tokio::test]
async fn enqueue_refusals_are_classified() {
    let a = pending("m", "a");
    let s = select(snapshot(vec![a.clone()]), "m/a");
    let port = FakePort::with(&[&a]);
    *port.refuse.lock().unwrap() = Some(EnqueueError::AlreadyCounting);
    let r = say_shown(&mut Executor::new(), &port, &s, "approve").await;
    assert_eq!(r.hud, error(Refusal::AlreadyCounting));
}

#[test]
fn only_did_not_understand_shows_the_transcript() {
    let port = FakePort::with(&[]);
    let mut exec = Executor::new();
    let (n, _) = exec.begin(&port, snapshot(vec![]), VoiceText::new("frobnicate"), Locale::En).unwrap();
    let r = exec.routed(n, Routed::Refused(Refusal::DidNotUnderstand)).unwrap();
    assert_eq!(r.hud, Hud::Error { refusal: Refusal::DidNotUnderstand, transcript: Some(VoiceText::new("frobnicate")) });
}

#[test]
fn debug_output_never_carries_voice_text() {
    let w = working("m", "w");
    let s = select(snapshot(vec![w.clone()]), "m/w");
    let port = FakePort::with(&[&w]);
    let mut exec = Executor::new();
    let (n, thinking) = exec.begin(&port, s, VoiceText::new("SECRET-T"), Locale::En).unwrap();
    let send = Routed::Proposal(Proposal {
        step: Step::Send { card: "m/w".into(), text: VoiceText::new("SECRET-P") },
        origin: Origin::Model,
        provenance: Some(Provenance::Selected),
        off_screen: false,
        level: Level::Confirm,
    });
    let confirm = exec.routed(n, send).unwrap();
    for shown in [format!("{thinking:?}"), format!("{confirm:?}"), format!("{exec:?}")] {
        assert!(!shown.contains("SECRET"), "{shown}");
    }
    let id = confirm.hud.awaiting_display().unwrap();
    exec.shown(&port, id);
    let countdown = exec.confirm(&port, Some(id)).unwrap();
    assert!(!format!("{countdown:?} {:?}", port.enqueued()).contains("SECRET"));
}

#[test]
fn the_hud_json_shape_is_the_frontend_contract() {
    let target = acd_voice::hud::TargetView {
        card: "m/a".into(),
        machine: "minos".into(),
        agent: "Claude".into(),
        title: "Fix".into(),
        request: Some("bash approval".into()),
        off_screen: false,
        disambiguator: Some("api".into()),
    };
    let hud = Hud::WhichOne { candidates: vec![target], transcript: VoiceText::new("approve"), choice_id: 4 };
    let v = serde_json::to_value(HudEvent::new(7, &hud)).unwrap();
    assert_eq!(v["seq"], 7);
    assert_eq!(v["id"], 4, "the id the HUD sends back");
    assert_eq!(v["hud"]["choice_id"], 4);
    assert_eq!(v["hud"]["state"], "which_one");
    assert_eq!(v["hud"]["candidates"][0]["request"], "bash approval");
    assert_eq!(v["hud"]["candidates"][0]["off_screen"], false);
    assert_eq!(v["hud"]["candidates"][0]["disambiguator"], "api");
    let v = serde_json::to_value(Hud::Done { proposal: None, outcome: OutcomeView::Rejected { code: ErrorCode::RequestGone } }).unwrap();
    assert_eq!(v["outcome"], serde_json::json!({"kind": "rejected", "code": "request_gone"}));
    let v = serde_json::to_value(Hud::Error { refusal: Refusal::DisplayFailed, transcript: None }).unwrap();
    assert_eq!(v, serde_json::json!({"state": "error", "refusal": "display_failed", "transcript": null}));
    assert!(!Hud::Hidden.fades() && !Hud::Hidden.wants_key());
    // Thinking is cancelled from the inbox or the HUD's button; it never
    // takes key, or every typed command would bounce focus.
    assert!(!Hud::Thinking { transcript: VoiceText::new("x"), id: 1 }.wants_key());
    assert!(!Hud::Listening.wants_key() && !Hud::Speaking { text: VoiceText::new("x") }.wants_key());
    assert!(Hud::Speaking { text: VoiceText::new("x") }.fades());
}

#[tokio::test]
async fn a_pick_for_a_replaced_list_is_ignored() {
    let (a, b) = (pending("m", "a"), pending("m", "b"));
    let s = snapshot(vec![a.clone(), b.clone()]);
    let port = FakePort::with(&[&a, &b]);
    let mut exec = Executor::new();
    let Hud::WhichOne { choice_id: old, .. } = say(&mut exec, &port, &s, "approve").await.hud else {
        panic!("expected which-one")
    };
    let Hud::WhichOne { choice_id: new, .. } = say(&mut exec, &port, &s, "approve").await.hud else {
        panic!("expected which-one")
    };
    assert_ne!(old, new);
    assert_eq!(exec.choose(Some(old), 1), None, "a click on the list that was replaced");
    assert!(exec.choose(Some(new), 1).is_some(), "the list on screen works");
}

#[tokio::test]
async fn a_cancel_for_an_older_step_does_not_cancel_a_newer_one() {
    let a = pending("m", "a");
    let s = select(snapshot(vec![a.clone()]), "m/a");
    let port = FakePort::with(&[&a]);
    let mut exec = Executor::new();
    let old = say(&mut exec, &port, &s, "approve").await.hud.awaiting_display().unwrap();
    exec.cancel(&port, None);
    let new = say(&mut exec, &port, &s, "approve").await.hud.awaiting_display().unwrap();
    assert_ne!(old, new);
    assert_eq!(exec.cancel(&port, Some(old)), None, "Esc for the old HUD state");
    exec.shown(&port, new);
    assert_eq!(port.enqueued().len(), 1, "the newer step is still running");
    assert_eq!(exec.cancel(&port, Some(old)), None, "also once it counts");
    let r = exec.cancel(&port, Some(new)).unwrap();
    assert!(matches!(r.hud, Hud::Done { outcome: OutcomeView::Cancelled, .. }));
}

#[tokio::test]
async fn esc_on_the_thinking_hud_names_its_id() {
    let a = pending("m", "a");
    let s = select(snapshot(vec![a.clone()]), "m/a");
    let port = FakePort::with(&[&a]);
    let mut exec = Executor::new();
    let (_, thinking) = exec.begin(&port, s, VoiceText::new("approve"), Locale::En).unwrap();
    let id = thinking.hud.input_id().unwrap();
    assert_eq!(exec.cancel(&port, Some(id + 1)), None);
    assert!(exec.cancel(&port, Some(id)).is_some());
}

/// Esc in the inbox cancels a step that is still thinking, and nothing else.
#[tokio::test]
async fn cancel_thinking_cancels_only_a_thinking_step() {
    let a = pending("m1", "a");
    let s = snapshot(vec![a.clone()]);
    let port = FakePort::with(&[&a]);
    let mut exec = Executor::new();
    assert!(exec.cancel_thinking(&port).is_none(), "nothing is thinking");
    let (n, _) = exec.begin(&port, s.clone(), VoiceText::new("approve"), Locale::En).unwrap();
    let r = exec.cancel_thinking(&port).expect("a thinking step is cancelled");
    assert!(matches!(r.hud, Hud::Done { outcome: OutcomeView::Cancelled, .. }));
    let routed = route(&s, &VoiceText::new("approve"), None, &RouteOptions::default()).await;
    assert!(exec.routed(n, routed).is_none(), "the late result is ignored");
    assert!(port.enqueued().is_empty());

    // A shown proposal is not thinking: Esc in the inbox leaves it alone.
    let r = say(&mut exec, &port, &s, "yes always").await;
    assert!(r.hud.awaiting_display().is_some());
    assert!(exec.cancel_thinking(&port).is_none());
}

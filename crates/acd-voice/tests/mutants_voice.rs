//! Tests added from a cargo-mutants run (docs/research/2026-11-04-mutation-testing.md).
//! Each test names the mutant it kills. The scripted inbox is copied from
//! `executor.rs`, so this file stands alone.
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use acd_voice::fast::Locale;
use acd_voice::hud::{Hud, OutcomeView};
use acd_voice::plan::*;
use acd_voice::refusal::Refusal;
use acd_voice::router::target::Provenance;
use acd_voice::router::{Proposal, RouteOptions, Routed, Step, route};
use acd_voice::safety::{Level, Origin};
use acd_voice::snapshot::fixtures::*;
use acd_voice::snapshot::{CardSnap, Snapshot};
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

fn proposal(step: Step, provenance: Option<Provenance>, level: Level) -> Routed {
    Routed::Proposal(Proposal { step, origin: Origin::Model, provenance, off_screen: false, level })
}

/// Begins an utterance and returns its number.
fn begin(exec: &mut Executor, port: &FakePort, s: &Snapshot, text: &str) -> u64 {
    exec.begin(port, s.clone(), VoiceText::new(text), Locale::En).expect("admitted").0
}

/// plan.rs:211 `delete match arm Some(Reply::Cancel)`: a spoken "cancel" ends
/// a waiting confirm and a waiting which-one.
#[tokio::test]
async fn a_spoken_cancel_ends_the_waiting_step() {
    let (a, b) = (pending("m", "a"), pending("m", "b"));
    let port = FakePort::with(&[&a, &b]);
    let s = select(snapshot(vec![a.clone(), b.clone()]), "m/a");
    let mut exec = Executor::new();
    let id = say(&mut exec, &port, &s, "yes always").await.hud.awaiting_display().unwrap();
    exec.shown(&port, id);
    let r = exec.reply(&port, &VoiceText::new("cancel"), Locale::En).unwrap();
    assert!(matches!(r.hud, Hud::Done { outcome: OutcomeView::Cancelled, .. }), "{:?}", r.hud);
    assert!(port.enqueued().is_empty());

    let mut exec = Executor::new();
    let s = snapshot(vec![a, b]);
    let r = say(&mut exec, &port, &s, "approve").await;
    assert!(matches!(r.hud, Hud::WhichOne { .. }));
    let r = exec.reply(&port, &VoiceText::new("cancel"), Locale::En).unwrap();
    assert!(matches!(r.hud, Hud::Done { outcome: OutcomeView::Cancelled, .. }), "{:?}", r.hud);
}

/// plan.rs:238 `&&` -> `||` (`display_failed`): only the proposal the HUD was
/// asked to show, and only while unacknowledged, fails closed.
#[tokio::test]
async fn display_failed_names_the_unacknowledged_proposal_only() {
    let a = pending("m", "a");
    let s = select(snapshot(vec![a.clone()]), "m/a");
    let port = FakePort::with(&[&a]);
    let mut exec = Executor::new();
    let id = say(&mut exec, &port, &s, "yes always").await.hud.awaiting_display().unwrap();
    assert_eq!(exec.display_failed(id + 1), None, "another proposal's failure is not this one's");
    exec.shown(&port, id);
    assert_eq!(exec.display_failed(id), None, "an acknowledged proposal cannot fail to show");
    let r = exec.confirm(&port, Some(id)).unwrap();
    assert!(matches!(r.hud, Hud::Countdown { action_id: Some(_), .. }), "{:?}", r.hud);
    assert_eq!(port.enqueued().len(), 1);
    // And the unacknowledged one does fail closed.
    let mut exec = Executor::new();
    let id = say(&mut exec, &port, &s, "yes always").await.hud.awaiting_display().unwrap();
    assert_eq!(exec.display_failed(id).unwrap().hud, Hud::Error { refusal: Refusal::DisplayFailed, transcript: None });
}

/// plan.rs:268 `<` -> `<=` (`choose`): a pick past the last candidate is ignored.
#[tokio::test]
async fn choosing_past_the_last_candidate_is_ignored() {
    let (a, b) = (pending("m", "a"), pending("m", "b"));
    let port = FakePort::with(&[&a, &b]);
    let s = snapshot(vec![a, b]);
    let mut exec = Executor::new();
    let Hud::WhichOne { candidates, choice_id, .. } = say(&mut exec, &port, &s, "approve").await.hud else {
        panic!("expected which-one")
    };
    assert_eq!(candidates.len(), 2);
    assert_eq!(exec.choose(Some(choice_id), candidates.len()), None);
    assert!(exec.choose(Some(choice_id), candidates.len() - 1).is_some());
}

/// plan.rs:303 `+=` -> `-=` / `*=` (`listen`) and :426 (`reset`): a routing
/// result from before a press or a reset is never taken for a later utterance.
#[tokio::test]
async fn a_result_from_before_a_press_or_a_reset_is_dropped_for_good() {
    let a = pending("m", "a");
    let s = select(snapshot(vec![a.clone()]), "m/a");
    let port = FakePort::with(&[&a]);
    let routed = route(&s, &VoiceText::new("approve"), None, &RouteOptions::default()).await;
    for how in ["press", "reset"] {
        let mut exec = Executor::new();
        let old = begin(&mut exec, &port, &s, "approve");
        assert_eq!(exec.routing(), Some(old));
        if how == "press" {
            exec.listen(&port);
        } else {
            exec.reset(&port);
        }
        assert_eq!(exec.routing(), None);
        let new = begin(&mut exec, &port, &s, "approve");
        assert_ne!(new, old, "{how}: utterance numbers are never reused");
        assert!(exec.routed(old, routed.clone()).is_none(), "{how}: the old result is stale");
        assert!(exec.routed(new, routed.clone()).is_some());
        assert_eq!(exec.routing(), None, "answered: nothing is awaited");
    }
}

/// plan.rs:489 `delete match arm Step::Summarize` (`immediate`).
#[tokio::test]
async fn summarize_speaks_a_summary() {
    let w = working("m", "w");
    let s = snapshot(vec![w.clone()]);
    let port = FakePort::with(&[&w]);
    let mut exec = Executor::new();
    let n = begin(&mut exec, &port, &s, "summary");
    let r = exec.routed(n, proposal(Step::Summarize { machine: None }, None, Level::None)).unwrap();
    assert!(matches!(r.hud, Hud::Speaking { audio: false, .. }), "{:?}", r.hud);
}

/// plan.rs:562/563 (`operation`): only a named send to a send-only card
/// carries `named_send_only`; no other send or step does.
#[tokio::test]
async fn only_a_named_send_to_a_send_only_card_is_marked() {
    let mut idle = working("m", "i");
    idle.idle_hidden = true;
    let visible = working("m", "v");
    let s = snapshot(vec![idle.clone(), visible.clone()]);
    let port = FakePort::with(&[&idle, &visible]);
    let cases = [
        // (card, provenance, expected flag)
        ("m/i", Some(Provenance::Named), true),
        ("m/i", Some(Provenance::Selected), false),
        ("m/i", None, false),
        ("m/v", Some(Provenance::Named), false),
        ("m/v", Some(Provenance::Selected), false),
    ];
    for (card, provenance, want) in cases {
        let mut exec = Executor::new();
        let n = begin(&mut exec, &port, &s, "tell it hi");
        let send = proposal(Step::Send { card: card.into(), text: VoiceText::new("hi") }, provenance, Level::Confirm);
        let id = exec.routed(n, send).unwrap().hud.awaiting_display().unwrap();
        exec.shown(&port, id);
        exec.confirm(&port, Some(id)).unwrap();
        let Some(Enqueue::Reply { guard, .. }) = port.enqueued().pop() else { panic!("a reply was enqueued") };
        assert_eq!(guard.named_send_only, want, "{card} {provenance:?}");
    }
}

/// plan.rs:572 `==` -> `!=` (`operation`, typed answers): a typed answer is
/// planned against the request it was heard for, and only that one.
#[tokio::test]
async fn a_typed_answer_is_bound_to_the_request_it_was_planned_for() {
    let mut a = pending("m", "a");
    a.pending.as_mut().unwrap().free_text = true;
    let s = select(snapshot(vec![a.clone()]), "m/a");
    let port = FakePort::with(&[&a]);
    let answer = |revision: u64, request: &str| {
        proposal(
            Step::AnswerText { card: "m/a".into(), request_id: request.into(), revision, text: VoiceText::new("go") },
            Some(Provenance::Selected),
            Level::Countdown,
        )
    };
    let frozen = a.pending.as_ref().unwrap();
    let mut exec = Executor::new();
    let n = begin(&mut exec, &port, &s, "type go");
    let id = exec.routed(n, answer(frozen.revision, &frozen.id)).unwrap().hud.awaiting_display().unwrap();
    exec.shown(&port, id).unwrap();
    assert!(matches!(port.enqueued().as_slice(), [Enqueue::AnswerText { .. }]));
    // A different revision or request: nothing is enqueued, the step is dropped.
    for (revision, request) in [(frozen.revision + 1, frozen.id.as_str()), (frozen.revision, "r-other")] {
        let port = FakePort::with(&[&a]);
        let mut exec = Executor::new();
        let n = begin(&mut exec, &port, &s, "type go");
        let id = exec.routed(n, answer(revision, request)).unwrap().hud.awaiting_display().unwrap();
        let r = exec.shown(&port, id).unwrap();
        assert_eq!(r.hud, Hud::Error { refusal: Refusal::SnapshotChanged, transcript: None });
        assert!(port.enqueued().is_empty());
    }
}

/// plan.rs:626 `!=` -> `==` (`target_view`): a card the machine filter hides
/// is shown as off screen, a visible card is not.
#[tokio::test]
async fn off_screen_targets_are_marked_off_screen() {
    let (a, b) = (pending("minos", "a"), pending("studio", "b"));
    let mut s = snapshot(vec![a.clone(), b.clone()]);
    s.presentation.machine_filter = Some("studio".into());
    s.presentation.visible = vec![b.key.clone()];
    let port = FakePort::with(&[&a, &b]);
    for (card, want) in [(&a, true), (&b, false)] {
        let req = card.pending.as_ref().unwrap();
        let step = Step::Answer {
            card: card.key.clone(),
            request_id: req.id.clone(),
            revision: req.revision,
            option_id: "allow".into(),
        };
        let mut exec = Executor::new();
        let n = begin(&mut exec, &port, &s, "approve");
        let r = exec.routed(n, proposal(step, Some(Provenance::Named), Level::Countdown)).unwrap();
        let Hud::Countdown { proposal, .. } = r.hud else { panic!("expected a countdown, got {:?}", r.hud) };
        assert_eq!(proposal.target.off_screen, want, "{}", card.key);
    }
}

// ---- hud.rs ----

fn target() -> acd_voice::hud::TargetView {
    acd_voice::hud::TargetView {
        card: "m/a".into(),
        machine: "SECRET-MACHINE".into(),
        agent: "Claude".into(),
        title: "SECRET-TITLE".into(),
        request: Some("SECRET-REQUEST".into()),
        off_screen: false,
        disambiguator: Some("SECRET-DISAMBIGUATOR".into()),
    }
}

fn proposal_view() -> acd_voice::hud::ProposalView {
    acd_voice::hud::ProposalView {
        action: acd_voice::hud::ProposalAction::Send,
        target: target(),
        option: Some("SECRET-OPTION".into()),
        effect: None,
        rule: Some("SECRET-RULE".into()),
        question: Some("SECRET-QUESTION".into()),
        text: Some(VoiceText::new("SECRET-TEXT")),
        provenance: None,
        level: Level::Confirm,
    }
}

/// hud.rs:59/73 `Debug` for `TargetView` / `ProposalView`: agent and voice text appear only as lengths.
#[test]
fn hud_views_debug_as_lengths() {
    for shown in [format!("{:?}", target()), format!("{:?}", proposal_view())] {
        assert!(!shown.contains("SECRET"), "{shown}");
    }
    let t = format!("{:?}", target());
    assert!(t.starts_with("TargetView") && t.contains("m/a") && t.contains("Claude"), "{t}");
    let p = format!("{:?}", proposal_view());
    assert!(p.starts_with("ProposalView") && p.contains("Send") && p.contains("Confirm"), "{p}");
}

/// hud.rs:132 `wants_key -> false`, :149 `delete match arm Confirm | Countdown` (`input_id`).
#[test]
fn the_hud_takes_key_and_names_its_input_only_when_it_waits() {
    let p = proposal_view();
    let confirm = Hud::Confirm { proposal: p.clone(), proposal_id: 7 };
    let countdown = Hud::Countdown { proposal: p.clone(), proposal_id: 8, action_id: None };
    let which = Hud::WhichOne { candidates: vec![target()], transcript: VoiceText::new("x"), choice_id: 9 };
    for hud in [&confirm, &countdown, &which] {
        assert!(hud.wants_key(), "{hud:?}");
    }
    for hud in [
        Hud::Hidden,
        Hud::Thinking { transcript: VoiceText::new("x"), id: 1 },
        Hud::Sending { proposal: p.clone(), too_late_to_cancel: false },
        Hud::Done { proposal: None, outcome: OutcomeView::Selected },
        Hud::Error { refusal: Refusal::NothingToCancel, transcript: None },
        Hud::Speaking { text: VoiceText::new("x"), audio: true },
    ] {
        assert!(!hud.wants_key(), "{hud:?}");
    }
    assert_eq!(confirm.input_id(), Some(7));
    assert_eq!(countdown.input_id(), Some(8));
    assert_eq!(which.input_id(), Some(9));
    assert_eq!(Hud::Hidden.input_id(), None);
    assert_eq!(Hud::Done { proposal: None, outcome: OutcomeView::Selected }.input_id(), None);
}

// ---- router ----
use acd_voice::refusal::Refusal as R;
use acd_voice::router::model::{FakeIntentModel, IntentModel, ModelError};
use acd_voice::router::target::{self, Purpose, Resolution};

async fn model(s: &Snapshot, said: &str, reply: &str) -> Routed {
    let m = FakeIntentModel::replying(reply);
    route(s, &VoiceText::new(said), Some(&m as &dyn IntentModel), &RouteOptions::default()).await
}

/// validate.rs:17 `64 * 1024` -> `+`: a reply of exactly 64 KiB is allowed.
#[tokio::test]
async fn a_reply_of_exactly_64_kib_is_allowed() {
    let s = select(snapshot(vec![working("m", "w")]), "m/w");
    for (len, ok) in [(64 * 1024, true), (64 * 1024 + 1, false), (2000, true)] {
        let reply = serde_json::json!({"step": "send", "agent": "a1", "text": "x".repeat(len)}).to_string();
        let r = model(&s, "tell it", &reply).await;
        assert_eq!(matches!(r, Routed::Proposal(_)), ok, "{len}: {r:?}");
        if !ok {
            assert_eq!(r, Routed::Refused(R::BadText));
        }
    }
}

/// validate.rs:133 (`aim`, guard `x == y` and `==` -> `!=`): when the model names
/// both an agent and a request, they must be the same card.
#[tokio::test]
async fn an_agent_and_a_request_must_name_the_same_card() {
    let s = snapshot(vec![pending("m", "a"), pending("m", "b")]);
    // a1/r1 are one card, a2/r2 the other.
    let same = model(&s, "do the thing", r#"{"step":"answer","agent":"a1","request":"r1","option":"1"}"#).await;
    let Routed::Proposal(p) = same else { panic!("expected a proposal, got {same:?}") };
    assert_eq!(p.step.card(), Some("m/a"));
    let other = model(&s, "do the thing", r#"{"step":"answer","agent":"a2","request":"r2","option":"1"}"#).await;
    let Routed::Proposal(p) = other else { panic!("expected a proposal, got {other:?}") };
    assert_eq!(p.step.card(), Some("m/b"));
    for crossed in [
        r#"{"step":"answer","agent":"a1","request":"r2","option":"1"}"#,
        r#"{"step":"answer","agent":"a2","request":"r1","option":"1"}"#,
    ] {
        assert_eq!(model(&s, "do the thing", crossed).await, Routed::Refused(R::DidNotUnderstand), "{crossed}");
    }
}

/// validate.rs:120 (`aim`, candidates filter `==` -> `!=`, `&&` -> `||`): a
/// which-one list holds only cards that are on screen and can take the step.
#[tokio::test]
async fn model_candidates_are_limited_to_visible_cards_that_fit() {
    // a: on screen and fits. c: on screen but offline, so it cannot take the
    // step. other: hidden by the machine filter (it still has a prompt id).
    let (a, mut c, other) = (pending("m", "a"), pending("m", "c"), pending("n", "o"));
    c.online = false;
    let mut s = snapshot(vec![a, c, other]);
    s.presentation.machine_filter = Some("m".into());
    s.presentation.visible = vec!["m/a".into(), "m/c".into()];
    let r = model(&s, "do the thing", r#"{"step":"answer","candidates":["a1","a2","a3"],"option":"1"}"#).await;
    match r {
        Routed::WhichOne(choice) => assert_eq!(choice.candidates, ["m/a"]),
        other => panic!("expected which-one, got {other:?}"),
    }
    // Nothing fits: refused rather than an empty list.
    let (mut x, mut y) = (pending("m", "x"), pending("m", "y"));
    x.online = false;
    y.online = false;
    let s = snapshot(vec![x, y]);
    let r = model(&s, "do the thing", r#"{"step":"answer","candidates":["a1","a2"],"option":"1"}"#).await;
    assert_eq!(r, Routed::Refused(R::NoSuchTarget));
}

struct Plain;
#[async_trait::async_trait]
impl IntentModel for Plain {
    async fn plan(&self, _: &acd_voice::router::prompt::Prompt) -> Result<VoiceText, ModelError> {
        Err(ModelError::Failed)
    }
}

/// model.rs:109 `receives_details -> true`: request details go to a model only when it says so.
#[test]
fn a_model_gets_no_request_details_by_default() {
    assert!(!Plain.receives_details());
}

/// model.rs:50 / prompt.rs:71 `Debug` for `ModelPlan` and `Prompt`: model
/// output and the prompt are never printed.
#[test]
fn model_plan_and_prompt_debug_hide_their_content() {
    let plan: acd_voice::router::model::ModelPlan = serde_json::from_str(
        r#"{"step":"send","agent":"SECRET-AGENT","request":"SECRET-REQUEST","text":"SECRET-TEXT","machine":"SECRET-MACHINE","candidates":["SECRET-C"]}"#,
    )
    .unwrap();
    let shown = format!("{plan:?}");
    assert!(!shown.contains("SECRET"), "{shown}");
    assert!(shown.starts_with("ModelPlan") && shown.contains("Send"), "{shown}");
    let s = select(snapshot(vec![pending("m", "a")]), "m/a");
    let (prompt, _ids) = acd_voice::router::prompt::build(
        &s,
        &VoiceText::new("SECRET-TRANSCRIPT"),
        acd_voice::router::prompt::PromptOptions { include_details: true },
    );
    let shown = format!("{prompt:?}");
    assert!(!shown.contains("SECRET") && !shown.contains("npm test"), "{shown}");
    assert!(shown.starts_with("Prompt") && shown.contains("bytes"), "{shown}");
}

// ---- target.rs ----

fn card_with(machine: &str, agent: &str, edit: impl FnOnce(&mut CardSnap)) -> CardSnap {
    let mut c = pending(machine, agent);
    edit(&mut c);
    c
}

/// target.rs:57 match guard `!card.online` (`fits`) and the neighbouring rules.
#[test]
fn fits_refuses_offline_and_unreplyable_cards() {
    let online = working("m", "w");
    assert_eq!(target::fits(&online, Purpose::Send), Ok(()));
    let offline = card_with("m", "w", |c| {
        c.online = false;
        c.can_reply = true;
    });
    assert_eq!(target::fits(&offline, Purpose::Send), Err(R::Offline));
    let mute = card_with("m", "w", |c| c.can_reply = false);
    assert_eq!(target::fits(&mute, Purpose::Send), Err(R::CannotReply));
    assert_eq!(target::fits(&offline, Purpose::Select), Ok(()));
    assert_eq!(target::fits(&offline, Purpose::Answer), Err(R::Offline));
    assert_eq!(target::fits(&working("m", "w"), Purpose::Read), Err(R::NothingToRead));
    assert_eq!(target::fits(&pending("m", "a"), Purpose::Read), Ok(()));
}

/// target.rs:81 match guard (`said_for_names`): the payload is cut only at a word boundary.
#[test]
fn the_payload_is_cut_from_the_names_only_at_a_word_boundary() {
    assert_eq!(target::said_for_names("tell studio to deploy minos", Some("deploy minos")), "tell studio to");
    assert_eq!(target::said_for_names("tell xminos", Some("minos")), "tell xminos");
    assert_eq!(target::said_for_names("minos", Some("minos")), "");
    assert_eq!(target::said_for_names("tell minos hi", None), "tell minos hi");
}

fn kind_card(machine: &str, agent: &str, kind: acd_protocol::AgentKind, project: Option<&str>) -> CardSnap {
    card_with(machine, agent, |c| {
        c.kind = kind;
        c.project = project.map(str::to_owned);
        c.title = format!("job {agent}");
    })
}

/// target.rs:110 `delete match arm "codex"` (`kinds`).
#[test]
fn the_word_codex_names_codex_cards() {
    let s = snapshot(vec![
        kind_card("m", "a", acd_protocol::AgentKind::Claude, None),
        kind_card("m", "b", acd_protocol::AgentKind::Codex, None),
    ]);
    assert_eq!(target::named(&s, "approve codex"), Some(vec!["m/b".to_string()]));
    assert_eq!(target::named(&s, "approve claude"), Some(vec!["m/a".to_string()]));
}

/// target.rs:175 `>` -> `==` / `<` (`named`): several cards of the same kind
/// are narrowed by the project the transcript names.
#[test]
fn a_named_project_narrows_cards_of_the_same_kind() {
    let s = snapshot(vec![
        kind_card("m", "a", acd_protocol::AgentKind::Claude, Some("billing")),
        kind_card("m", "b", acd_protocol::AgentKind::Claude, Some("search")),
    ]);
    assert_eq!(target::named(&s, "approve claude billing"), Some(vec!["m/a".to_string()]));
    assert_eq!(target::named(&s, "approve claude search"), Some(vec!["m/b".to_string()]));
    let both = target::named(&s, "approve claude").unwrap();
    assert_eq!(both, ["m/a", "m/b"]);
}

/// target.rs:207 match guard `purpose == Read` (`resolve`): with nothing pending,
/// a read has nothing to read and an answer has nothing to answer.
#[test]
fn with_nothing_pending_the_refusal_names_the_purpose() {
    let s = snapshot(vec![working("m", "w")]);
    assert_eq!(target::resolve(&s, "do it", Purpose::Read, None), Resolution::Refused(R::NothingToRead));
    assert_eq!(target::resolve(&s, "do it", Purpose::Answer, None), Resolution::Refused(R::NothingToAnswer));
    assert_eq!(target::resolve(&s, "do it", Purpose::AnswerText, None), Resolution::Refused(R::NothingToAnswer));
}

// ---- snapshot.rs ----

/// snapshot.rs:108/120/136 `Debug` for `OptionSnap`, `RequestSnap`, `CardSnap`.
#[test]
fn card_debug_hides_agent_text() {
    let mut c = pending("m", "a");
    c.machine_name = "SECRET-MACHINE".into();
    c.title = "SECRET-TITLE".into();
    c.project = Some("SECRET-PROJECT".into());
    {
        let r = c.pending.as_mut().unwrap();
        r.title = "SECRET-RTITLE".into();
        r.detail = "SECRET-DETAIL".into();
    }
    let shown = format!("{c:?}");
    assert!(!shown.contains("SECRET"), "{shown}");
    assert!(shown.starts_with("CardSnap") && shown.contains("RequestSnap") && shown.contains("OptionSnap"), "{shown}");
    assert!(shown.contains("r-a") && shown.contains("allow"), "{shown}");
}

/// snapshot.rs:220 match guard `*machine != card.machine_key` (`visibility`): only a
/// card of another machine is hidden by the machine filter; one of the filtered
/// machine that the panel did not publish is simply not shown.
#[test]
fn the_machine_filter_hides_only_other_machines() {
    let (a, b) = (pending("minos", "a"), pending("studio", "b"));
    let mut s = snapshot(vec![a.clone(), b.clone()]);
    s.presentation.machine_filter = Some("studio".into());
    s.presentation.visible = vec![];
    assert_eq!(s.visibility(&a), acd_voice::snapshot::Visibility::FilterHidden);
    assert_eq!(s.visibility(&b), acd_voice::snapshot::Visibility::Never(acd_voice::snapshot::Hidden::NotShown));
}

// ---- listen.rs ----

/// listen.rs:231 `Recorder::limits -> Default::default()`.
#[test]
fn a_recorder_reports_the_limits_it_was_given() {
    use acd_voice::listen::{Limits, Recorder};
    let limits = Limits { max: std::time::Duration::from_secs(3), final_timeout: std::time::Duration::from_millis(7) };
    assert_eq!(Recorder::new(limits).limits(), limits);
    assert_ne!(limits, Limits::default());
}

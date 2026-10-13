mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use acd_client::inbox::OUTCOME_CHANNEL_CAPACITY;
use acd_protocol::ErrorCode;
use acd_voice::hud::{Hud, OutcomeView};
use acd_voice::refusal::Refusal;
use acd_voice::router::model::{FakeIntentModel, IntentModel, ModelError};
use acd_voice::router::prompt::Prompt;
use acd_voice::text::VoiceText;
use acd_voice_client::{DISPLAY_TIMEOUT, InputError};
use common::*;
use serde_json::json;

fn countdown_started(h: &Hud) -> bool {
    matches!(h, Hud::Countdown { action_id: Some(_), .. })
}

#[tokio::test]
async fn approve_counts_down_after_the_hud_showed_it_then_reports_delivery() {
    let r = rig(vec![pending_agent()], None, Display::Acks).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.text_input("approve".into(), true).await.unwrap();
    hud_until(&r.sink, "countdown", countdown_started).await;
    let sent = frame(&r.f, "request.answer").await;
    assert_eq!((sent["request_id"].as_str(), sent["option_id"].as_str()), (Some("r1"), Some("allow")));
    r.c.send(json!({"type":"result","id":sent["id"],"ok":true,"outcome":"delivered"})).await;
    hud_until(&r.sink, "delivered", |h| matches!(h, Hud::Done { outcome: OutcomeView::Delivered, .. })).await;
    let huds = r.sink.huds.lock().unwrap().clone();
    assert!(huds.windows(2).all(|w| w[0].0 < w[1].0), "published in order");
    let shown_at = huds.iter().position(|(_, h)| matches!(h, Hud::Countdown { action_id: None, .. })).unwrap();
    let started_at = huds.iter().position(|(_, h)| countdown_started(h)).unwrap();
    assert!(shown_at < started_at, "the step was on screen before its countdown started");
}

#[tokio::test]
async fn a_hud_that_never_answers_sends_nothing() {
    let r = rig(vec![pending_agent()], None, Display::Silent).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.text_input("approve".into(), true).await.unwrap();
    tokio::time::sleep(DISPLAY_TIMEOUT + Duration::from_millis(300)).await;
    assert_eq!(r.sink.last(), Some(Hud::Error { refusal: Refusal::DisplayFailed, transcript: None }));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(frames(&r.f, "request.answer").is_empty());
}

#[tokio::test]
async fn a_refused_display_fails_closed_at_once() {
    let r = rig(vec![pending_agent()], None, Display::Acks).await;
    r.sink.refuse_display.store(true, Ordering::SeqCst);
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.text_input("approve".into(), true).await.unwrap();
    assert_eq!(r.sink.last(), Some(Hud::Error { refusal: Refusal::DisplayFailed, transcript: None }));
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(frames(&r.f, "request.answer").is_empty());
}

#[tokio::test]
async fn a_revision_bump_before_confirm_sends_nothing() {
    let r = rig(vec![pending_agent()], None, Display::Acks).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.text_input("yes always".into(), true).await.unwrap();
    let Hud::Confirm { proposal_id, .. } = hud_until(&r.sink, "confirm", |h| matches!(h, Hud::Confirm { .. })).await else {
        unreachable!()
    };
    let mut agent = pending_agent();
    agent["pending"]["revision"] = json!(2);
    r.c.send(json!({"type":"agent.updated","epoch":"e1","seq":2,"agent":agent})).await;
    view_until(&r.inbox, "revision 2", |v| {
        v.cards.iter().any(|c| c.agent.pending.as_ref().is_some_and(|p| p.revision == 2))
    })
    .await;
    r.voice.confirm(proposal_id);
    hud_until(&r.sink, "dropped", |h| *h == Hud::Error { refusal: Refusal::SnapshotChanged, transcript: None }).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(frames(&r.f, "request.answer").is_empty());
}

#[tokio::test]
async fn typing_confirm_confirms_the_waiting_step_and_yes_does_not() {
    let r = rig(vec![pending_agent()], None, Display::Acks).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.text_input("yes always".into(), true).await.unwrap();
    hud_until(&r.sink, "confirm", |h| matches!(h, Hud::Confirm { .. })).await;
    tokio::time::sleep(Duration::from_millis(50)).await; // let the ack land
    r.voice.text_input("yes".into(), true).await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(frames(&r.f, "request.answer").is_empty(), "yes did not answer anything");
    r.voice.text_input("confirm".into(), true).await.unwrap();
    let sent = frame(&r.f, "request.answer").await;
    assert_eq!(sent["option_id"].as_str(), Some("always"));
}

#[tokio::test]
async fn hud_cancel_during_the_countdown_sends_nothing() {
    let r = rig(vec![pending_agent()], None, Display::Acks).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.text_input("approve".into(), true).await.unwrap();
    hud_until(&r.sink, "countdown", countdown_started).await;
    r.voice.cancel(None);
    hud_until(&r.sink, "cancelled", |h| matches!(h, Hud::Done { outcome: OutcomeView::Cancelled, .. })).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(frames(&r.f, "request.answer").is_empty());
}

/// Saving settings unchanged must not cancel a running countdown.
#[tokio::test]
async fn a_redundant_set_enabled_leaves_a_countdown_running() {
    let r = rig(vec![pending_agent()], None, Display::Acks).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.text_input("approve".into(), true).await.unwrap();
    hud_until(&r.sink, "countdown", countdown_started).await;
    r.voice.set_enabled(true);
    let sent = frame(&r.f, "request.answer").await;
    assert_eq!(sent["option_id"].as_str(), Some("allow"));
    assert!(!r.sink.huds.lock().unwrap().iter().any(|(_, h)| *h == Hud::Hidden), "the HUD was not reset");
}

#[tokio::test]
async fn a_second_command_cancels_the_running_countdown_at_once() {
    let r = rig(vec![pending_agent()], None, Display::Acks).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.text_input("approve".into(), true).await.unwrap();
    hud_until(&r.sink, "countdown", countdown_started).await;
    r.voice.text_input("cancel".into(), true).await.unwrap();
    hud_until(&r.sink, "cancelled", |h| matches!(h, Hud::Done { outcome: OutcomeView::Cancelled, .. })).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(frames(&r.f, "request.answer").is_empty());
}

/// Final review B4 through the real inbox: a command typed while the
/// last step waits for its result is refused; the late rejection shows.
#[tokio::test]
async fn a_command_while_sending_is_refused_and_the_late_rejection_shows() {
    let r = rig(vec![pending_agent()], None, Display::Acks).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.text_input("approve".into(), true).await.unwrap();
    let sent = frame(&r.f, "request.answer").await;
    hud_until(&r.sink, "sending", |h| matches!(h, Hud::Sending { .. })).await;
    r.voice.text_input("deny".into(), true).await.unwrap();
    assert_eq!(r.sink.last(), Some(Hud::Error { refusal: Refusal::StillSending, transcript: None }));
    r.c.send(json!({"type":"result","id":sent["id"],"ok":false,"error":{"code":"request_gone","message":"gone"}})).await;
    hud_until(&r.sink, "rejected", |h| {
        matches!(h, Hud::Done { outcome: OutcomeView::Rejected { code: ErrorCode::RequestGone }, .. })
    })
    .await;
    assert_eq!(frames(&r.f, "request.answer").len(), 1, "the refused command sent nothing");
}

#[tokio::test]
async fn esc_in_the_panel_cancels_the_voice_action_too() {
    let r = rig(vec![pending_agent()], None, Display::Acks).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.text_input("approve".into(), true).await.unwrap();
    hud_until(&r.sink, "countdown", countdown_started).await;
    assert!(r.inbox.escape());
    hud_until(&r.sink, "cancelled", |h| matches!(h, Hud::Done { outcome: OutcomeView::Cancelled, .. })).await;
}

#[tokio::test]
async fn a_rejection_is_reported_with_its_code() {
    let r = rig(vec![pending_agent()], None, Display::Acks).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.text_input("approve".into(), true).await.unwrap();
    let sent = frame(&r.f, "request.answer").await;
    r.c.send(json!({"type":"result","id":sent["id"],"ok":false,"error":{"code":"request_gone","message":"gone"}})).await;
    hud_until(&r.sink, "rejected", |h| {
        matches!(h, Hud::Done { outcome: OutcomeView::Rejected { code: ErrorCode::RequestGone }, .. })
    })
    .await;
}

async fn confirm_shown(r: &Rig) {
    let Hud::Confirm { proposal_id, .. } = hud_until(&r.sink, "confirm", |h| matches!(h, Hud::Confirm { .. })).await else {
        unreachable!()
    };
    tokio::time::sleep(Duration::from_millis(50)).await; // let the ack land
    r.voice.confirm(proposal_id);
}

#[tokio::test]
async fn a_confirmed_send_reaches_the_agent() {
    let model: Arc<dyn IntentModel> =
        Arc::new(FakeIntentModel::replying(r#"{"step":"send","agent":"a1","text":"run the tests"}"#));
    let r = rig(vec![working_agent()], Some(model), Display::Acks).await;
    r.voice.publish(shown(None, &["m1/a2"]));
    r.voice.text_input("tell the box claude to run the tests".into(), false).await.unwrap();
    assert!(frames(&r.f, "agent.send").is_empty(), "nothing before confirm");
    confirm_shown(&r).await;
    let sent = frame(&r.f, "agent.send").await;
    assert_eq!((sent["agent_id"].as_str(), sent["text"].as_str()), (Some("a2"), Some("run the tests")));
}

/// D7: a named send reaches an idle agent the panel hides, confirmed first.
#[tokio::test]
async fn a_named_send_reaches_a_hidden_idle_agent() {
    // The prompt lists the visible a1 first, then the hidden idle a3 as "a2".
    let model: Arc<dyn IntentModel> =
        Arc::new(FakeIntentModel::replying(r#"{"step":"send","agent":"a2","text":"rebase"}"#));
    let r = rig(vec![pending_agent(), idle_agent()], Some(model), Display::Acks).await;
    r.voice.publish(shown(None, &["m1/a1"]));
    r.voice.text_input("tell the codex to rebase".into(), false).await.unwrap();
    let Hud::Confirm { proposal, .. } = hud_until(&r.sink, "confirm", |h| matches!(h, Hud::Confirm { .. })).await else {
        unreachable!()
    };
    assert!(proposal.target.off_screen);
    confirm_shown(&r).await;
    let sent = frame(&r.f, "agent.send").await;
    assert_eq!((sent["agent_id"].as_str(), sent["text"].as_str()), (Some("a3"), Some("rebase")));
}

/// Final review B1: the step was planned on a visible subagent; the user
/// hid subagents while Confirm waited. Confirm sends nothing.
#[tokio::test]
async fn a_subagent_hidden_while_confirm_waits_is_never_answered() {
    let mut sub = pending_agent();
    sub["role"] = "subagent".into();
    let r = rig(vec![], None, Display::Acks).await;
    r.inbox.set_show_subagents(true).unwrap();
    r.c.send(json!({"type":"snapshot","epoch":"e1","seq":2,"agents":[sub]})).await;
    view_until(&r.inbox, "subagent shown", |v| v.cards.len() == 1).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.text_input("yes always".into(), true).await.unwrap();
    let Hud::Confirm { proposal_id, .. } = hud_until(&r.sink, "confirm", |h| matches!(h, Hud::Confirm { .. })).await else {
        unreachable!()
    };
    tokio::time::sleep(Duration::from_millis(50)).await; // let the ack land
    // Keep the published card, so only the live check can refuse.
    r.inbox.hold_rebuild_for_test(true);
    r.inbox.set_show_subagents(false).unwrap();
    r.voice.confirm(proposal_id);
    hud_until(&r.sink, "refused", |h| *h == Hud::Error { refusal: Refusal::Hidden, transcript: None }).await;
    r.inbox.hold_rebuild_for_test(false);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(frames(&r.f, "request.answer").is_empty(), "no daemon operation");
}

/// A model that waits until released, and notes when its call is dropped.
struct Gated {
    gate: Arc<tokio::sync::Notify>,
    dropped: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl IntentModel for Gated {
    async fn plan(&self, _prompt: &Prompt) -> Result<VoiceText, ModelError> {
        struct OnDrop(Arc<AtomicBool>);
        impl Drop for OnDrop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let armed = OnDrop(self.dropped.clone());
        self.gate.notified().await;
        std::mem::forget(armed);
        Ok(VoiceText::new(r#"{"step":"send","agent":"a1","text":"old"}"#))
    }
}

/// B4: disable, enable, a new command; the old model call was cancelled
/// and its answer never lands.
#[tokio::test]
async fn turning_voice_off_and_on_drops_work_from_before() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let model: Arc<dyn IntentModel> = Arc::new(Gated { gate: gate.clone(), dropped: dropped.clone() });
    let r = rig(vec![pending_agent(), working_agent()], Some(model), Display::Acks).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1", "m1/a2"]));
    let voice = r.voice.clone();
    let old = tokio::spawn(async move { voice.text_input("tell the box claude something".into(), true).await });
    hud_until(&r.sink, "thinking", |h| matches!(h, Hud::Thinking { .. })).await;
    r.voice.set_enabled(false);
    assert_eq!(r.voice.text_input("approve".into(), true).await, Err(InputError::Off));
    r.voice.set_enabled(true);
    assert_eq!(old.await.unwrap(), Ok(()));
    assert!(dropped.load(Ordering::SeqCst), "the model call was cancelled");
    r.voice.text_input("approve".into(), true).await.unwrap();
    hud_until(&r.sink, "countdown", countdown_started).await;
    gate.notify_waiters();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!r.sink.huds.lock().unwrap().iter().any(|(_, h)| matches!(h, Hud::Confirm { .. })), "the old answer never lands");
    assert!(frames(&r.f, "agent.send").is_empty());
}

/// A model that only counts its calls.
struct Counting(Arc<std::sync::atomic::AtomicUsize>);

#[async_trait::async_trait]
impl IntentModel for Counting {
    async fn plan(&self, _prompt: &Prompt) -> Result<VoiceText, ModelError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(VoiceText::new(r#"{"step":"none"}"#))
    }
}

/// B4, the interleaving itself: the input has arrived, voice turns off
/// before admission. Admission and reset share one lock, so the input is
/// refused and no work starts while voice is off. (The other order, a
/// reset after admission, is `turning_voice_off_and_on_drops_work_from_before`.)
#[tokio::test]
async fn a_reset_racing_admission_never_starts_work_while_disabled() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let model: Arc<dyn IntentModel> = Arc::new(Counting(calls.clone()));
    let r = rig(vec![pending_agent()], Some(model), Display::Acks).await;
    let weak = Arc::downgrade(&r.voice);
    r.voice.on_admission_for_test(move || {
        if let Some(voice) = weak.upgrade() {
            voice.set_enabled(false);
        }
    });
    assert_eq!(r.voice.text_input("tell the box claude to go on".into(), true).await, Err(InputError::Off));
    assert_eq!(calls.load(Ordering::SeqCst), 0, "no model work started");
    assert!(
        !r.sink.huds.lock().unwrap().iter().any(|(_, h)| matches!(h, Hud::Thinking { .. })),
        "nothing was admitted"
    );
    assert_eq!(r.sink.last(), Some(Hud::Hidden));
}

#[tokio::test]
async fn turning_voice_off_cancels_and_hides() {
    let r = rig(vec![pending_agent()], None, Display::Acks).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.text_input("approve".into(), true).await.unwrap();
    hud_until(&r.sink, "countdown", countdown_started).await;
    r.voice.set_enabled(false);
    assert_eq!(r.sink.last(), Some(Hud::Hidden));
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(frames(&r.f, "request.answer").is_empty());
}

#[tokio::test]
async fn stale_presentations_lose_and_an_empty_command_is_refused() {
    let r = rig(vec![pending_agent()], None, Display::Acks).await;
    let mut newer = shown(Some("m1/a1"), &["m1/a1"]);
    newer.version = 2;
    assert!(r.voice.publish(newer));
    assert!(!r.voice.publish(shown(None, &[])));
    assert_eq!(r.voice.text_input("  ".into(), true).await, Err(InputError::Empty));
}

/// Task 9 review: when the outcome stream lags, a countdown that has not
/// fired is cancelled, not left to send while the controller no longer
/// knows what happened to it. The flood runs inside the sink, on the
/// current-thread runtime, so the forwarder cannot drain it meanwhile.
#[tokio::test]
async fn a_lagged_outcome_stream_cancels_the_unfired_voice_action() {
    let mut other = pending_agent();
    other["id"] = "a4".into();
    other["pending"]["id"] = "r4".into();
    other["pending"]["agent_id"] = "a4".into();
    let r = rig(vec![pending_agent(), other], None, Display::Acks).await;
    let inbox = r.inbox.clone();
    let flooded = AtomicBool::new(false);
    *r.sink.on_hud.lock().unwrap() = Some(Box::new(move |h| {
        if countdown_started(h) && !flooded.swap(true, Ordering::SeqCst) {
            for _ in 0..OUTCOME_CHANNEL_CAPACITY + 50 {
                let id = inbox.pick("m1/a4", "r4", "deny", 1).unwrap();
                assert!(inbox.cancel(&id));
            }
        }
    }));
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1", "m1/a4"]));
    r.voice.text_input("approve".into(), true).await.unwrap();
    hud_until(&r.sink, "cancelled", |h| matches!(h, Hud::Done { outcome: OutcomeView::Cancelled, .. })).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(frames(&r.f, "request.answer").is_empty(), "the unfired action never sent");
}

/// Review I1: a change of the switch starts a new session, which drops the
/// routing in flight; the HUD must not stay on Thinking. (Re-saving the same
/// value changes nothing: `a_redundant_set_enabled_leaves_a_countdown_running`.)
#[tokio::test]
async fn disabling_voice_does_not_strand_the_hud_on_thinking() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let model: Arc<dyn IntentModel> = Arc::new(Gated { gate: gate.clone(), dropped });
    let r = rig(vec![pending_agent()], Some(model), Display::Acks).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    let voice = r.voice.clone();
    let old = tokio::spawn(async move { voice.text_input("tell the box claude something".into(), true).await });
    hud_until(&r.sink, "thinking", |h| matches!(h, Hud::Thinking { .. })).await;
    r.voice.set_enabled(false);
    assert_eq!(old.await.unwrap(), Ok(()));
    assert_eq!(r.sink.last(), Some(Hud::Hidden));
    gate.notify_waiters();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(r.sink.last(), Some(Hud::Hidden), "the dropped answer never lands");
}

/// Review I2: Esc and clicks carry the id of the state the HUD showed.
#[tokio::test]
async fn a_cancel_for_an_older_hud_state_does_not_cancel_the_newer_step() {
    let r = rig(vec![pending_agent()], None, Display::Acks).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.text_input("yes always".into(), true).await.unwrap();
    let Hud::Confirm { proposal_id: old, .. } = hud_until(&r.sink, "confirm", |h| matches!(h, Hud::Confirm { .. })).await else {
        unreachable!()
    };
    r.voice.cancel(Some(old));
    hud_until(&r.sink, "cancelled", |h| matches!(h, Hud::Done { outcome: OutcomeView::Cancelled, .. })).await;
    r.voice.text_input("yes always".into(), true).await.unwrap();
    let Hud::Confirm { proposal_id: new, .. } = hud_until(&r.sink, "confirm", |h| matches!(h, Hud::Confirm { .. })).await else {
        unreachable!()
    };
    assert_ne!(old, new);
    tokio::time::sleep(Duration::from_millis(50)).await;
    r.voice.cancel(Some(old));
    assert!(matches!(r.sink.last(), Some(Hud::Confirm { .. })), "the stale Esc was ignored");
    r.voice.confirm(new);
    frame(&r.f, "request.answer").await;
}

/// Esc in the inbox panel cancels a step that is still thinking; its model
/// answer is ignored when it lands. With nothing thinking it reports false,
/// so the panel goes on to its own Esc handling.
#[tokio::test]
async fn esc_in_the_inbox_cancels_a_thinking_step() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let model: Arc<dyn IntentModel> = Arc::new(Gated { gate: gate.clone(), dropped });
    let r = rig(vec![pending_agent()], Some(model), Display::Acks).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    assert!(!r.voice.cancel_thinking(), "nothing is thinking");
    let voice = r.voice.clone();
    let utterance = tokio::spawn(async move { voice.text_input("tell the box claude something".into(), true).await });
    hud_until(&r.sink, "thinking", |h| matches!(h, Hud::Thinking { .. })).await;
    assert!(r.voice.cancel_thinking());
    hud_until(&r.sink, "cancelled", |h| matches!(h, Hud::Done { outcome: OutcomeView::Cancelled, .. })).await;
    gate.notify_waiters();
    assert_eq!(utterance.await.unwrap(), Ok(()));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(matches!(r.sink.last(), Some(Hud::Done { outcome: OutcomeView::Cancelled, .. })), "the late answer never lands");
    assert!(frames(&r.f, "request.answer").is_empty());
}

fn gated() -> (Arc<dyn IntentModel>, Arc<tokio::sync::Notify>, Arc<AtomicBool>) {
    let gate = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    (Arc::new(Gated { gate: gate.clone(), dropped: dropped.clone() }), gate, dropped)
}

/// Bounded, so a missing fix fails the test instead of hanging until the job times out.
async fn ends_within_5s(task: tokio::task::JoinHandle<Result<(), InputError>>) -> Result<(), InputError> {
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("the utterance task ended: its model call was dropped")
        .unwrap()
}

/// Review Focus 1, plan V9. These tests rely on `#[tokio::test]`'s single
/// thread: the first poll of the utterance task's `select!` starts the route
/// (and so the model call) before the test task sees `Thinking`.
#[tokio::test]
async fn cancelling_a_thinking_step_cancels_the_model_call() {
    let (model, _gate, dropped) = gated();
    let r = rig(vec![pending_agent(), working_agent()], Some(model), Display::Acks).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1", "m1/a2"]));
    let voice = r.voice.clone();
    let old = tokio::spawn(async move { voice.text_input("tell the box claude something".into(), true).await });
    let Hud::Thinking { id, .. } = hud_until(&r.sink, "thinking", |h| matches!(h, Hud::Thinking { .. })).await else { unreachable!() };
    r.voice.cancel(Some(id));
    assert_eq!(ends_within_5s(old).await, Ok(()), "the utterance task ends without waiting for the model");
    assert!(dropped.load(Ordering::SeqCst), "the model call was dropped");
}

#[tokio::test]
async fn esc_in_the_inbox_while_thinking_cancels_the_model_call() {
    let (model, _gate, dropped) = gated();
    let r = rig(vec![pending_agent(), working_agent()], Some(model), Display::Acks).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1", "m1/a2"]));
    let voice = r.voice.clone();
    let old = tokio::spawn(async move { voice.text_input("tell the box claude something".into(), true).await });
    hud_until(&r.sink, "thinking", |h| matches!(h, Hud::Thinking { .. })).await;
    assert!(r.voice.cancel_thinking());
    assert_eq!(ends_within_5s(old).await, Ok(()));
    assert!(dropped.load(Ordering::SeqCst));
}

/// Spec §5.4, plan V8.
#[tokio::test]
async fn an_on_device_model_gets_request_details_and_others_do_not() {
    for (local, expected) in [(true, true), (false, false)] {
        let fake = FakeIntentModel::replying(r#"{"step":"none"}"#);
        let fake = Arc::new(if local { fake.on_device() } else { fake });
        let model: Arc<dyn IntentModel> = fake.clone();
        let r = rig(vec![pending_agent()], Some(model), Display::Acks).await;
        r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
        r.voice.text_input("what does it want".into(), true).await.unwrap();
        assert_eq!(fake.prompts()[0].contains("\"detail\":\"npm test\""), expected, "local={local}");
    }
}

/// Review Focus 2, spec §9, plan V7.
#[tokio::test]
async fn an_unavailable_model_names_the_reason_and_the_fast_path_still_works() {
    use acd_voice::router::model::ModelUnavailable;
    let model: Arc<dyn IntentModel> = Arc::new(FakeIntentModel::failing(ModelError::Unavailable(ModelUnavailable::IntelligenceOff)));
    let r = rig(vec![pending_agent()], Some(model), Display::Acks).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.text_input("tell it to go on".into(), true).await.unwrap();
    hud_until(&r.sink, "refused", |h| *h == Hud::Error { refusal: Refusal::ModelOff, transcript: None }).await;
    r.voice.text_input("approve".into(), true).await.unwrap();
    hud_until(&r.sink, "countdown", countdown_started).await;
}

/// Spec §6.3: voice off, lock and overflow discard model sessions.
#[tokio::test]
async fn voice_off_lock_and_overflow_discard_the_model() {
    let fake = Arc::new(FakeIntentModel::replying(r#"{"step":"none"}"#));
    let model: Arc<dyn IntentModel> = fake.clone();
    let r = rig(vec![pending_agent()], Some(model), Display::Acks).await;
    r.voice.set_enabled(false);
    r.voice.set_enabled(true);
    r.voice.system_stop();
    r.voice.overflow();
    assert_eq!(fake.discards(), 4, "off, on, lock and overflow each invalidate");
}

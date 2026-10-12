mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use acd_voice::hud::{Hud, OutcomeView, ProposalView};
use acd_voice::listen::fake::Call;
use acd_voice::listen::{Capabilities, Limits, ListenError, ListenEvent, Unavailable};
use acd_voice::refusal::Refusal;
use acd_voice::router::model::{IntentModel, ModelError};
use acd_voice::router::prompt::Prompt;
use acd_voice::snapshot::Presentation;
use acd_voice::text::VoiceText;
use acd_client::voice::ActionOutcome;
use acd_voice_client::Controller;
use common::*;
use serde_json::json;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn countdown_started(h: &Hud) -> bool {
    matches!(h, Hud::Countdown { action_id: Some(_), .. })
}

fn first_countdown(s: &Sink) -> ProposalView {
    s.huds
        .lock()
        .unwrap()
        .iter()
        .find_map(|(_, h)| match h {
            Hud::Countdown { proposal, .. } => Some(proposal.clone()),
            _ => None,
        })
        .expect("a countdown")
}

async fn until(what: &str, pred: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !pred() {
            tokio::time::sleep(ms(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

/// Runs `f` once, at the next admission, with no lock held: the barrier
/// between "the transcript left the recorder" and "admission" (S23).
fn at_next_admission(voice: &Arc<Controller>, f: impl Fn(&Controller) + Send + Sync + 'static) -> Arc<AtomicBool> {
    let fired = Arc::new(AtomicBool::new(false));
    let (flag, weak) = (fired.clone(), Arc::downgrade(voice));
    voice.on_admission_for_test(move || {
        if !flag.swap(true, Ordering::SeqCst)
            && let Some(v) = weak.upgrade()
        {
            f(&v);
        }
    });
    fired
}

async fn confirm_on_screen(r: &Rig) -> u64 {
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.text_input("yes always".into(), true).await.unwrap();
    let Hud::Confirm { proposal_id, .. } = hud_until(&r.sink, "confirm", |h| matches!(h, Hud::Confirm { .. })).await else {
        unreachable!()
    };
    acked_until(&r.sink, proposal_id).await;
    proposal_id
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

/// Plan S15: speech takes the typed path; the same proposal and frame.
#[tokio::test]
async fn a_spoken_approve_counts_down_like_a_typed_one() {
    let typed = rig(vec![pending_agent()], None, Display::Acks).await;
    typed.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    typed.voice.text_input("approve".into(), true).await.unwrap();
    hud_until(&typed.sink, "typed countdown", countdown_started).await;

    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.hold(true, true);
    let g = speech.last_start().unwrap();
    assert_eq!(r.sink.last(), Some(Hud::Listening { id: g, partial: None, over: None }));
    r.voice.hold(false, true);
    assert_eq!(speech.calls(), [Call::Start(g), Call::Stop(g)]);
    r.voice.speech_event(final_(g, "approve"));
    hud_until(&r.sink, "spoken countdown", countdown_started).await;

    assert_eq!(first_countdown(&typed.sink), first_countdown(&r.sink));
    let sent = frame(&r.f, "request.answer").await;
    assert_eq!((sent["request_id"].as_str(), sent["option_id"].as_str()), (Some("r1"), Some("allow")));
}

/// Plan S18: the indicator shows what Swift reports, never Rust's commands.
#[tokio::test]
async fn the_indicator_follows_swifts_capture_state() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.hold(true, true);
    let g = speech.last_start().unwrap();
    assert!(r.sink.mics.lock().unwrap().is_empty(), "the press alone proves nothing");
    r.voice.capture(g, true);
    assert_eq!(*r.sink.mics.lock().unwrap(), [true]);
    r.voice.hold(false, true);
    assert_eq!(*r.sink.mics.lock().unwrap(), [true], "on until Swift says capture stopped");
    r.voice.capture(g, false);
    assert_eq!(*r.sink.mics.lock().unwrap(), [true, false]);
    r.voice.capture(g + 100, true);
    assert_eq!(speech.calls().last(), Some(&Call::Cancel(g + 100)), "capture nobody asked for is stopped");
    assert_eq!(*r.sink.mics.lock().unwrap(), [true, false]);
}

/// Plan S6, spec §4.3: the target is the card selected at the press.
#[tokio::test]
async fn the_snapshot_freezes_when_the_key_goes_down() {
    let mut b = pending_agent();
    b["id"] = "b1".into();
    b["title"] = "other".into();
    b["pending"]["id"] = "r2".into();
    b["pending"]["agent_id"] = "b1".into();
    let (r, speech) = speech_rig(vec![pending_agent(), b]).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1", "m1/b1"]));
    r.voice.hold(true, true);
    let g = speech.last_start().unwrap();
    r.voice.publish(Presentation { version: 2, ..shown(Some("m1/b1"), &["m1/a1", "m1/b1"]) });
    r.voice.hold(false, true);
    r.voice.speech_event(final_(g, "approve"));
    let sent = frame(&r.f, "request.answer").await;
    assert_eq!(sent["request_id"].as_str(), Some("r1"), "the card selected at the press");
}

#[tokio::test]
async fn partial_text_shows_while_listening() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.hold(true, true);
    let g = speech.last_start().unwrap();
    r.voice.speech_event(ListenEvent::Partial { generation: g, text: VoiceText::new("appr") });
    assert_eq!(r.sink.last(), Some(Hud::Listening { id: g, partial: Some(VoiceText::new("appr")), over: None }));
}

#[tokio::test]
async fn a_short_recording_sends_nothing_and_hides_the_hud() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.hold(true, true);
    let g = speech.last_start().unwrap();
    r.voice.hold(false, true);
    r.voice.speech_event(ListenEvent::Final { generation: g, text: VoiceText::new("approve"), audio: ms(200), voiced: ms(200) });
    assert_eq!(r.sink.last(), Some(Hud::Hidden));
    tokio::time::sleep(ms(400)).await; // past the 200 ms undo window
    assert!(frames(&r.f, "request.answer").is_empty());
}

/// Review Focus 1.
#[tokio::test]
async fn the_cap_stops_the_microphone_and_sends_nothing() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.set_listen_limits_for_test(Limits { max: ms(200), final_timeout: ms(200) });
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.hold(true, true);
    let g = speech.last_start().unwrap();
    hud_until(&r.sink, "too long", |h| *h == Hud::Error { refusal: Refusal::RecordingTooLong, transcript: None }).await;
    assert_eq!(speech.calls(), [Call::Start(g), Call::Cancel(g)]);
    r.voice.speech_event(final_(g, "approve"));
    r.voice.hold(false, true);
    tokio::time::sleep(ms(400)).await;
    assert!(frames(&r.f, "request.answer").is_empty());
    assert_eq!(speech.calls().len(), 2, "the late key-up stops nothing");
}

#[tokio::test]
async fn a_missing_final_fails_closed() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.set_listen_limits_for_test(Limits { max: ms(60_000), final_timeout: ms(200) });
    r.voice.hold(true, true);
    let g = speech.last_start().unwrap();
    r.voice.hold(false, true);
    hud_until(&r.sink, "timed out", |h| *h == Hud::Error { refusal: Refusal::SpeechUnavailable, transcript: None }).await;
    assert_eq!(speech.calls(), [Call::Start(g), Call::Stop(g), Call::Cancel(g)]);
}

/// Review Focus 2.
#[tokio::test]
async fn typing_with_the_hold_key_down_cancels_the_recording() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.hold(true, true);
    let g = speech.last_start().unwrap();
    r.voice.other_key();
    assert_eq!(speech.calls(), [Call::Start(g), Call::Cancel(g)]);
    assert_eq!(r.sink.last(), Some(Hud::Hidden));
    r.voice.hold(false, true);
    assert_eq!(speech.calls().len(), 2, "the key-up after a chord does nothing");
    r.voice.speech_event(final_(g, "approve"));
    tokio::time::sleep(ms(400)).await;
    assert!(frames(&r.f, "request.answer").is_empty());
}

/// Review Focus 2, plan S5: a lost event might have been the chord.
#[tokio::test]
async fn an_overflow_cancels_the_recording_and_its_final_never_routes() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.hold(true, true);
    let g = speech.last_start().unwrap();
    r.voice.overflow();
    assert_eq!(speech.calls(), [Call::Start(g), Call::Cancel(g)]);
    r.voice.hold(false, true);
    assert_eq!(speech.calls().len(), 2, "the hold state was reset");
    r.voice.speech_event(final_(g, "approve"));
    tokio::time::sleep(ms(400)).await;
    assert!(frames(&r.f, "request.answer").is_empty());
}

/// Plan S26: a replaced tap may lose the key-up.
#[tokio::test]
async fn a_lost_hold_cancels_the_recording() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.hold(true, true);
    let g = speech.last_start().unwrap();
    r.voice.hold_lost();
    assert_eq!(speech.calls(), [Call::Start(g), Call::Cancel(g)]);
    r.voice.hold(false, true);
    assert_eq!(speech.calls().len(), 2);
}

/// Review Focus 3, spec §4.4.
#[tokio::test]
async fn the_trigger_cancels_a_running_countdown_before_anything_is_heard() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.text_input("approve".into(), true).await.unwrap();
    hud_until(&r.sink, "countdown", countdown_started).await;
    r.voice.hold(true, true);
    assert!(matches!(r.sink.last(), Some(Hud::Listening { .. })));
    tokio::time::sleep(ms(400)).await;
    assert!(frames(&r.f, "request.answer").is_empty(), "cancelled before any audio arrived");
    let g = speech.last_start().unwrap();
    r.voice.hold(false, true);
    r.voice.speech_event(final_(g, "cancel"));
    hud_until(&r.sink, "cancelled", |h| matches!(h, Hud::Done { outcome: OutcomeView::Cancelled, .. })).await;
}

/// Review Focus 4.
#[tokio::test]
async fn a_late_final_from_a_cancelled_recording_routes_nothing() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.hold(true, true);
    let g1 = speech.last_start().unwrap();
    r.voice.cancel(Some(g1)); // the HUD's Cancel names the recording
    r.voice.hold(false, true);
    assert_eq!(speech.calls(), [Call::Start(g1), Call::Cancel(g1)]);
    r.voice.hold(true, true);
    let g2 = speech.last_start().unwrap();
    assert_ne!(g1, g2);
    r.voice.speech_event(final_(g1, "approve"));
    tokio::time::sleep(ms(400)).await;
    assert!(frames(&r.f, "request.answer").is_empty());
    assert!(r.voice.listening(), "the old final did not end the new recording");
}

/// Review Focus 4, plan S23: a newer press between the recorder and
/// admission. Without the token, the old "confirm" would answer the confirm
/// that is still waiting.
#[tokio::test]
async fn a_final_admitted_after_a_newer_press_is_dropped() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    confirm_on_screen(&r).await;
    r.voice.hold(true, true);
    let g1 = speech.last_start().unwrap();
    r.voice.hold(false, true);
    let fired = at_next_admission(&r.voice, |v| v.hold(true, true));
    r.voice.speech_event(final_(g1, "confirm"));
    until("the barrier", || fired.load(Ordering::SeqCst)).await;
    let g2 = speech.last_start().unwrap();
    assert_ne!(g1, g2);
    tokio::time::sleep(ms(400)).await;
    assert!(frames(&r.f, "request.answer").is_empty(), "the old confirm answered nothing");
    assert!(matches!(r.sink.last(), Some(Hud::Listening { id, over: Some(_), .. }) if id == g2));
}

/// Review Focus 4, plan S23 and S24.
#[tokio::test]
async fn a_final_admitted_after_a_lock_is_dropped() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.hold(true, true);
    let g = speech.last_start().unwrap();
    r.voice.hold(false, true);
    let fired = at_next_admission(&r.voice, |v| v.system_stop());
    r.voice.speech_event(final_(g, "approve"));
    until("the barrier", || fired.load(Ordering::SeqCst)).await;
    tokio::time::sleep(ms(400)).await;
    assert!(frames(&r.f, "request.answer").is_empty());
}

/// Review Focus 4, plan S23.
#[tokio::test]
async fn a_final_admitted_after_voice_off_and_on_is_dropped() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.hold(true, true);
    let g = speech.last_start().unwrap();
    r.voice.hold(false, true);
    let fired = at_next_admission(&r.voice, |v| {
        v.set_enabled(false);
        v.set_enabled(true);
    });
    r.voice.speech_event(final_(g, "approve"));
    until("the barrier", || fired.load(Ordering::SeqCst)).await;
    tokio::time::sleep(ms(400)).await;
    assert!(frames(&r.f, "request.answer").is_empty());
}

/// Plan S5 and S23: an overflow drops a final already on its way.
#[tokio::test]
async fn an_overflow_before_admission_drops_the_final() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.hold(true, true);
    let g = speech.last_start().unwrap();
    r.voice.hold(false, true);
    let fired = at_next_admission(&r.voice, |v| v.overflow());
    r.voice.speech_event(final_(g, "approve"));
    until("the barrier", || fired.load(Ordering::SeqCst)).await;
    tokio::time::sleep(ms(400)).await;
    assert!(frames(&r.f, "request.answer").is_empty());
}

/// Review Focus 5.
#[tokio::test]
async fn voice_off_mid_recording_stops_the_microphone() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.hold(true, true);
    let g = speech.last_start().unwrap();
    r.voice.capture(g, true);
    r.voice.set_enabled(false);
    assert_eq!(speech.calls(), [Call::Start(g), Call::Cancel(g)]);
    r.voice.capture(g, false); // Swift reports the stop
    assert_eq!(*r.sink.mics.lock().unwrap(), [true, false]);
    r.voice.speech_event(final_(g, "approve"));
    tokio::time::sleep(ms(400)).await;
    assert!(frames(&r.f, "request.answer").is_empty());
}

/// Review Focus 5.
#[tokio::test]
async fn screen_lock_cancels_the_recording() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.hold(true, true);
    let g = speech.last_start().unwrap();
    r.voice.system_stop();
    assert_eq!(speech.calls(), [Call::Start(g), Call::Cancel(g)]);
    assert_eq!(r.sink.last(), Some(Hud::Hidden));
    r.voice.hold(false, true);
    assert_eq!(speech.calls().len(), 2, "the key-up after a lock does nothing");
}

/// Lock and overflow invalidate alike (plan S5, S24): a dropped event may
/// have been the lock. These helpers take the trigger, and each test runs
/// one of them for each trigger. None sleeps: the countdown uses a 30 s undo
/// window and waits for its `Cancelled` outcome.
async fn invalidation_clears_a_waiting_confirm(invalidate: fn(&Controller)) {
    let (r, _speech) = speech_rig(vec![pending_agent()]).await;
    let proposal_id = confirm_on_screen(&r).await;
    invalidate(&r.voice);
    assert_eq!(r.sink.last(), Some(Hud::Hidden));
    r.voice.confirm(proposal_id);
    assert_eq!(r.sink.last(), Some(Hud::Hidden), "nothing left to confirm");
    r.voice.text_input("confirm".into(), true).await.unwrap();
    assert!(!matches!(r.sink.last(), Some(Hud::Countdown { .. })));
    assert!(r.inbox.view().borrow().actions.is_empty(), "nothing was enqueued");
}

async fn invalidation_cancels_an_unsent_countdown(invalidate: fn(&Controller)) {
    let (r, _speech) = speech_rig_slow(vec![pending_agent()], None).await;
    let mut outcomes = r.inbox.action_outcomes();
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.text_input("approve".into(), true).await.unwrap();
    let Hud::Countdown { action_id: Some(id), .. } = hud_until(&r.sink, "countdown", countdown_started).await else {
        unreachable!()
    };
    invalidate(&r.voice);
    assert_eq!(r.sink.last(), Some(Hud::Hidden));
    loop {
        let e = outcomes.recv().await.unwrap();
        if e.action_id == id {
            assert!(matches!(e.outcome, ActionOutcome::Cancelled), "cancelled, never fired");
            break;
        }
    }
    assert!(frames(&r.f, "request.answer").is_empty());
}

async fn invalidation_drops_a_thinking_step(invalidate: fn(&Controller)) {
    let gate = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let model: Arc<dyn IntentModel> = Arc::new(Gated { gate: gate.clone(), dropped: dropped.clone() });
    let (r, _speech) = speech_rig_with(vec![pending_agent(), working_agent()], Some(model), Display::Acks).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1", "m1/a2"]));
    let voice = r.voice.clone();
    let old = tokio::spawn(async move { voice.text_input("tell the box claude something".into(), true).await });
    hud_until(&r.sink, "thinking", |h| matches!(h, Hud::Thinking { .. })).await;
    invalidate(&r.voice);
    assert_eq!(old.await.unwrap(), Ok(()));
    assert!(dropped.load(Ordering::SeqCst), "the model call was cancelled");
    gate.notify_waiters();
    assert_eq!(r.sink.last(), Some(Hud::Hidden));
    assert!(frames(&r.f, "agent.send").is_empty());
}

/// Review Focus 5, plan S24: nothing a lock leaves behind can be confirmed.
#[tokio::test]
async fn screen_lock_clears_a_waiting_confirm() {
    invalidation_clears_a_waiting_confirm(Controller::system_stop).await;
}

/// Review Focus 5, plan S24.
#[tokio::test]
async fn screen_lock_cancels_an_unsent_countdown() {
    invalidation_cancels_an_unsent_countdown(Controller::system_stop).await;
}

/// Review Focus 5, plan S24: a model call in flight is cancelled.
#[tokio::test]
async fn screen_lock_drops_a_thinking_step() {
    invalidation_drops_a_thinking_step(Controller::system_stop).await;
}

/// Review Focus 2, plan S5: a dropped event may have been the lock.
#[tokio::test]
async fn an_overflow_clears_a_waiting_confirm() {
    invalidation_clears_a_waiting_confirm(Controller::overflow).await;
}

#[tokio::test]
async fn an_overflow_cancels_an_unsent_countdown() {
    invalidation_cancels_an_unsent_countdown(Controller::overflow).await;
}

#[tokio::test]
async fn an_overflow_drops_a_thinking_step() {
    invalidation_drops_a_thinking_step(Controller::overflow).await;
}

/// S18: Swift reports per recording in order, and Rust also rejects an
/// `on` that arrives after that recording's `off`.
#[tokio::test]
async fn a_late_capture_on_after_off_never_lights_the_indicator() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.hold(true, true);
    let g = speech.last_start().unwrap();
    r.voice.hold(false, true);
    r.voice.capture(g, false);
    r.voice.capture(g, true);
    assert!(r.sink.mics.lock().unwrap().is_empty(), "never lit");
    assert_eq!(speech.calls(), [Call::Start(g), Call::Stop(g)], "a stale on is ignored, not answered with a cancel");
}

/// S18: generations only grow, so an `off` for a newer recording also
/// clears an older one whose own `off` was coalesced away.
#[tokio::test]
async fn an_off_for_a_newer_recording_clears_an_older_on() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.hold(true, true);
    let g1 = speech.last_start().unwrap();
    r.voice.capture(g1, true);
    r.voice.hold(false, true);
    r.voice.hold(true, true);
    let g2 = speech.last_start().unwrap();
    assert!(g2 > g1);
    r.voice.capture(g2, false);
    assert_eq!(*r.sink.mics.lock().unwrap(), [true, false]);
}

/// Plan S25.
#[tokio::test]
async fn esc_in_the_inbox_cancels_a_toggle_recording() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    assert!(!r.voice.escape_listening(), "nothing to cancel: the inbox handles Esc");
    r.voice.toggle(true);
    let g = speech.last_start().unwrap();
    assert!(r.voice.escape_listening());
    assert_eq!(speech.calls(), [Call::Start(g), Call::Cancel(g)]);
    assert!(!r.voice.listening());
    r.voice.speech_event(final_(g, "approve"));
    tokio::time::sleep(ms(400)).await;
    assert!(frames(&r.f, "request.answer").is_empty());
}

/// Plan S25: `cancel(None)` goes through the recorder too.
#[tokio::test]
async fn cancel_without_an_id_cancels_the_recording() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.hold(true, true);
    let g = speech.last_start().unwrap();
    r.voice.cancel(None);
    assert_eq!(speech.calls(), [Call::Start(g), Call::Cancel(g)]);
    assert!(!r.voice.listening());
}

#[tokio::test]
async fn the_toggle_starts_and_a_second_press_stops() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.toggle(false);
    let g = speech.last_start().unwrap();
    assert!(r.voice.listening());
    r.voice.toggle(false);
    assert_eq!(speech.calls(), [Call::Start(g), Call::Stop(g)]);
    assert!(!r.voice.listening());
}

/// Review: a hold press during a toggle recording must not become its hold.
#[tokio::test]
async fn a_hold_press_during_a_toggle_recording_leaves_it_alone() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.toggle(false);
    let g = speech.last_start().unwrap();
    r.voice.hold(true, false);
    r.voice.hold(false, false);
    assert!(r.voice.listening(), "the release did not stop the toggle recording");
    r.voice.hold(true, false);
    r.voice.other_key();
    assert!(r.voice.listening(), "a chord did not cancel it");
    assert_eq!(speech.calls(), [Call::Start(g)]);
}

/// Review: typed input cannot run beside a live recording.
#[tokio::test]
async fn typed_input_is_refused_while_a_recording_runs() {
    let (r, _speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.hold(true, true);
    let err = r.voice.text_input("approve".into(), true).await.unwrap_err();
    assert_eq!(err.to_string(), "A recording is running. Finish or cancel it first.");
    r.voice.hold(false, true);
    assert!(r.voice.text_input("approve".into(), true).await.is_err(), "still finishing");
}

#[tokio::test]
async fn a_press_with_voice_off_listens_to_nothing() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.set_enabled(false);
    r.voice.hold(true, true);
    r.voice.toggle(true);
    assert!(speech.calls().is_empty());
    assert!(!r.sink.huds.lock().unwrap().iter().any(|(_, h)| matches!(h, Hud::Listening { .. })));
}

#[tokio::test]
async fn a_refused_start_names_the_microphone_and_listens_to_nothing() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    *speech.refuse.lock().unwrap() = Some(ListenError::MicrophoneDenied);
    r.voice.hold(true, true);
    assert_eq!(r.sink.last(), Some(Hud::Error { refusal: Refusal::MicrophoneDenied, transcript: None }));
    assert!(!r.voice.listening());
}

#[tokio::test]
async fn an_unavailable_backend_is_never_started() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    *speech.caps.lock().unwrap() =
        Some(Capabilities { unavailable: Some(Unavailable::AssetsMissing), streams_partials: false, sample_rate: 0 });
    r.voice.hold(true, true);
    assert_eq!(r.sink.last(), Some(Hud::Error { refusal: Refusal::SpeechAssetsMissing, transcript: None }));
    assert!(speech.calls().is_empty());
}

/// Spec §9 "HUD says so", without hiding a step that waits.
#[tokio::test]
async fn a_notice_never_hides_a_waiting_step() {
    let (r, _speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.notice(Refusal::HoldKeyOff);
    assert_eq!(r.sink.last(), Some(Hud::Error { refusal: Refusal::HoldKeyOff, transcript: None }));
    confirm_on_screen(&r).await;
    r.voice.notice(Refusal::HoldKeyOff);
    assert!(matches!(r.sink.last(), Some(Hud::Confirm { .. })));
}

/// Plan S7: confirm stays on screen under Listening, and the spoken
/// "confirm" confirms it.
#[tokio::test]
async fn a_spoken_confirm_confirms_the_step_on_screen() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    confirm_on_screen(&r).await;
    r.voice.hold(true, true);
    assert!(matches!(r.sink.last(), Some(Hud::Listening { over: Some(_), .. })));
    let g = speech.last_start().unwrap();
    r.voice.hold(false, true);
    r.voice.speech_event(final_(g, "confirm"));
    let sent = frame(&r.f, "request.answer").await;
    assert_eq!(sent["option_id"].as_str(), Some("always"));
}

/// D22 for speech: a HUD that never acknowledges sends nothing.
#[tokio::test]
async fn a_withheld_display_acknowledgement_sends_nothing() {
    let (r, speech) = speech_rig_with(vec![pending_agent()], None, Display::Silent).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.hold(true, true);
    let g = speech.last_start().unwrap();
    r.voice.hold(false, true);
    r.voice.speech_event(final_(g, "approve"));
    hud_until(&r.sink, "display failed", |h| *h == Hud::Error { refusal: Refusal::DisplayFailed, transcript: None }).await;
    tokio::time::sleep(ms(400)).await;
    assert!(frames(&r.f, "request.answer").is_empty());
}

/// D22 for speech: a refused display fails closed at once.
#[tokio::test]
async fn a_refused_display_sends_nothing() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.sink.refuse_display.store(true, Ordering::SeqCst);
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.hold(true, true);
    let g = speech.last_start().unwrap();
    r.voice.hold(false, true);
    r.voice.speech_event(final_(g, "approve"));
    hud_until(&r.sink, "display failed", |h| *h == Hud::Error { refusal: Refusal::DisplayFailed, transcript: None }).await;
    tokio::time::sleep(ms(400)).await;
    assert!(frames(&r.f, "request.answer").is_empty());
}

/// The live revalidation still guards speech: a revision bump after the
/// press drops the step.
#[tokio::test]
async fn a_revision_bump_after_the_press_drops_the_spoken_step() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.hold(true, true);
    let g = speech.last_start().unwrap();
    let mut agent = pending_agent();
    agent["pending"]["revision"] = json!(2);
    r.c.send(json!({"type":"agent.updated","epoch":"e1","seq":2,"agent":agent})).await;
    view_until(&r.inbox, "revision 2", |v| {
        v.cards.iter().any(|c| c.agent.pending.as_ref().is_some_and(|p| p.revision == 2))
    })
    .await;
    r.voice.hold(false, true);
    r.voice.speech_event(final_(g, "approve"));
    hud_until(&r.sink, "dropped", |h| *h == Hud::Error { refusal: Refusal::SnapshotChanged, transcript: None }).await;
    tokio::time::sleep(ms(400)).await;
    assert!(frames(&r.f, "request.answer").is_empty());
}

/// A replaced confirmation: "confirm" answers only the one on screen, and
/// a click meant for the old one does nothing.
#[tokio::test]
async fn a_spoken_confirm_answers_only_the_current_confirmation() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    let old = confirm_on_screen(&r).await;
    r.voice.cancel(Some(old));
    hud_until(&r.sink, "cancelled", |h| matches!(h, Hud::Done { outcome: OutcomeView::Cancelled, .. })).await;
    let new = confirm_on_screen(&r).await;
    assert_ne!(old, new);
    r.voice.hold(true, true);
    let g = speech.last_start().unwrap();
    r.voice.hold(false, true);
    r.voice.speech_event(final_(g, "confirm"));
    frame(&r.f, "request.answer").await;
    r.voice.confirm(old);
    tokio::time::sleep(ms(400)).await;
    assert_eq!(frames(&r.f, "request.answer").len(), 1);
}

/// A final the user never released routes nothing, and the recording goes on.
#[tokio::test]
async fn a_final_while_the_key_is_still_down_routes_nothing() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.hold(true, true);
    let g = speech.last_start().unwrap();
    r.voice.speech_event(final_(g, "approve"));
    assert!(r.voice.listening(), "still recording");
    assert!(matches!(r.sink.last(), Some(Hud::Listening { .. })));
    tokio::time::sleep(ms(400)).await;
    assert!(frames(&r.f, "request.answer").is_empty());
    assert_eq!(speech.calls(), [Call::Start(g)]);
}

/// A reset never hands a new recording an old generation.
#[tokio::test]
async fn a_recording_after_a_lock_gets_a_new_generation() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.hold(true, true);
    let g1 = speech.last_start().unwrap();
    r.voice.system_stop();
    r.voice.hold(true, true);
    let g2 = speech.last_start().unwrap();
    assert!(g2 > g1);
    r.voice.speech_event(final_(g1, "approve"));
    assert!(r.voice.listening(), "the old final did not end the new recording");
}

/// Spec §4.4: the press cancels a running voice countdown even when nothing
/// can listen.
#[tokio::test]
async fn an_unavailable_backend_still_cancels_a_running_countdown() {
    let (r, speech) = speech_rig_slow(vec![pending_agent()], None).await;
    let mut outcomes = r.inbox.action_outcomes();
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.text_input("approve".into(), true).await.unwrap();
    let Hud::Countdown { action_id: Some(id), .. } = hud_until(&r.sink, "countdown", countdown_started).await else {
        unreachable!()
    };
    *speech.caps.lock().unwrap() =
        Some(Capabilities { unavailable: Some(Unavailable::AssetsMissing), streams_partials: false, sample_rate: 0 });
    r.voice.hold(true, true);
    loop {
        let e = outcomes.recv().await.unwrap();
        if e.action_id == id {
            assert!(matches!(e.outcome, ActionOutcome::Cancelled), "cancelled, never fired");
            break;
        }
    }
    assert_eq!(r.sink.last(), Some(Hud::Error { refusal: Refusal::SpeechAssetsMissing, transcript: None }));
    assert!(speech.calls().is_empty());
    assert!(frames(&r.f, "request.answer").is_empty());
}

/// Final review: an outcome that lands during a recording must not take the
/// HUD from Listening (the microphone indicator is on it, spec §6.2). The
/// result shows when the recording ends with nothing to route.
#[tokio::test]
async fn an_outcome_during_a_recording_waits_for_the_recording_to_end() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.text_input("approve".into(), true).await.unwrap();
    let sent = frame(&r.f, "request.answer").await;
    hud_until(&r.sink, "sending", |h| matches!(h, Hud::Sending { .. })).await;
    r.voice.hold(true, true);
    let g = speech.last_start().unwrap();
    r.c.send(json!({"type":"result","id":sent["id"],"ok":false,"error":{"code":"request_gone","message":"gone"}})).await;
    tokio::time::sleep(ms(150)).await;
    assert_eq!(r.sink.last(), Some(Hud::Listening { id: g, partial: None, over: None }), "the result did not take the HUD");
    r.voice.hold(false, true);
    r.voice.speech_event(ListenEvent::Final { generation: g, text: VoiceText::new("uh"), audio: ms(200), voiced: ms(200) });
    assert!(
        matches!(r.sink.last(), Some(Hud::Done { outcome: OutcomeView::Rejected { .. }, .. })),
        "the result shows once the recording ended: {:?}",
        r.sink.last()
    );
}

/// Final review: Esc or Cancel after the final left the recorder, before
/// admission, must still stop the command. The HUD shows Listening then.
#[tokio::test]
async fn esc_between_the_final_and_admission_sends_nothing() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.hold(true, true);
    let g = speech.last_start().unwrap();
    r.voice.hold(false, true);
    let esc = Arc::new(AtomicBool::new(false));
    let seen = esc.clone();
    let fired = at_next_admission(&r.voice, move |v| seen.store(v.escape_listening(), Ordering::SeqCst));
    r.voice.speech_event(final_(g, "approve"));
    until("the barrier", || fired.load(Ordering::SeqCst)).await;
    assert!(esc.load(Ordering::SeqCst), "Esc found something to cancel");
    tokio::time::sleep(ms(400)).await;
    assert!(frames(&r.f, "request.answer").is_empty());
    assert_eq!(r.sink.last(), Some(Hud::Hidden));
}

#[tokio::test]
async fn hud_cancel_between_the_final_and_admission_sends_nothing() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.hold(true, true);
    let g = speech.last_start().unwrap();
    r.voice.hold(false, true);
    let fired = at_next_admission(&r.voice, move |v| v.cancel(Some(g)));
    r.voice.speech_event(final_(g, "approve"));
    until("the barrier", || fired.load(Ordering::SeqCst)).await;
    tokio::time::sleep(ms(400)).await;
    assert!(frames(&r.f, "request.answer").is_empty());
}

/// Esc with nothing in flight still goes to the inbox.
#[tokio::test]
async fn esc_with_nothing_to_cancel_is_left_to_the_inbox() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.hold(true, true);
    let g = speech.last_start().unwrap();
    r.voice.hold(false, true);
    r.voice.speech_event(final_(g, "approve"));
    hud_until(&r.sink, "countdown", countdown_started).await;
    assert!(!r.voice.escape_listening(), "admission cleared the marker");
}

/// Final re-review: a result held back during a recording still reaches the
/// HUD when the recording turns into a new command.
#[tokio::test]
async fn a_result_held_during_a_recording_is_not_lost_to_the_next_command() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.text_input("approve".into(), true).await.unwrap();
    let sent = frame(&r.f, "request.answer").await;
    hud_until(&r.sink, "sending", |h| matches!(h, Hud::Sending { .. })).await;
    r.voice.hold(true, true);
    let g = speech.last_start().unwrap();
    r.c.send(json!({"type":"result","id":sent["id"],"ok":false,"error":{"code":"request_gone","message":"gone"}})).await;
    tokio::time::sleep(ms(150)).await;
    r.voice.hold(false, true);
    r.voice.speech_event(final_(g, "deny"));
    until("the late result reached the HUD", || {
        r.sink
            .huds
            .lock()
            .unwrap()
            .iter()
            .any(|(_, h)| matches!(h, Hud::Done { outcome: OutcomeView::Rejected { .. }, .. }))
    })
    .await;
}

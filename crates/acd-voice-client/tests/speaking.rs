mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use acd_voice::hud::Hud;
use acd_voice::listen::fake::{Call, FakeSpeechInput};
use acd_voice::refusal::Refusal;
use acd_voice::speak::fake::{FakeSpeaker, Said};
use acd_voice::speak::{Rate, Speaker, SpeechSettings};
use acd_voice::text::VoiceText;
use common::*;

async fn read_it(r: &Rig) -> u64 {
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.text_input("read it".into(), true).await.unwrap();
    hud_until(&r.sink, "speaking", |h| matches!(h, Hud::Speaking { audio: true, .. })).await;
    last_seq(&r.sink)
}

/// Spec §7, §4.4, plan V11.
#[tokio::test]
async fn a_read_is_spoken_and_the_hud_fades_from_its_end() {
    let (r, speaker) = speaking_rig(vec![pending_agent()]).await;
    let seq = read_it(&r).await;
    let (generation, text) = speaker.last_say().unwrap();
    assert_eq!(text, "Bash approval. npm test");
    r.voice.spoken(generation);
    assert_eq!(*r.sink.ended.lock().unwrap(), [seq]);
    r.voice.spoken(generation);
    assert_eq!(r.sink.ended.lock().unwrap().len(), 1, "a second end changes nothing");
}

#[tokio::test]
async fn without_a_speaker_the_text_shows_on_its_timer() {
    let r = rig(vec![pending_agent()], None, Display::Acks).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.text_input("read it".into(), true).await.unwrap();
    hud_until(&r.sink, "speaking", |h| matches!(h, Hud::Speaking { audio: false, .. })).await;
}

/// Records how many recordings had started each time speech was stopped.
struct OrderSpeaker {
    speech: Arc<FakeSpeechInput>,
    starts_at_stop: Mutex<Vec<usize>>,
    inner: FakeSpeaker,
}

impl Speaker for OrderSpeaker {
    fn say(&self, generation: u64, text: &VoiceText, voice: Option<&str>, rate: Rate) {
        self.inner.say(generation, text, voice, rate);
    }
    fn stop(&self) {
        let starts = self.speech.calls().iter().filter(|c| matches!(c, Call::Start(_))).count();
        self.starts_at_stop.lock().unwrap().push(starts);
        self.inner.stop();
    }
}

/// Review Focus 3: the microphone never records the app's own voice.
#[tokio::test]
async fn a_press_stops_speech_before_the_microphone_opens() {
    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    let speaker = Arc::new(OrderSpeaker { speech: speech.clone(), starts_at_stop: Mutex::new(Vec::new()), inner: FakeSpeaker::default() });
    r.voice.attach_speaker(speaker.clone());
    read_it(&r).await;
    r.voice.hold(true, true);
    assert_eq!(speaker.starts_at_stop.lock().unwrap().first(), Some(&0), "stopped before capture started");
    assert!(speech.last_start().is_some(), "then the microphone opened");
}

/// Spec §7, plan V12.
#[tokio::test]
async fn the_stop_button_or_esc_stops_speech_and_hides_the_hud() {
    let (r, speaker) = speaking_rig(vec![pending_agent()]).await;
    let seq = read_it(&r).await;
    assert!(!r.voice.stop_speaking(Some(seq - 1)), "a stop for another state is ignored");
    assert!(r.voice.stop_speaking(Some(seq)));
    assert_eq!(speaker.calls().last(), Some(&Said::Stop));
    assert_eq!(r.sink.last(), Some(Hud::Hidden));
    assert!(!r.voice.stop_speaking(Some(seq)), "nothing is speaking now");
    read_it(&r).await;
    assert!(r.voice.stop_speaking(None), "Esc in the inbox panel");
}

#[tokio::test]
async fn a_late_end_from_stopped_speech_is_ignored() {
    let (r, speaker) = speaking_rig(vec![pending_agent()]).await;
    let seq = read_it(&r).await;
    let (generation, _) = speaker.last_say().unwrap();
    r.voice.stop_speaking(Some(seq));
    r.voice.spoken(generation);
    assert!(r.sink.ended.lock().unwrap().is_empty());
}

/// Review Focus 5, spec §6.2, §6.3.
#[tokio::test]
async fn voice_off_lock_and_overflow_stop_speech() {
    let (r, speaker) = speaking_rig(vec![pending_agent()]).await;
    for stop in [0, 1, 2] {
        read_it(&r).await;
        let before = speaker.calls().len();
        match stop {
            0 => {
                r.voice.set_enabled(false);
                r.voice.set_enabled(true);
            }
            1 => r.voice.system_stop(),
            _ => r.voice.overflow(),
        }
        assert!(speaker.calls()[before..].contains(&Said::Stop), "case {stop}");
    }
}

/// Review Focus 5, plan V11.
#[tokio::test]
async fn speech_stops_at_the_cap() {
    let (r, speaker) = speaking_rig(vec![pending_agent()]).await;
    r.voice.set_speech_cap_for_test(Duration::from_millis(100));
    read_it(&r).await;
    hud_until(&r.sink, "hidden", |h| *h == Hud::Hidden).await;
    assert_eq!(speaker.calls().last(), Some(&Said::Stop));
}

/// Plan V13: another HUD state replacing the spoken reply stops it.
#[tokio::test]
async fn a_new_hud_state_stops_a_spoken_reply() {
    let (r, speaker) = speaking_rig(vec![pending_agent()]).await;
    read_it(&r).await;
    r.voice.notice(Refusal::HoldKeyOff);
    assert!(matches!(r.sink.last(), Some(Hud::Error { .. })));
    assert_eq!(speaker.calls().last(), Some(&Said::Stop));
}

#[tokio::test]
async fn a_typed_command_stops_speech() {
    let (r, speaker) = speaking_rig(vec![pending_agent()]).await;
    read_it(&r).await;
    let before = speaker.calls().len();
    r.voice.text_input("approve".into(), true).await.unwrap();
    assert!(speaker.calls()[before..].contains(&Said::Stop));
}

/// Spec §8.
#[tokio::test]
async fn the_voice_and_rate_reach_the_speaker() {
    let (r, speaker) = speaking_rig(vec![pending_agent()]).await;
    r.voice.set_speech(SpeechSettings { alerts: false, voice: Some("v1".into()), rate: Rate::Fast });
    read_it(&r).await;
    assert!(matches!(speaker.calls().last(), Some(Said::Say { voice: Some(v), rate: Rate::Fast, .. }) if v == "v1"));
}

/// Review: an end for an older generation must not fade the newer reply.
#[tokio::test]
async fn a_stale_end_does_not_fade_the_new_reply() {
    let (r, speaker) = speaking_rig(vec![pending_agent()]).await;
    read_it(&r).await;
    let (first, _) = speaker.last_say().unwrap();
    let seq2 = read_it(&r).await;
    let (second, _) = speaker.last_say().unwrap();
    assert_ne!(first, second);
    r.voice.spoken(first);
    assert!(r.sink.ended.lock().unwrap().is_empty(), "the old generation's end is ignored");
    r.voice.spoken(second);
    assert_eq!(*r.sink.ended.lock().unwrap(), [seq2]);
}

/// Review: an old reply's cap timer must not cut the reply that replaced it.
#[tokio::test]
async fn an_old_cap_does_not_cut_the_new_reply() {
    let (r, speaker) = speaking_rig(vec![pending_agent()]).await;
    r.voice.set_speech_cap_for_test(Duration::from_millis(100));
    read_it(&r).await;
    r.voice.set_speech_cap_for_test(Duration::from_secs(30));
    read_it(&r).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(matches!(speaker.calls().last(), Some(Said::Say { .. })), "the first reply's cap left the second playing");
    assert!(matches!(r.sink.last(), Some(Hud::Speaking { audio: true, .. })));
}

/// Review, plan V13: a reply the HUD could not show is not spoken on.
#[tokio::test]
async fn a_reply_the_hud_cannot_show_is_not_spoken_on() {
    let (r, speaker) = speaking_rig(vec![pending_agent()]).await;
    r.sink.refuse_speaking.store(true, std::sync::atomic::Ordering::SeqCst);
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.text_input("read it".into(), true).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(speaker.calls().last(), Some(&Said::Stop), "stopped as soon as the HUD refused it");
}

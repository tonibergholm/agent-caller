mod common;

use std::time::Duration;

use acd_voice::speak::SpeechSettings;
use acd_voice::speak::fake::FakeSpeaker;
use common::*;
use serde_json::{Value, json};

fn needing_you(id: &str, request: &str) -> Value {
    let mut a = pending_agent();
    a["id"] = id.into();
    a["pending"]["id"] = request.into();
    a["pending"]["agent_id"] = id.into();
    a
}

async fn said(speaker: &FakeSpeaker, n: usize) -> String {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if speaker.says() >= n {
                return speaker.last_say().unwrap().1;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("an alert was spoken")
}

async fn alerts_on(r: &Rig) {
    r.voice.set_speech(SpeechSettings { alerts: true, ..SpeechSettings::default() });
    tokio::time::sleep(Duration::from_millis(100)).await; // the baseline is taken
}

#[tokio::test]
async fn a_new_request_is_announced_once() {
    let (r, speaker) = speaking_rig(vec![working_agent()]).await;
    alerts_on(&r).await;
    r.c.send(json!({"type":"agent.updated","epoch":"e1","seq":2,"agent":needing_you("a2","r2")})).await;
    assert_eq!(said(&speaker, 1).await, "Box Claude needs you: bash approval");
    let (generation, _) = speaker.last_say().unwrap();
    r.voice.spoken(generation);
    r.c.send(json!({"type":"agent.updated","epoch":"e1","seq":3,"agent":needing_you("a2","r2")})).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(speaker.says(), 1);
}

/// Review Focus 4.
#[tokio::test]
async fn requests_waiting_when_alerts_turn_on_are_not_announced() {
    let (r, speaker) = speaking_rig(vec![pending_agent()]).await;
    alerts_on(&r).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(speaker.says(), 0);
}

/// Review blocker 1: the tests above never publish a presentation, so they
/// already prove alerts work before the panel's first publish. This one
/// publishes a presentation that predates the new card, as the panel does
/// between an inbox change and its next republish.
#[tokio::test]
async fn a_request_the_panel_has_not_shown_yet_is_announced() {
    let (r, speaker) = speaking_rig(vec![working_agent()]).await;
    r.voice.publish(shown(None, &["m1/a2"]));
    alerts_on(&r).await;
    r.c.send(json!({"type":"agent.updated","epoch":"e1","seq":2,"agent":needing_you("a3","r3")})).await;
    assert_eq!(said(&speaker, 1).await, "Box Claude needs you: bash approval");
}

#[tokio::test]
async fn alerts_are_off_by_default() {
    let (r, speaker) = speaking_rig(vec![working_agent()]).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    r.c.send(json!({"type":"agent.updated","epoch":"e1","seq":2,"agent":needing_you("a2","r2")})).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(speaker.says(), 0);
}

/// Review Focus 4.
#[tokio::test]
async fn two_new_requests_are_one_alert() {
    let (r, speaker) = speaking_rig(vec![working_agent()]).await;
    alerts_on(&r).await;
    let agents = json!([needing_you("a2", "r2"), needing_you("a3", "r3")]);
    r.c.send(json!({"type":"snapshot","epoch":"e1","seq":2,"agents":agents})).await;
    assert_eq!(said(&speaker, 1).await, "Box Claude needs you: bash approval. 1 more needs you.");
}

/// Spec §7: silent while recording.
#[tokio::test]
async fn an_alert_waits_until_the_recording_ends() {
    let (r, _speech) = speech_rig(vec![working_agent()]).await;
    let speaker = std::sync::Arc::new(FakeSpeaker::default());
    r.voice.attach_speaker(speaker.clone());
    alerts_on(&r).await;
    r.voice.hold(true, true);
    r.c.send(json!({"type":"agent.updated","epoch":"e1","seq":2,"agent":needing_you("a2","r2")})).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(speaker.says(), 0, "nothing while the microphone is on");
    assert!(r.voice.escape_listening());
    assert_eq!(said(&speaker, 1).await, "Box Claude needs you: bash approval");
}

#[tokio::test]
async fn voice_off_silences_alerts() {
    let (r, speaker) = speaking_rig(vec![working_agent()]).await;
    alerts_on(&r).await;
    r.voice.set_enabled(false);
    r.c.send(json!({"type":"agent.updated","epoch":"e1","seq":2,"agent":needing_you("a2","r2")})).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(speaker.says(), 0);
}
/// Spec §6.3 / review: stopping speech by voice off also stops a speaking
/// alert (it has no HUD state to hide, so only the explicit stop does it).
#[tokio::test]
async fn voice_off_stops_a_speaking_alert() {
    use acd_voice::speak::fake::Said;
    let (r, speaker) = speaking_rig(vec![working_agent()]).await;
    alerts_on(&r).await;
    r.c.send(json!({"type":"agent.updated","epoch":"e1","seq":2,"agent":needing_you("a2","r2")})).await;
    said(&speaker, 1).await;
    let before = speaker.calls().len();
    r.voice.set_enabled(false);
    assert!(speaker.calls()[before..].contains(&Said::Stop), "the alert was cut when voice went off");
}

/// Spec §7: a press stops an alert before the microphone opens.
#[tokio::test]
async fn a_press_stops_a_speaking_alert() {
    use acd_voice::speak::fake::Said;
    let (r, _speech) = speech_rig(vec![working_agent()]).await;
    let speaker = std::sync::Arc::new(FakeSpeaker::default());
    r.voice.attach_speaker(speaker.clone());
    alerts_on(&r).await;
    r.c.send(json!({"type":"agent.updated","epoch":"e1","seq":2,"agent":needing_you("a2","r2")})).await;
    said(&speaker, 1).await;
    let before = speaker.calls().len();
    r.voice.hold(true, true);
    assert!(speaker.calls()[before..].contains(&Said::Stop));
}

/// Spec §7, plan V12: a typed command stops an alert, which has no HUD state
/// of its own to hide, so only the explicit stop at admission does it.
#[tokio::test]
async fn a_typed_command_stops_a_speaking_alert() {
    use acd_voice::speak::fake::Said;
    let (r, speaker) = speaking_rig(vec![working_agent()]).await;
    alerts_on(&r).await;
    r.c.send(json!({"type":"agent.updated","epoch":"e1","seq":2,"agent":needing_you("a2","r2")})).await;
    said(&speaker, 1).await;
    let before = speaker.calls().len();
    r.voice.text_input("what is waiting".into(), true).await.unwrap();
    assert!(speaker.calls()[before..].contains(&Said::Stop));
}

/// Final review: spec §6.2 says locking stops speech. An alert for a request that
/// arrives while the screen is locked must not be read aloud on a locked Mac; it
/// is announced once the screen is back.
#[tokio::test]
async fn an_alert_waits_while_the_screen_is_locked() {
    let (r, speaker) = speaking_rig(vec![working_agent()]).await;
    alerts_on(&r).await;
    r.voice.system_stop();
    r.c.send(json!({"type":"agent.updated","epoch":"e1","seq":2,"agent":needing_you("a2","r2")})).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(speaker.says(), 0, "nothing is spoken while locked");
    r.voice.system_resume();
    assert_eq!(said(&speaker, 1).await, "Box Claude needs you: bash approval");
}

/// Final review: turning alerts off stops an alert that is already playing.
#[tokio::test]
async fn turning_alerts_off_stops_a_speaking_alert() {
    use acd_voice::speak::fake::Said;
    let (r, speaker) = speaking_rig(vec![working_agent()]).await;
    alerts_on(&r).await;
    r.c.send(json!({"type":"agent.updated","epoch":"e1","seq":2,"agent":needing_you("a2","r2")})).await;
    said(&speaker, 1).await;
    let before = speaker.calls().len();
    r.voice.set_speech(SpeechSettings { alerts: false, ..SpeechSettings::default() });
    assert!(speaker.calls()[before..].contains(&Said::Stop));
}

/// Final review: the app sets the speech settings before the speaker exists
/// (the bridge starts later). Attaching the speaker takes the baseline, so the
/// next request is news instead of becoming the baseline itself.
#[tokio::test]
async fn attaching_the_speaker_after_the_settings_still_takes_a_baseline() {
    let r = rig(vec![working_agent()], None, Display::Acks).await;
    r.voice.set_speech(SpeechSettings { alerts: true, ..SpeechSettings::default() });
    tokio::time::sleep(Duration::from_millis(100)).await; // no speaker yet: nothing to baseline
    let speaker = std::sync::Arc::new(FakeSpeaker::default());
    r.voice.attach_speaker(speaker.clone());
    tokio::time::sleep(Duration::from_millis(150)).await; // the baseline is taken here
    r.c.send(json!({"type":"agent.updated","epoch":"e1","seq":2,"agent":needing_you("a2","r2")})).await;
    assert_eq!(said(&speaker, 1).await, "Box Claude needs you: bash approval");
}

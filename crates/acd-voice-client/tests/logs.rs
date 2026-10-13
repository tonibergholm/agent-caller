mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use acd_voice::hud::Hud;
use acd_voice::router::model::{FakeIntentModel, IntentModel};
use common::*;
use serde_json::json;

#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Spec §3.7, through the real client transport: transcript, payload,
/// daemon error messages and result messages never reach the logs.
/// `set_default` is per thread, so the runtime is current-thread.
#[tokio::test(flavor = "current_thread")]
async fn voice_text_never_reaches_the_logs() {
    let buffer = Buffer::default();
    let writer = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let model: Arc<dyn IntentModel> = Arc::new(FakeIntentModel::replying(
        r#"{"step":"send","agent":"a1","text":"SECRET-PAYLOAD-7f3a"}"#,
    ));
    let r = rig(vec![working_agent()], Some(model), Display::Acks).await;
    r.voice.publish(shown(None, &["m1/a2"]));
    r.voice
        .text_input("tell the box claude SECRET-TRANSCRIPT-91c2 SECRET-PAYLOAD-7f3a".into(), false)
        .await
        .unwrap();
    let Hud::Confirm { proposal_id, .. } = hud_until(&r.sink, "confirm", |h| matches!(h, Hud::Confirm { .. })).await else {
        unreachable!()
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    r.voice.confirm(proposal_id);
    let sent = frame(&r.f, "agent.send").await;
    r.c.send(json!({"type":"error","code":"bad_request","message":"SECRET-DAEMON echo"})).await;
    r.c.send(json!({"type":"result","id":sent["id"],"ok":false,"error":{"code":"agent_busy","message":"SECRET-RESULT"}})).await;
    hud_until(&r.sink, "rejected", |h| matches!(h, Hud::Done { .. })).await;
    r.voice.text_input("SECRET-TRANSCRIPT-91c2 what now".into(), false).await.unwrap();

    let logs = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("utterance routed"), "the capture works: {logs}");
    assert!(logs.contains("voice action outcome"), "{logs}");
    assert!(logs.contains("daemon error"), "{logs}");
    assert!(!logs.contains("SECRET"), "{logs}");
}

/// Spec §3.7 for speech: partial and final transcripts never reach the logs.
#[tokio::test(flavor = "current_thread")]
async fn spoken_text_never_reaches_the_logs() {
    use acd_voice::listen::ListenEvent;
    use acd_voice::text::VoiceText;

    let buffer = Buffer::default();
    let writer = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let (r, speech) = speech_rig(vec![pending_agent()]).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.hold(true, true);
    let g = speech.last_start().unwrap();
    r.voice.speech_event(ListenEvent::Partial { generation: g, text: VoiceText::new("SECRET-PARTIAL-a1") });
    r.voice.hold(false, true);
    r.voice.speech_event(final_(g, "approve SECRET-SPOKEN-55"));
    hud_until(&r.sink, "routed", |h| matches!(h, Hud::Error { .. })).await;

    let logs = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("utterance routed") && logs.contains("source=\"speech\""), "{logs}");
    assert!(logs.contains("listening"), "{logs}");
    assert!(!logs.contains("SECRET"), "{logs}");
}

/// Spec §3.7 for speech: spoken text never reaches the logs.
#[tokio::test(flavor = "current_thread")]
async fn spoken_replies_never_reach_the_logs() {
    let buffer = Buffer::default();
    let writer = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let mut agent = pending_agent();
    agent["pending"]["detail"] = "SECRET-DETAIL-3b".into();
    let (r, speaker) = speaking_rig(vec![agent]).await;
    r.voice.publish(shown(Some("m1/a1"), &["m1/a1"]));
    r.voice.text_input("read it".into(), true).await.unwrap();
    hud_until(&r.sink, "speaking", |h| matches!(h, Hud::Speaking { .. })).await;
    let (generation, text) = speaker.last_say().unwrap();
    assert!(text.contains("SECRET-DETAIL-3b"), "the test speaks the secret");
    r.voice.spoken(generation);
    let logs = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("speaking"), "{logs}");
    assert!(!logs.contains("SECRET"), "{logs}");
}

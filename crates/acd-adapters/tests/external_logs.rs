//! Log privacy, alone in its binary: a thread-local subscriber is only
//! reliable when no other test registers tracing call sites at the same time.
use std::sync::Arc;

use serde_json::json;

mod external_support;
use external_support::*;

#[tokio::test]
async fn titles_details_answers_and_ack_messages_never_reach_the_logs() {
    #[derive(Clone, Default)]
    struct Buffer(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buffer = Buffer::default();
    let writer = buffer.clone();
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish(),
    );

    // Other tests in this binary run without a subscriber and may have cached
    // these call sites as disabled.
    tracing::callsite::rebuild_interest_cache();
    let mut rig = rig().await;
    let mut it = item("log-key", "a1", "SECRET-AGENT-TITLE");
    it["title"] = json!("SECRET-ITEM-TITLE");
    it["detail"] = json!("SECRET-DETAIL");
    it["question"]["text"] = json!("SECRET-QUESTION");
    it["options"][0]["label"] = json!("SECRET-LABEL");
    it["free_text"] = json!(true);
    rig.ok("sup", state(vec![it.clone()])).await;
    let request = rig.request("SECRET-AGENT-TITLE");
    rig.answer_text(&request, "SECRET-ANSWER-TEXT").await;
    let id = only_answer(&rig.get_answers("sup", 0).await.1)["answer_id"]
        .as_str()
        .unwrap()
        .to_owned();
    rig.ok(
        "sup",
        with_acks(
            vec![it],
            vec![ack(&id, "rejected", Some("SECRET-ACK-MESSAGE"))],
        ),
    )
    .await;
    let (status, _) = rig
        .put(
            "sup",
            state(vec![
                json!({"key": "bad key", "agent_key": "a", "title": "SECRET-INVALID-TITLE"}),
            ]),
        )
        .await;
    assert_eq!(status, 422);
    rig.stop("sup").await;

    let logs = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("sup"), "the producer id is logged: {logs}");
    assert!(logs.contains("log-key"), "the key is logged: {logs}");
    assert!(!logs.contains("SECRET"), "{logs}");
}

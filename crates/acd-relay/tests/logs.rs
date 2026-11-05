//! One test per process: it installs a global tracing subscriber at TRACE and
//! checks that no secret reaches a log line.

mod common;

use std::io::Write;
use std::sync::{Arc, Mutex};

use acd_relay::ManualClock;
use common::*;
use serde_json::json;
use tracing_subscriber::fmt::MakeWriter;

#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Buf {
    type Writer = Buf;
    fn make_writer(&'a self) -> Buf {
        self.clone()
    }
}

#[tokio::test]
async fn logs_never_contain_secrets() {
    let buf = Buf::default();
    {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;
        tracing_subscriber::registry().with(acd_relay::log_layer("trace", buf.clone())).init();
    }

    let fake = fake_fcm().await;
    let clock = Arc::new(ManualClock::new(T0));
    let r = start_clock(Setup::default(), fcm_client(&fake, clock.clone()), clock).await;
    let address = "tok:SECRET-ADDRESS-VALUE";
    let ip = "203.0.113.99";

    let (st, b) = r.post("/v1/handles", &Relay::reg_body(address), ip).await;
    assert_eq!(st, 202);
    let id = b["challenge_id"].as_str().unwrap().to_string();
    let code = fake.st.sends.lock().unwrap().last().unwrap().1["message"]["data"]["c"].as_str().unwrap().to_string();
    // A wrong-code attempt, then the right one.
    r.post("/v1/handles/confirm", &Relay::confirm_body(&id, "wrong-code-guess", address), ip).await;
    let (st, b) = r.post("/v1/handles/confirm", &Relay::confirm_body(&id, &code, address), ip).await;
    assert_eq!(st, 200);
    let handle = b["handle"].as_str().unwrap().to_string();
    assert_eq!(r.post("/v1/wake", &json!({"v":1,"handle":handle}), ip).await.0, 204);
    r.post("/v1/wake", &json!({"v":1,"handle":"tampered-handle-value"}), ip).await;
    // Failure paths: FCM errors and an OAuth failure.
    *fake.st.send_reply.lock().unwrap() = (400, format!(r#"{{"error":{{"status":"INVALID_ARGUMENT","message":"{address} {handle}"}}}}"#));
    r.post("/v1/wake", &json!({"v":1,"handle":handle}), ip).await;
    *fake.st.token_reply.lock().unwrap() = (400, format!(r#"{{"error":"{ACCESS_TOKEN}"}}"#));
    r.clock.advance(7200);
    let (_, _) = r.post("/v1/handles", &Relay::reg_body("tok:other"), ip).await;

    let logs = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("route=\"wake\"") || logs.contains("route=wake"), "no request lines logged:\n{logs}");
    let pem_body = TEST_KEY_PEM.lines().nth(1).unwrap();
    for (what, secret) in [
        ("address", address),
        ("handle", handle.as_str()),
        ("code", code.as_str()),
        ("wrong code", "wrong-code-guess"),
        ("challenge id", id.as_str()),
        ("source ip", ip),
        ("loopback ip", "127.0.0.1:"),
        ("access token", ACCESS_TOKEN),
        ("private key", pem_body),
        ("tampered handle", "tampered-handle-value"),
        ("other address", "tok:other"),
    ] {
        assert!(!logs.contains(secret), "{what} leaked into logs:\n{logs}");
    }
}

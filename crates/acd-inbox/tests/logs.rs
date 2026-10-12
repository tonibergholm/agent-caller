mod common;

use std::sync::{Arc, Mutex};

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

/// Spec §4.1 and CLAUDE.md: agent text, typed text, pairing strings, tokens
/// and raw socket lines never reach the logs. It installs the production
/// logging (`init_logging_to`, with the `log` bridge) once, at `trace`, so
/// `RUST_LOG` is shown unable to open the dependencies; this file holds
/// one test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_text_secrets_and_raw_lines_never_reach_the_logs() {
    let buffer = Buffer::default();
    let writer = buffer.clone();
    acd_inbox::serve::init_logging_to(Some("trace"), move || writer.clone()).unwrap();
    // What the dependencies would print at trace through the `log` bridge
    // and through tracing: frames carry tokens and agent text.
    log::trace!(target: "tungstenite::protocol", "SECRET-FRAME {TOKEN}");
    log::warn!(target: "tokio_tungstenite::compat", "SECRET-WARN");
    // Positive control: a `log` record from an allowed target arrives, so the
    // bridge is installed and the absences below mean something.
    log::info!(target: "acd_bridge_probe", "BRIDGE-OK");
    tracing::debug!(target: "rustls::conn", "SECRET-TLS");
    tracing::trace!(target: "zbus::connection", "SECRET-DBUS");

    let mut agent = pending_agent("a1", "r1");
    agent["title"] = "SECRET-TITLE".into();
    agent["pending"]["detail"] = "SECRET-DETAIL".into();
    agent["pending"]["raw_screen"] = "SECRET-SCREEN".into();
    let rig = rig(vec![agent], held()).await;
    let mut v = View::connect(&rig.socket).await;
    v.greeting().await;
    v.cmd(json!({"type":"reply","card":"m1/a1","text":"SECRET-REPLY"})).await;
    v.cmd(json!({"type":"answer_text","card":"m1/a1","request":"r1","revision":1,"text":"SECRET-ANSWER"})).await;
    v.cmd(json!({"type":"escape"})).await;
    v.cmd(json!({"type":"pair","pairing":format!("acd://h:1?pin=SECRET-PIN#{TOKEN}")})).await;
    let good = format!("acd://localhost:{}?pin={}#{}", rig.f.port(), rig.f.pin(), TOKEN);
    assert_eq!(v.cmd(json!({"type":"pair","pairing":good})).await["type"], "ok");
    v.cmd(json!({"type":"SECRET-TYPE"})).await;
    v.send_raw(b"{\"type\":\"pick\",\"id\":\"SECRET-ID-0123456789012345678901234567890123456789012345678901234567890123\"}\n").await;
    v.closed().await;
    let mut w = View::connect(&rig.socket).await;
    w.greeting().await;
    w.send_raw(b"SECRET-NOT-JSON\n").await;
    w.closed().await;
    let logs = || String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    until("both close lines", || logs().matches("connection closed").count() >= 2).await;

    let logs = logs();
    assert!(logs.contains("connection accepted"), "the capture works");
    assert!(logs.contains("line rejected"), "rejections are logged by tag");
    assert!(logs.contains("BRIDGE-OK"), "the log bridge is installed");
    assert!(logs.contains("command"), "trace is on for acd-inbox itself");
    assert!(!logs.contains("SECRET"), "agent, user or dependency text reached the logs");
    assert!(!logs.contains(TOKEN), "a token reached the logs");
    assert!(!logs.contains(&rig.f.pin()), "a pin reached the logs");
}

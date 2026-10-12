//! THROWAWAY SPIKE (2026-10-12). Runs acd-client's real Inbox (acd/1 over
//! TLS 1.3 with the pairing pin, token auth, snapshot sync) from a pairing
//! line. Built for the iOS simulator and run with `xcrun simctl spawn`.
//!
//! Inputs (environment only; the pairing line and token are never printed):
//!   ACD_PAIRING_FILE     file holding an acd pairing line from `acd pair`
//!   ACD_PROBE_BAD_PIN    if set, the pin is altered; the probe expects refusal
//!   ACD_PROBE_WAIT_SECS  how long to wait for Online (default 20)
//!
//! Exit: 0 = Online and snapshot read; 1 = import failed; 3 = not Online.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use acd_client::{
    Timing,
    inbox::{Inbox, InboxOptions},
    settings::{MemorySecrets, Settings},
};

/// Flips the first character of the pin's hash, keeping the `sha256:` prefix
/// valid so the line still parses and the TLS pin check is what refuses it.
fn altered_pin(line: &str) -> String {
    match line.find("sha256:") {
        Some(at) => {
            let i = at + "sha256:".len();
            let mut bytes = line.as_bytes().to_vec();
            bytes[i] = if bytes[i] == b'A' { b'B' } else { b'A' };
            String::from_utf8(bytes).expect("pairing line is ASCII")
        }
        None => line.to_string(),
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let started = Instant::now();
    let Some(path) = std::env::var_os("ACD_PAIRING_FILE") else {
        eprintln!("probe: set ACD_PAIRING_FILE to a file holding an acd pairing line");
        std::process::exit(2);
    };
    let mut line = match std::fs::read_to_string(&path) {
        Ok(text) => text.trim().to_string(),
        Err(error) => {
            eprintln!("probe: cannot read the pairing file: {}", error.kind());
            std::process::exit(2);
        }
    };
    let bad_pin = std::env::var_os("ACD_PROBE_BAD_PIN").is_some();
    if bad_pin {
        line = altered_pin(&line);
    }
    let wait = std::env::var("ACD_PROBE_WAIT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20u64);

    println!(
        "probe: os={} arch={} bad_pin={}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        bad_pin
    );

    let inbox = Inbox::new(
        Settings::fresh(),
        Arc::new(MemorySecrets::default()),
        InboxOptions {
            timing: Timing::default(),
            settings_path: None,
            client_name: "agent-caller-ios-probe".into(),
            client_version: "0.0.0-spike".into(),
        },
    );
    inbox.start();

    let entry = match inbox.import_pairing(&line) {
        Ok(entry) => entry,
        Err(error) => {
            println!("probe: import refused: {error}");
            std::process::exit(1);
        }
    };
    if let acd_client::settings::EndpointConfig::Tcp { host, port } = &entry.endpoint {
        println!("probe: imported machine endpoint host={host} port={port}");
    }

    let mut rx = inbox.view();
    let deadline = Instant::now() + Duration::from_secs(wait);
    let mut online_at: Option<Instant> = None;
    loop {
        let view = rx.borrow_and_update().clone();
        let status = view
            .machines
            .iter()
            .find(|m| m.key == entry.key)
            .map(|m| format!("{:?}", m.status));
        if let Some(s) = &status {
            if s.starts_with("Online") && online_at.is_none() {
                online_at = Some(Instant::now());
                println!("probe: status Online after {} ms", started.elapsed().as_millis());
            }
        }
        // A wrong pin never reaches Online: run the whole window, then report.
        if bad_pin && Instant::now() > deadline {
            println!("probe: last status with bad pin={status:?}");
            break;
        }
        // Give the snapshot a moment after Online to land in the view.
        if let Some(t) = online_at {
            if t.elapsed() > Duration::from_millis(1500) {
                break;
            }
        }
        if Instant::now() > deadline {
            println!("probe: timeout; last status={status:?}");
            break;
        }
        let _ = tokio::time::timeout(Duration::from_millis(250), rx.changed()).await;
    }

    let view = inbox.view().borrow().clone();
    let machines = serde_json::to_string(&view.machines).unwrap_or_default();
    println!("probe: machines={machines}");
    let cards = serde_json::to_string(&view.cards).unwrap_or_default();
    println!("probe: cards={} json={cards}", view.cards.len());

    if bad_pin {
        // Fail closed: a wrong pin must never reach Online.
        std::process::exit(if online_at.is_some() { 4 } else { 0 });
    }
    if online_at.is_some() {
        std::process::exit(0);
    }
    std::process::exit(3);
}

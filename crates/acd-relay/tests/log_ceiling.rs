//! No `RUST_LOG` directive raises the third-party ceiling (spec §8).

use std::io::Write;
use std::sync::{Arc, Mutex};

use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;

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

fn run(directives: &str) -> String {
    let buf = Buf::default();
    let sub = tracing_subscriber::registry().with(acd_relay::log_layer(directives, buf.clone()));
    tracing::subscriber::with_default(sub, || {
        tracing::debug!(target: "rustls::client::hs", "SECRET-rustls-hs-debug");
        tracing::trace!(target: "rustls", "SECRET-rustls-trace");
        tracing::info!(target: "hyper::proto::h1::conn", "SECRET-hyper-info");
        tracing::debug!(target: "hyper_util::client::legacy::connect::http", "SECRET-hyper-util-debug");
        tracing::debug!(target: "tokio_rustls::client", "SECRET-tokio-rustls-debug");
        tracing::info!(target: "axum::serve", "SECRET-axum-info");
        tracing::debug!(target: "acd_relay::server", "own-debug");
        tracing::warn!(target: "rustls::client::hs", "third-party-warn");
        tracing::error!(target: "hyper", "third-party-error");
        // A name that merely starts with a capped one is not capped.
        tracing::debug!(target: "hyperion::thing", "lookalike-debug");
    });
    String::from_utf8(buf.0.lock().unwrap().clone()).unwrap()
}

#[test]
fn specific_directives_cannot_raise_the_ceiling() {
    for directives in [
        "rustls::client::hs=debug",
        "trace",
        "rustls=trace,hyper=trace,hyper_util=trace,tokio_rustls=trace,axum=trace,acd_relay=debug",
        "hyper::proto::h1::conn=info,hyper_util::client::legacy::connect::http=debug,axum::serve=info,debug",
    ] {
        let logs = run(&format!("warn,{directives}"));
        assert!(!logs.contains("SECRET"), "{directives}: leaked:\n{logs}");
        // Warnings and errors from those targets still come through.
        assert!(logs.contains("third-party-warn") && logs.contains("third-party-error"), "{directives}:\n{logs}");
    }
    let logs = run("trace");
    assert!(logs.contains("own-debug") && logs.contains("lookalike-debug"), "{logs}");
}

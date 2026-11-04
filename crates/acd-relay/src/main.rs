//! `acd-relay`: plain HTTP behind a TLS-terminating proxy (spec §5.7).

use std::path::PathBuf;
use std::sync::Arc;

use acd_relay::config::Config;
use acd_relay::fcm::{FcmClient, SEND_TIMEOUT, ServiceAccount};
use acd_relay::http::HttpClient;
use acd_relay::server::{AppState, Options, Rates, serve};
use acd_relay::SystemClock;

const FCM_ENDPOINT: &str = "https://fcm.googleapis.com";

#[tokio::main]
async fn main() {
    let user = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into());
    tracing_subscriber::fmt().with_env_filter(acd_relay::log_filter(&user)).init();
    if let Err(e) = run().await {
        eprintln!("acd-relay: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let path = match (args.next().as_deref(), args.next()) {
        (Some("--config"), Some(p)) => PathBuf::from(p),
        _ => return Err("usage: acd-relay --config <file>".into()),
    };
    let cfg = Config::load(&path)?;
    let account = ServiceAccount::from_file(&cfg.fcm.credentials_file)?;
    let ca = std::fs::read(&cfg.fcm.ca_file).map_err(|_| "cannot read the CA file".to_string())?;
    let http = HttpClient::https(&ca, SEND_TIMEOUT, 16 * 1024).map_err(|_| "invalid CA file".to_string())?;
    let clock = Arc::new(SystemClock);
    let sender = Arc::new(FcmClient::new(http, account, FCM_ENDPOINT.into(), cfg.fcm.project_id.clone(), clock.clone()));
    let state = AppState::new(
        Options { keys: cfg.keys, allowed_apps: cfg.allowed_apps, trusted_proxy: cfg.trusted_proxy, rates: Rates::default() },
        sender,
        clock,
    );
    let listener = tokio::net::TcpListener::bind(cfg.listen).await.map_err(|e| format!("bind: {e}"))?;
    tracing::info!("listening");
    serve(listener, state).await.map_err(|e| format!("serve: {e}"))
}

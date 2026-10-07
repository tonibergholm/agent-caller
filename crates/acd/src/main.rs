mod bind;
mod config;
mod daemon;
mod identity;
mod paths;
mod secrets;
mod status;

use std::path::Path;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "acd", version, about = "agent-caller daemon")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create tokens, machine id and default config.
    Init {
        #[arg(long)]
        force: bool,
    },
    /// Run the daemon in the foreground.
    Run,
    /// Show sources, agents and open requests of the local daemon.
    Status,
    /// Token management.
    Token {
        #[command(subcommand)]
        cmd: TokenCmd,
    },
}

#[derive(Subcommand)]
enum TokenCmd {
    /// Replace a token. Running daemons close sessions that used the old one.
    Rotate { which: TokenKind },
}

#[derive(clap::ValueEnum, Clone, Copy)]
enum TokenKind { Client, Hook }

/// Targets that log WebSocket frames, including `hello.token`, at debug and
/// trace level.
const FRAME_LOGGING_TARGETS: [&str; 3] = ["tungstenite", "tokio_tungstenite", "axum::extract::ws"];

/// Caps frame-logging targets at `info`, whatever `RUST_LOG` says, so tokens
/// never reach the logs (spec §7.1).
fn below_frame_logging(meta: &tracing::Metadata<'_>) -> bool {
    let target = meta.target();
    let frames = FRAME_LOGGING_TARGETS.iter().any(|p| {
        target.strip_prefix(p).is_some_and(|rest| rest.is_empty() || rest.starts_with("::"))
    });
    !frames || *meta.level() <= tracing::Level::INFO
}

fn main() {
    use tracing_subscriber::prelude::*;
    let env = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(env)
        .with(tracing_subscriber::filter::filter_fn(below_frame_logging))
        .init();
    if let Err(e) = real_main() {
        eprintln!("acd: {e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    let dir = paths::config_dir()?;
    match cli.cmd {
        Cmd::Init { force } => init(&dir, force),
        Cmd::Token { cmd: TokenCmd::Rotate { which } } => {
            if !dir.is_dir() {
                bail!("{} not initialized; run `acd init`", dir.display());
            }
            preflight(&dir)?;
            let file = match which { TokenKind::Client => "client.token", TokenKind::Hook => "hook.token" };
            let token = secrets::new_token();
            secrets::write_secret(&dir.join(file), &token)?;
            println!("{token}");
            Ok(())
        }
        Cmd::Run => tokio::runtime::Runtime::new()?.block_on(daemon::run(&dir)),
        Cmd::Status => tokio::runtime::Runtime::new()?.block_on(status::status(&dir)),
    }
}

/// Checks file modes before the daemon reads anything.
pub(crate) fn preflight(dir: &Path) -> Result<()> {
    secrets::check_mode(dir, 0o700)?;
    for f in ["client.token", "hook.token", "config.toml"] {
        let p = dir.join(f);
        if p.exists() { secrets::check_mode(&p, 0o600)?; }
    }
    Ok(())
}

fn init(dir: &Path, force: bool) -> Result<()> {
    const FILES: [&str; 4] = ["client.token", "hook.token", "machine-id", "config.toml"];
    if FILES.iter().all(|f| dir.join(f).exists()) && !force {
        bail!("{} is already initialized; use --force to regenerate tokens", dir.display());
    }
    secrets::ensure_private_dir(dir)?;
    let mut created = Vec::new();
    let mut client = None;
    for (file, is_client) in [("client.token", true), ("hook.token", false)] {
        let path = dir.join(file);
        if force || !path.exists() {
            let token = secrets::new_token();
            secrets::write_secret(&path, &token)?;
            created.push(file);
            if is_client { client = Some(token); }
        }
    }
    let id_path = dir.join("machine-id");
    let id = if id_path.exists() {
        identity::machine_id(dir)?
    } else {
        created.push("machine-id");
        identity::create_machine_id(dir)?
    };
    let cfg_path = dir.join("config.toml");
    if !cfg_path.exists() {
        secrets::write_secret(&cfg_path, &toml::to_string(&config::Config::default())?)?;
        created.push("config.toml");
    }
    println!("Initialized acd in {}", dir.display());
    println!("created: {}", created.join(", "));
    println!("machine id: {id}");
    if let Some(client) = client {
        println!("client token (add it to the Mac app): {client}");
    }
    Ok(())
}

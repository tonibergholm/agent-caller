mod config;
mod identity;
mod paths;
mod secrets;

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

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .init();
    if let Err(e) = real_main() {
        eprintln!("acd: {e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    let dir = paths::config_dir();
    match cli.cmd {
        Cmd::Init { force } => init(&dir, force),
        Cmd::Token { cmd: TokenCmd::Rotate { which } } => {
            let file = match which { TokenKind::Client => "client.token", TokenKind::Hook => "hook.token" };
            let token = secrets::new_token();
            secrets::write_secret(&dir.join(file), &token)?;
            println!("{token}");
            Ok(())
        }
        Cmd::Run => {
            preflight(&dir)?;
            bail!("`acd run` is implemented in Task 10")
        }
        Cmd::Status => bail!("`acd status` is implemented in Task 10"),
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
    if dir.join("client.token").exists() && !force {
        bail!("{} is already initialized; use --force to regenerate tokens", dir.display());
    }
    secrets::ensure_private_dir(dir)?;
    let client = secrets::new_token();
    secrets::write_secret(&dir.join("client.token"), &client)?;
    secrets::write_secret(&dir.join("hook.token"), &secrets::new_token())?;
    let id = identity::create_machine_id(dir)?;
    let cfg_path = dir.join("config.toml");
    if !cfg_path.exists() {
        secrets::write_secret(&cfg_path, &toml::to_string(&config::Config::default())?)?;
    }
    println!("Initialized acd in {}", dir.display());
    println!("machine id: {id}");
    println!("client token (add it to the Mac app): {client}");
    Ok(())
}

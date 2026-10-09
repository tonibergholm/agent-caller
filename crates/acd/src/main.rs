mod bind;
mod config;
mod daemon;
mod hook_shim;
mod hook_token;
mod hooks_install;
mod identity;
mod local_socket;
mod pair;
mod paths;
mod secrets;
mod service;
mod status;
mod tls;
mod writer_lock;

use std::path::Path;

use anyhow::{Context, Result, bail};
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
    /// TLS key management.
    Tls {
        #[command(subcommand)]
        cmd: TlsCmd,
    },
    /// Print a pairing line for a client app.
    Pair {
        #[arg(long)]
        host: Option<String>,
    },
    /// Install or remove acd's Claude Code or Codex hooks.
    Hooks {
        #[command(subcommand)]
        cmd: HooksCmd,
    },
    /// Install acd as a launchd agent or systemd user service.
    Service {
        #[command(subcommand)]
        cmd: ServiceCmd,
    },
}

#[derive(Subcommand)]
enum ServiceCmd {
    Install {
        /// Print the service definition instead of installing it.
        #[arg(long)]
        print: bool,
        /// Replace a service file that acd did not write (a backup is kept).
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum HooksCmd {
    Install {
        target: hooks_install::Target,
        #[arg(long)]
        allow_tracked: bool,
        #[arg(long)]
        allow_unknown_git: bool,
    },
    Uninstall {
        target: hooks_install::Target,
    },
    /// Report the install records and any unresolved interrupted command.
    Status,
}

#[derive(Subcommand)]
enum TlsCmd {
    /// Replace the TLS key and certificate.
    Rotate,
}

#[derive(Subcommand)]
enum TokenCmd {
    /// Replace a token. Running daemons close sessions that used the old one.
    Rotate {
        which: TokenKind,
        #[arg(long)]
        allow_tracked: bool,
        #[arg(long)]
        allow_unknown_git: bool,
        #[arg(long)]
        force: bool,
    },
}

#[derive(clap::ValueEnum, Clone, Copy)]
enum TokenKind {
    Client,
    Hook,
}

/// Targets that log WebSocket frames, including `hello.token`, at debug and
/// trace level.
const FRAME_LOGGING_TARGETS: [&str; 3] = ["tungstenite", "tokio_tungstenite", "axum::extract::ws"];

/// Caps frame-logging targets at `info`, whatever `RUST_LOG` says, so tokens
/// never reach the logs (spec §7.1).
fn below_frame_logging(meta: &tracing::Metadata<'_>) -> bool {
    let target = meta.target();
    let frames = FRAME_LOGGING_TARGETS.iter().any(|p| {
        target
            .strip_prefix(p)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with("::"))
    });
    !frames || *meta.level() <= tracing::Level::INFO
}

fn main() {
    // The Codex shim runs before logging starts, so it never writes log
    // lines to Codex's stderr.
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if args.get(1).is_some_and(|a| a == "hook") {
        std::process::exit(hook_shim::main(&args[2..]));
    }
    use tracing_subscriber::prelude::*;
    let env =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
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
        Cmd::Token {
            cmd:
                TokenCmd::Rotate {
                    which,
                    allow_tracked,
                    allow_unknown_git,
                    force,
                },
        } => {
            if !dir.is_dir() {
                bail!("{} not initialized; run `acd init`", dir.display());
            }
            preflight(&dir)?;
            match which {
                TokenKind::Client => {
                    let token = secrets::new_token();
                    secrets::write_secret(&dir.join("client.token"), &token)?;
                    println!("{token}");
                    Ok(())
                }
                TokenKind::Hook => {
                    let ctx = hooks_install::Ctx::from_env(&dir)?;
                    let flags = hooks_install::Flags {
                        allow_tracked,
                        allow_unknown_git,
                    };
                    let (store, report) = hook_token::rotate(
                        &dir,
                        &ctx,
                        flags,
                        force,
                        chrono::Utc::now().timestamp_millis(),
                    )?;
                    for path in &report.updated {
                        println!("Updated {}", path.display());
                    }
                    if !report.failed.is_empty() {
                        for (path, why) in &report.failed {
                            eprintln!("could not update {}: {why}", path.display());
                        }
                        bail!(
                            "hook token rotation incomplete: fix the file(s) and run `acd hooks install claude`"
                        );
                    }
                    println!(
                        "Hook token rotated to generation {}. The previous token stays valid until every Claude session started before now has exited or used the new token, at most 30 days.",
                        store.generation
                    );
                    Ok(())
                }
            }
        }
        Cmd::Tls {
            cmd: TlsCmd::Rotate,
        } => {
            if !dir.is_dir() {
                bail!("{} not initialized; run `acd init`", dir.display());
            }
            preflight_prerequisites(&dir)?;
            tls::rotate(&dir)?;
            println!(
                "TLS key replaced. Re-pair every app with `acd pair`. If you rotated because the key may be compromised, also run `acd token rotate client`."
            );
            Ok(())
        }
        Cmd::Pair { host } => {
            if !dir.is_dir() {
                bail!("{} not initialized; run `acd init`", dir.display());
            }
            tokio::runtime::Runtime::new()?.block_on(pair::print(&dir, host))
        }
        Cmd::Hooks { cmd } => {
            if !dir.is_dir() {
                bail!("{} not initialized; run `acd init`", dir.display());
            }
            preflight(&dir)?;
            let ctx = hooks_install::Ctx::from_env(&dir)?;
            match cmd {
                HooksCmd::Install {
                    target,
                    allow_tracked,
                    allow_unknown_git,
                } => {
                    let r = hooks_install::install(
                        &ctx,
                        target,
                        hooks_install::Flags {
                            allow_tracked,
                            allow_unknown_git,
                        },
                    )?;
                    if let Some(n) = &r.recovery_note {
                        eprintln!("note: {n}");
                    }
                    if r.suspicious > 0 {
                        eprintln!(
                            "warning: {} unexpected entries in {} carry the acd marker; acd left them alone",
                            r.suspicious,
                            r.path.display()
                        );
                    }
                    println!("Installed acd hooks in {}", r.path.display());
                    if let Some(b) = r.backup {
                        println!("Backup: {}", b.display());
                    }
                    match target {
                        hooks_install::Target::Claude => {
                            println!(
                                "Claude reads hooks when a session starts: restart running sessions or run /clear in them."
                            );
                            println!(
                                "Trust note: Claude sends its hooks, with the hook token, to 127.0.0.1:{}. While acd is not running, another local user or process can take that port, read the token and payloads, and approve Claude permission prompts. Use acd v1 on personal, trusted hosts only.",
                                ctx.hook_port
                            );
                        }
                        hooks_install::Target::Codex => println!(
                            "Codex may ask you to review these hooks the first time it runs them. Codex sessions that share Codex's background app-server can be answered from the inbox only when acd can tell which herdr pane they run in; otherwise their prompts are shown read-only."
                        ),
                    }
                }
                HooksCmd::Status => {
                    print!("{}", hooks_install::status(&ctx)?);
                    if let Some(w) = status::hook_port_warning(&dir, ctx.hook_port) {
                        eprintln!("warning: {w}");
                    }
                }
                HooksCmd::Uninstall { target } => {
                    let r = hooks_install::uninstall(&ctx, target)?;
                    if let Some(n) = &r.recovery_note {
                        eprintln!("note: {n}");
                    }
                    if r.removed_file {
                        println!("Removed {} (acd created it)", r.path.display());
                    } else {
                        println!("Removed acd hooks from {}", r.path.display());
                    }
                }
            }
            Ok(())
        }
        Cmd::Service {
            cmd: ServiceCmd::Install { print, force },
        } => {
            if !dir.is_dir() {
                bail!("{} not initialized; run `acd init`", dir.display());
            }
            preflight(&dir)?;
            let home =
                std::path::PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
            let exe = service::invoked_exe()?;
            let dir = std::fs::canonicalize(&dir)?;
            println!(
                "{}",
                service::install(&home, &exe, &dir, print, force, service::runner().as_ref())?
            );
            Ok(())
        }
        Cmd::Run => {
            let rt = tokio::runtime::Runtime::new()?;
            let result = rt.block_on(daemon::run(&dir));
            // Blocking tasks (registry scans, path resolution) must not
            // delay the exit.
            rt.shutdown_timeout(std::time::Duration::from_millis(500));
            result
        }
        Cmd::Status => tokio::runtime::Runtime::new()?.block_on(status::status(&dir)),
    }
}

/// Checks file modes before the daemon reads anything.
pub(crate) fn preflight(dir: &Path) -> Result<()> {
    preflight_prerequisites(dir)?;
    tls::load(dir).context("TLS bundle is invalid or unsafe; run `acd init` to repair it")?;
    Ok(())
}

fn preflight_prerequisites(dir: &Path) -> Result<()> {
    secrets::check_mode(dir, 0o700)?;
    for f in [
        "client.token",
        "hook.token",
        "hook-tokens.json",
        "hooks-installed.json",
        "hooks-journal.json",
        "t3.token",
        "config.toml",
    ] {
        let p = dir.join(f);
        if p.symlink_metadata().is_ok() {
            secrets::check_mode(&p, 0o600)?;
        }
    }
    Ok(())
}

fn init(dir: &Path, force: bool) -> Result<()> {
    secrets::ensure_private_dir(dir)?;
    let mut created = Vec::new();
    let mut repaired = Vec::new();
    for file in [
        "client.token",
        "t3.token",
        "machine-id",
        "config.toml",
        hook_token::STORE_FILE,
    ] {
        let p = dir.join(file);
        if p.symlink_metadata().is_ok() && secrets::repair_mode(&p, 0o600)? {
            repaired.push(file);
        }
    }
    let had_tls = dir.join("tls.pem").exists();
    let mut client = None;
    let file = "client.token";
    let path = dir.join(file);
    if force || path.symlink_metadata().is_err() {
        let token = secrets::new_token();
        secrets::write_secret(&path, &token)?;
        created.push(file);
        client = Some(token);
    }
    if hook_token::ensure(dir, force)? {
        created.push(hook_token::STORE_FILE);
    }
    let id_path = dir.join("machine-id");
    let id = match identity::machine_id(dir) {
        Ok(id) => id,
        Err(_) if id_path.symlink_metadata().is_err() || identity::machine_id_is_empty(dir) => {
            created.push("machine-id");
            identity::create_machine_id(dir)?
        }
        Err(e) => return Err(e),
    };
    let cfg_path = dir.join("config.toml");
    if cfg_path.symlink_metadata().is_err() {
        let mut cfg = config::Config::default();
        cfg.hook_port = config::random_free_port(cfg.port)?;
        secrets::write_secret(&cfg_path, &toml::to_string(&cfg)?)?;
        created.push("config.toml");
    }
    tls::ensure(dir)?;
    if !had_tls {
        created.push("tls.pem");
    }
    println!("Initialized acd in {}", dir.display());
    println!("created: {}", created.join(", "));
    if !repaired.is_empty() {
        println!("repaired: {}", repaired.join(", "));
    }
    println!("machine id: {id}");
    if let Some(client) = client {
        println!("client token (add it to the Mac app): {client}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn preflight_rejects_a_t3_token_looser_than_0600() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let token = dir.path().join("t3.token");
        std::fs::write(&token, "token").unwrap();
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o640)).unwrap();
        let err = preflight(dir.path()).unwrap_err().to_string();
        assert!(err.contains("t3.token"), "{err}");
    }
}

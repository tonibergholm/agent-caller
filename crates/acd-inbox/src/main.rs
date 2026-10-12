use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "acd-inbox", version, about = "The agent-caller inbox service for Omarchy")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the service (the systemd user unit starts this).
    Serve,
    /// Install the service, the shell plugin and, if you agree, a keybinding.
    Install,
    /// Remove the service and the plugin.
    Uninstall,
    /// Report the service, the socket, the plugin and the keyring.
    Status,
}

fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Command::Serve => acd_inbox::serve::run(),
        Command::Install => acd_inbox::install::run_install(),
        Command::Uninstall => acd_inbox::install::run_uninstall(),
        Command::Status => acd_inbox::install::run_status(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("acd-inbox: {e:#}");
            ExitCode::FAILURE
        }
    }
}

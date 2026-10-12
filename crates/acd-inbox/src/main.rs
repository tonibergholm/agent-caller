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
}

fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Command::Serve => acd_inbox::serve::run(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("acd-inbox: {e:#}");
            ExitCode::FAILURE
        }
    }
}

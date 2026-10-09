use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use acd_protocol::pairing::Pairing;
use anyhow::{Context, Result, bail};

use crate::{config::Config, secrets, tls};

pub async fn print(dir: &Path, requested_host: Option<String>) -> Result<()> {
    let host = match requested_host {
        Some(host) if !host.trim().is_empty() => host,
        Some(_) => bail!("pass --host with a non-empty host"),
        None => discover_host()
            .await
            .context("could not find a Tailscale host; pass --host")?,
    };
    let cfg = Config::load(dir)?;
    let (_, pin) = tls::load(dir)?;
    let token_path = dir.join("client.token");
    let token = acd_securefs::read_private(&token_path, 0o600, 4096)
        .with_context(|| format!("reading {}", token_path.display()))?;
    let token = secrets::parse_token(std::str::from_utf8(&token)?)?;
    let line = Pairing {
        host,
        port: cfg.port,
        pin,
        token,
    }
    .to_line();
    Pairing::parse(&line).context("invalid pairing host")?;
    println!("{line}");
    eprintln!("This pairing line contains a secret. Share it only with the intended app.");
    Ok(())
}

async fn discover_host() -> Result<String> {
    let Some(path) = crate::bind::tailscale_cli() else {
        bail!(
            "Tailscale CLI not found (looked for: {})",
            crate::bind::tailscale_cli_searched()
        )
    };
    let mut cmd = tokio::process::Command::new(path);
    cmd.args(["status", "--json"])
        .kill_on_drop(true)
        .stdin(Stdio::null());
    if let Ok(Ok(out)) = tokio::time::timeout(Duration::from_secs(2), cmd.output()).await
        && out.status.success()
    {
        let value: serde_json::Value = serde_json::from_slice(&out.stdout)?;
        if let Some(name) = value
            .get("Self")
            .and_then(|this| this.get("DNSName"))
            .and_then(serde_json::Value::as_str)
        {
            let name = name.trim_end_matches('.');
            if !name.is_empty() {
                return Ok(name.to_string());
            }
        }
        if let Some(ip) = value
            .get("Self")
            .and_then(|this| this.get("TailscaleIPs"))
            .and_then(serde_json::Value::as_array)
            .and_then(|ips| ips.iter().find_map(serde_json::Value::as_str))
        {
            return Ok(ip.to_string());
        }
    }
    bail!("Tailscale did not report a host")
}

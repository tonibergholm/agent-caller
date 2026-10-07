use std::net::IpAddr;

use anyhow::{bail, Result};

/// 100.64.0.0/10 or fd7a:115c:a1e0::/48.
pub fn in_tailscale_range(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => { let o = v4.octets(); o[0] == 100 && (o[1] & 0xC0) == 0x40 }
        IpAddr::V6(v6) => { let s = v6.segments(); s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0 }
    }
}

/// The addresses the local Tailscale daemon reports. `None` means the probe
/// failed or timed out (Tailscale missing, daemon down, hung); callers must not
/// treat that as "no addresses".
pub async fn tailscale_reported() -> Option<Vec<IpAddr>> {
    let mut cmd = tokio::process::Command::new("tailscale");
    cmd.arg("ip").kill_on_drop(true).stdin(std::process::Stdio::null());
    let out = tokio::time::timeout(std::time::Duration::from_secs(5), cmd.output()).await.ok()?.ok()?;
    if !out.status.success() { return None; }
    Some(String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.trim().parse().ok()).collect())
}

/// Debounce for the address watcher. Returns `(exit_now, pending)`. The
/// watcher exits only when the same changed set is seen on two consecutive
/// probes; a probe matching the bound set clears the pending change.
pub fn should_exit(
    prev_pending: Option<&Vec<IpAddr>>,
    current: &Vec<IpAddr>,
    bound: &Vec<IpAddr>,
) -> (bool, Option<Vec<IpAddr>>) {
    if current == bound { return (false, None); }
    if prev_pending == Some(current) { return (true, Some(current.clone())); }
    (false, Some(current.clone()))
}

/// Expands `listen` entries into addresses allowed by spec §7.1.
pub fn resolve_listen(listen: &[String], reported: &[IpAddr]) -> Result<Vec<IpAddr>> {
    let mut out: Vec<IpAddr> = Vec::new();
    let mut push = |ip: IpAddr| if !out.contains(&ip) { out.push(ip) };
    for entry in listen {
        match entry.as_str() {
            "loopback" => { push("127.0.0.1".parse().unwrap()); push("::1".parse().unwrap()); }
            "tailscale" => {
                for ip in reported.iter().filter(|ip| in_tailscale_range(ip)) { push(*ip); }
            }
            other => {
                let ip: IpAddr = other.parse().map_err(|_| anyhow::anyhow!("invalid listen entry {other:?}"))?;
                let allowed = ip.is_loopback() || (in_tailscale_range(&ip) && reported.contains(&ip));
                if !allowed {
                    bail!("refusing to listen on {ip}: only loopback and this machine's Tailscale addresses are allowed");
                }
                push(ip);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr { s.parse().unwrap() }

    #[test]
    fn tailscale_ranges() {
        assert!(in_tailscale_range(&ip("100.64.0.1")));
        assert!(in_tailscale_range(&ip("100.127.255.254")));
        assert!(!in_tailscale_range(&ip("100.128.0.1")));
        assert!(!in_tailscale_range(&ip("192.168.1.10")));
        assert!(in_tailscale_range(&ip("fd7a:115c:a1e0::2f01:9db1")));
        assert!(!in_tailscale_range(&ip("fd7a:115c:a1e1::1")));
    }

    #[test]
    fn loopback_and_reported_tailscale_addresses_are_allowed() {
        let reported = [ip("100.123.157.75"), ip("fd7a:115c:a1e0::2f01:9db1")];
        let got = resolve_listen(&["loopback".into(), "tailscale".into()], &reported).unwrap();
        assert_eq!(got, vec![ip("127.0.0.1"), ip("::1"), reported[0], reported[1]]);
    }

    #[test]
    fn explicit_addresses_must_be_loopback_or_reported_tailscale() {
        let reported = [ip("100.123.157.75")];
        assert!(resolve_listen(&["100.123.157.75".into()], &reported).is_ok());
        assert!(resolve_listen(&["127.0.0.1".into()], &reported).is_ok());
        let unreported = resolve_listen(&["100.100.100.100".into()], &reported).unwrap_err().to_string();
        assert!(unreported.contains("refusing to listen on 100.100.100.100"));
        assert!(resolve_listen(&["0.0.0.0".into()], &reported).is_err());
        assert!(resolve_listen(&["192.168.1.10".into()], &reported).is_err());
    }

    #[test]
    fn address_change_must_be_seen_twice_to_exit() {
        let bound = vec![ip("127.0.0.1"), ip("100.1.1.1")];
        let changed = vec![ip("127.0.0.1")];
        let other = vec![ip("127.0.0.1"), ip("100.2.2.2")];
        assert_eq!(should_exit(None, &bound, &bound), (false, None));
        assert_eq!(should_exit(None, &changed, &bound), (false, Some(changed.clone())));
        assert_eq!(should_exit(Some(&changed), &changed, &bound), (true, Some(changed.clone())));
        assert_eq!(should_exit(Some(&changed), &other, &bound), (false, Some(other.clone())));
        assert_eq!(should_exit(Some(&changed), &bound, &bound), (false, None));
    }

    #[test]
    fn reported_addresses_outside_tailscale_ranges_are_ignored() {
        let got = resolve_listen(&["tailscale".into()], &[ip("10.0.0.5")]).unwrap();
        assert!(got.is_empty());
    }
}

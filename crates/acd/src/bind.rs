use std::ffi::OsStr;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

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
    let mut cmd = tokio::process::Command::new(tailscale_cli()?);
    cmd.arg("ip").kill_on_drop(true).stdin(std::process::Stdio::null());
    let out = tokio::time::timeout(std::time::Duration::from_secs(5), cmd.output()).await.ok()?.ok()?;
    if !out.status.success() { return None; }
    Some(String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.trim().parse().ok()).collect())
}

/// Where the macOS GUI app bundles the Tailscale CLI; it is usually not on PATH.
#[cfg(target_os = "macos")]
const FALLBACK_CLIS: &[&str] = &["/Applications/Tailscale.app/Contents/MacOS/Tailscale"];
#[cfg(not(target_os = "macos"))]
const FALLBACK_CLIS: &[&str] = &[];

/// The `tailscale` CLI: an absolute PATH entry first, then known app-bundle paths.
pub fn tailscale_cli() -> Option<PathBuf> {
    find_cli(std::env::var_os("PATH").as_deref(), FALLBACK_CLIS, Path::is_file)
}

/// Every location `tailscale_cli` looks at, for diagnostics.
pub fn tailscale_cli_searched() -> String {
    let mut parts = vec!["tailscale on PATH".to_string()];
    parts.extend(FALLBACK_CLIS.iter().map(|p| (*p).to_string()));
    parts.join(", ")
}

fn find_cli(path: Option<&OsStr>, fallbacks: &[&str], is_file: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    path.into_iter()
        .flat_map(std::env::split_paths)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("tailscale"))
        .chain(fallbacks.iter().map(PathBuf::from))
        .find(|candidate| is_file(candidate))
}

/// Consecutive failed probes after which acd exits if it has a Tailscale
/// address bound (about 5 minutes at the 30 s probe interval).
pub const MAX_FAILED_PROBES: u32 = 10;

/// Next consecutive-failure count: a successful probe resets the streak.
pub fn next_failure_count(prev: u32, probe_ok: bool) -> u32 {
    if probe_ok { 0 } else { prev.saturating_add(1) }
}

/// Failure-streak rule for the address watcher. `failures` is the streak
/// including the current failed probe. Exits only when a Tailscale address is
/// bound and the streak reaches `MAX_FAILED_PROBES`.
pub fn should_exit_on_probe_failures(failures: u32, bound: &[IpAddr]) -> bool {
    failures >= MAX_FAILED_PROBES && bound.iter().any(in_tailscale_range)
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

/// Whether any `listen` entry depends on `tailscale ip`: the `tailscale`
/// keyword or an explicit address in the Tailscale ranges.
pub fn needs_tailscale_probe(listen: &[String]) -> bool {
    listen.iter().any(|e| {
        e == "tailscale" || e.parse::<IpAddr>().is_ok_and(|ip| in_tailscale_range(&ip))
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn probe_needed_for_the_keyword_or_an_explicit_tailscale_address() {
        let l = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(needs_tailscale_probe(&l(&["loopback", "tailscale"])));
        assert!(needs_tailscale_probe(&l(&["loopback", "100.101.102.103"])));
        assert!(needs_tailscale_probe(&l(&["fd7a:115c:a1e0::1"])));
        assert!(!needs_tailscale_probe(&l(&["loopback"])));
        assert!(!needs_tailscale_probe(&l(&["127.0.0.1", "::1"])));
    }

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
    fn probe_failure_streak_exits_only_with_tailscale_bound() {
        let ts = vec![ip("127.0.0.1"), ip("100.100.1.1")];
        let lo = vec![ip("127.0.0.1")];
        assert!(!should_exit_on_probe_failures(9, &ts));
        assert!(should_exit_on_probe_failures(10, &ts));
        assert!(!should_exit_on_probe_failures(1000, &lo));
        // A success breaks the streak.
        let mut n = 0;
        for _ in 0..9 { n = next_failure_count(n, false); }
        assert!(!should_exit_on_probe_failures(n, &ts));
        n = next_failure_count(n, true);
        assert_eq!(n, 0);
        n = next_failure_count(n, false);
        assert!(!should_exit_on_probe_failures(n, &ts));
    }

    #[test]
    fn reported_addresses_outside_tailscale_ranges_are_ignored() {
        let got = resolve_listen(&["tailscale".into()], &[ip("10.0.0.5")]).unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn cli_path_hit_wins_over_fallback() {
        let path = std::env::join_paths(["/nope", "/opt/bin"]).unwrap();
        let got = find_cli(Some(&path), &["/app/Tailscale"], |p| {
            p == Path::new("/opt/bin/tailscale") || p == Path::new("/app/Tailscale")
        });
        assert_eq!(got, Some(PathBuf::from("/opt/bin/tailscale")));
    }

    #[test]
    fn cli_path_miss_falls_back_to_app_path() {
        let path = std::env::join_paths(["/nope"]).unwrap();
        let got = find_cli(Some(&path), &["/app/Tailscale"], |p| p == Path::new("/app/Tailscale"));
        assert_eq!(got, Some(PathBuf::from("/app/Tailscale")));
    }

    #[test]
    fn cli_nothing_found_is_none() {
        let path = std::env::join_paths(["/nope"]).unwrap();
        assert_eq!(find_cli(Some(&path), &["/app/Tailscale"], |_| false), None);
        assert_eq!(find_cli(None, &[], |_| true), None);
    }

    #[test]
    fn cli_relative_path_entries_are_ignored() {
        let path = std::env::join_paths(["relative/bin", "."]).unwrap();
        let got = find_cli(Some(&path), &[], |p| p.ends_with("tailscale"));
        assert_eq!(got, None);
    }
}

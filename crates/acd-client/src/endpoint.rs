//! Where a machine's acd lives, and the Tailscale address rule (spec §7.1).
//! The range check is not authentication (100.64/10 is also CGNAT space);
//! the D1 handshake authenticates the server.
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Endpoint {
    Local {
        config_dir: PathBuf,
    },
    /// Always wss, verified against `pin` (decision D1).
    Tcp {
        host: String,
        port: u16,
        pin: String,
    },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EndpointError {
    #[error("cannot resolve {0}")]
    Resolve(String),
    #[error("{host} resolves to {addrs:?}; none is a Tailscale or loopback address")]
    NotTailscale { host: String, addrs: Vec<IpAddr> },
}

pub fn allowed_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback() || (v4.octets()[0] == 100 && v4.octets()[1] & 0xC0 == 0x40)
        }
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => allowed_ip(IpAddr::V4(v4)),
            None => v6.is_loopback() || v6.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
        },
    }
}

/// Whether `host` is a literal address the connect-time rule would refuse.
/// Names are not refused here: `vet` checks the addresses they resolve to.
pub fn refuses_literal(host: &str) -> bool {
    host.parse::<IpAddr>().is_ok_and(|ip| !allowed_ip(ip))
}

/// Resolves once and keeps only allowed addresses. Callers connect to the
/// returned addresses and never resolve the name again.
pub async fn vet(host: &str, port: u16) -> Result<Vec<SocketAddr>, EndpointError> {
    let all: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| EndpointError::Resolve(host.to_string()))?
        .collect();
    let ok: Vec<SocketAddr> = all.iter().copied().filter(|a| allowed_ip(a.ip())).collect();
    if ok.is_empty() {
        return Err(EndpointError::NotTailscale {
            host: host.to_string(),
            addrs: all.iter().map(|a| a.ip()).collect(),
        });
    }
    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn tailscale_and_loopback_only() {
        for ok in [
            "100.64.0.1",
            "100.127.255.255",
            "100.100.100.100",
            "127.0.0.1",
            "::1",
            "fd7a:115c:a1e0::1",
            "fd7a:115c:a1e0:ab12::1",
            "::ffff:100.101.1.1",
        ] {
            assert!(allowed_ip(ip(ok)), "{ok}");
        }
        for bad in [
            "100.63.255.255",
            "100.128.0.0",
            "10.0.0.1",
            "192.168.1.10",
            "8.8.8.8",
            "fd7a:115c:a1e1::1",
            "fe80::1",
            "::ffff:192.168.1.1",
        ] {
            assert!(!allowed_ip(ip(bad)), "{bad}");
        }
    }

    #[tokio::test]
    async fn vet_filters_and_refuses() {
        assert!(
            vet("127.0.0.1", 47800)
                .await
                .unwrap()
                .iter()
                .all(|a| a.ip().is_loopback())
        );
        assert!(matches!(
            vet("192.0.2.1", 47800).await,
            Err(EndpointError::NotTailscale { .. })
        ));
        assert!(matches!(
            vet("no-such-host.invalid", 47800).await,
            Err(EndpointError::Resolve(_))
        ));
    }
}

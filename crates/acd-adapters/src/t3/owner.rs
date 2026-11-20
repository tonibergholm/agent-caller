//! Proves, per connection, that the server end of a loopback T3 connection
//! belongs to the daemon's own user, before the token is written to it.
//!
//! Any local user can bind a free loopback port, so a bearer token sent to
//! "whatever answers on 127.0.0.1:<port>" can be taken (security review
//! local F2). The check looks at this exact connection: the socket whose
//! local address is the server's and whose remote address is our local one.
//! Not found, unreadable or another uid all refuse (fail closed).
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
#[cfg(target_os = "macos")]
use std::time::Duration;

use tokio::net::TcpStream;

/// True when `tcp` is a connection to a loopback address.
pub fn is_loopback(tcp: &TcpStream) -> bool {
    tcp.peer_addr().is_ok_and(|a| a.ip().to_canonical().is_loopback())
}

/// True only when the process holding the server end of `tcp` runs as this
/// user. Call it after connect and before writing anything.
pub async fn server_is_ours(tcp: &TcpStream) -> bool {
    let (Ok(client), Ok(server)) = (tcp.local_addr(), tcp.peer_addr()) else {
        return false;
    };
    let (client, server) = (canonical(client), canonical(server));
    // SAFETY: geteuid has no preconditions.
    let ours = unsafe { libc::geteuid() };
    server_uid(server, client).await == Some(ours)
}

fn canonical(a: SocketAddr) -> SocketAddr {
    SocketAddr::new(a.ip().to_canonical(), a.port())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
async fn server_uid(server: SocketAddr, client: SocketAddr) -> Option<u32> {
    for file in ["/proc/net/tcp", "/proc/net/tcp6"] {
        // /proc is memory-backed: a plain read is quicker than a blocking-pool hop.
        if let Ok(text) = std::fs::read_to_string(file)
            && let Some(uid) = proc_net_uid(&text, server, client)
        {
            return Some(uid);
        }
    }
    None
}

/// macOS: `lsof` run as a normal user lists only that user's processes, so a
/// server end owned by someone else is simply absent, and absent refuses.
#[cfg(target_os = "macos")]
async fn server_uid(server: SocketAddr, client: SocketAddr) -> Option<u32> {
    let run = tokio::process::Command::new("/usr/sbin/lsof")
        .args(["-nP", "-F", "pun"])
        .arg(format!("-iTCP:{}", client.port()))
        .env_clear()
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .ok()?
        .ok()?;
    lsof_uid(&String::from_utf8_lossy(&out.stdout), server, client)
}

/// Other platforms have no way to look: refuse.
#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
async fn server_uid(_server: SocketAddr, _client: SocketAddr) -> Option<u32> {
    None
}

/// Decodes one `/proc/net/tcp{,6}` address (`0100007F:0CBD`). The address
/// words are printed in host byte order.
fn proc_addr(s: &str) -> Option<SocketAddr> {
    let (ip, port) = s.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let ip = match ip.len() {
        8 => IpAddr::V4(Ipv4Addr::from(
            u32::from_str_radix(ip, 16).ok()?.to_ne_bytes(),
        )),
        32 => {
            let mut b = [0u8; 16];
            for i in 0..4 {
                let w = u32::from_str_radix(&ip[i * 8..i * 8 + 8], 16).ok()?;
                b[i * 4..i * 4 + 4].copy_from_slice(&w.to_ne_bytes());
            }
            IpAddr::V6(Ipv6Addr::from(b))
        }
        _ => return None,
    };
    Some(canonical(SocketAddr::new(ip, port)))
}

/// The uid column of the row whose local address is `server` and whose remote
/// address is `client`.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn proc_net_uid(text: &str, server: SocketAddr, client: SocketAddr) -> Option<u32> {
    text.lines().skip(1).find_map(|line| {
        let mut f = line.split_whitespace();
        let (local, remote) = (f.nth(1)?, f.next()?);
        if proc_addr(local)? != server || proc_addr(remote)? != client {
            return None;
        }
        // Columns after remote: st, tx:rx, tr:tm, retrnsmt, uid.
        f.nth(4)?.parse().ok()
    })
}

/// The uid (`u`) of the process whose open socket is named
/// `<server>-><client>` in `lsof -F pun` output.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn lsof_uid(text: &str, server: SocketAddr, client: SocketAddr) -> Option<u32> {
    let want = format!("{server}->{client}");
    let mut uid = None;
    for line in text.lines() {
        match line.split_at_checked(1)? {
            ("p", _) => uid = None,
            ("u", v) => uid = v.parse().ok(),
            ("n", name) if name == want => return uid,
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROC4: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 0100007F:0ECD 0100007F:CB20 01 00000000:00000000 00:00000000 00000000  1000        0 5555 1 0000000000000000 100 0 0 10 0\n\
   1: 0100007F:CB20 0100007F:0ECD 01 00000000:00000000 00:00000000 00000000  1001        0 5556 1 0000000000000000 100 0 0 10 0\n";

    fn sa(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[cfg(target_endian = "little")]
    #[test]
    fn proc_net_tcp_picks_the_server_row_not_the_client_row() {
        // Row 0 is the server end (local 127.0.0.1:3789), row 1 the client.
        let (server, client) = (sa("127.0.0.1:3789"), sa("127.0.0.1:52000"));
        assert_eq!(proc_net_uid(PROC4, server, client), Some(1000));
        assert_eq!(proc_net_uid(PROC4, client, server), Some(1001));
        assert_eq!(proc_net_uid(PROC4, server, sa("127.0.0.1:52001")), None);
    }

    #[cfg(target_endian = "little")]
    #[test]
    fn proc_net_tcp6_reads_loopback_and_mapped_v4() {
        let text = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 00000000000000000000000001000000:0ECD 00000000000000000000000001000000:CB20 01 00000000:00000000 00:00000000 00000000  1234        0 1 1 0 100 0 0 10 0\n\
   1: 0000000000000000FFFF00000100007F:0ECD 0000000000000000FFFF00000100007F:CB21 01 00000000:00000000 00:00000000 00000000  4321        0 2 1 0 100 0 0 10 0\n";
        assert_eq!(proc_net_uid(text, sa("[::1]:3789"), sa("[::1]:52000")), Some(1234));
        // A dual-stack server seen through ::ffff:127.0.0.1 matches the v4 pair.
        let (s, c) = (canonical(sa("127.0.0.1:3789")), canonical(sa("127.0.0.1:52001")));
        assert_eq!(proc_net_uid(text, s, c), Some(4321));
    }

    #[test]
    fn proc_net_tcp_garbage_is_not_found() {
        let (s, c) = (sa("127.0.0.1:1"), sa("127.0.0.1:2"));
        assert_eq!(proc_net_uid("", s, c), None);
        assert_eq!(proc_net_uid("header\nnot a row\n0: zz:1 zz:2 01 0 0 0 0 5", s, c), None);
    }

    #[test]
    fn lsof_picks_the_process_holding_the_server_side() {
        let out = "p100\nu501\nn127.0.0.1:52000->127.0.0.1:3789\np200\nu502\nn127.0.0.1:3789->127.0.0.1:52000\n";
        let (server, client) = (sa("127.0.0.1:3789"), sa("127.0.0.1:52000"));
        assert_eq!(lsof_uid(out, server, client), Some(502));
        assert_eq!(lsof_uid(out, client, server), Some(501));
        // Only the client end is visible (the server's user is not ours).
        let hidden = "p100\nu501\nn127.0.0.1:52000->127.0.0.1:3789\n";
        assert_eq!(lsof_uid(hidden, server, client), None);
        assert_eq!(lsof_uid("", server, client), None);
        let v6 = "p9\nu501\nn[::1]:3789->[::1]:52000\n";
        assert_eq!(lsof_uid(v6, sa("[::1]:3789"), sa("[::1]:52000")), Some(501));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn a_listener_owned_by_this_process_is_ours() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let accept = tokio::spawn(async move { l.accept().await.unwrap() });
        let tcp = TcpStream::connect(addr).await.unwrap();
        let _server_end = accept.await.unwrap();
        assert!(is_loopback(&tcp));
        assert!(server_is_ours(&tcp).await);
    }

}

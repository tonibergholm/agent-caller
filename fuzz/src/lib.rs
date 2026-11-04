//! Helpers shared by the fuzz targets: independent oracles, so a target does
//! not check the production code against a copy of itself where it can avoid it.

/// Inverse of `escape_text(raw, true)` in the hooks mapping: `\\`, `\n`,
/// `\t`, `\r` and `\u{hex}` become the character they stand for; a real
/// newline stays one. `None` for any other backslash sequence, which the
/// escaper never writes.
pub fn unescape_raw(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next()? {
            '\\' => out.push('\\'),
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            'u' => {
                if it.next()? != '{' {
                    return None;
                }
                let mut hex = String::new();
                loop {
                    match it.next()? {
                        '}' => break,
                        h if h.is_ascii_hexdigit() && hex.len() < 6 => hex.push(h),
                        _ => return None,
                    }
                }
                out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
            }
            _ => return None,
        }
    }
    Some(out)
}

/// Characters a card must never show raw: controls other than newline,
/// bidi controls, line/paragraph separators and the other zero-width and
/// format characters (a list written out here on purpose).
pub fn hidden_or_forbidden(c: char) -> bool {
    (c.is_control() && c != '\n')
        || matches!(c as u32,
            0x00AD | 0x0600..=0x0605 | 0x061C | 0x06DD | 0x070F | 0x0890..=0x0891 |
            0x08E2 | 0x180E | 0x200B..=0x200F | 0x2028 | 0x2029 | 0x202A..=0x202E |
            0x2060..=0x2064 | 0x2066..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB | 0x110BD |
            0x110CD | 0x13430..=0x1343F | 0x1BCA0..=0x1BCA3 | 0x1D173..=0x1D17A |
            0xE0001 | 0xE0020..=0xE007F)
}

/// The hostname rule of PR #88, written again: at most 253 bytes, dot
/// separated labels of 1 to 63 ASCII letters, digits or hyphens, none
/// starting or ending with a hyphen.
pub fn hostname_ok(h: &str) -> bool {
    h.len() <= 253
        && h.split('.').all(|l| {
            (1..=63).contains(&l.len())
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

/// A host a pairing may carry: an IPv4 or IPv6 literal or a hostname.
pub fn host_ok(h: &str) -> bool {
    h.parse::<std::net::Ipv4Addr>().is_ok()
        || h.parse::<std::net::Ipv6Addr>().is_ok()
        || hostname_ok(h)
}

/// Feeds an observation to a registry, as the daemon does, and checks that
/// no agent it then publishes encodes to more than the 64 KiB a client keeps
/// (PROTOCOL.md, client state limits): over it, every client drops the
/// connection and resyncs, in a loop for as long as the request is pending.
pub fn assert_agent_fits(obs: &acd_core::RequestObs) {
    use acd_core::{AgentObs, Observation, Registry};
    use acd_protocol::{AgentKind, AgentRole, AgentState, MAX_AGENT_BYTES, Source};
    let mut r = Registry::new("m".into());
    r.apply(Observation::AgentUpsert(AgentObs {
        id: obs.agent_id.clone(),
        source: Source::Herdr,
        kind: AgentKind::Claude,
        title: "agent".into(),
        cwd: Some("/work".into()),
        project: Some("work".into()),
        state: AgentState::Working,
        failed: false,
        last_message: None,
        can_send: true,
        role: AgentRole::Main,
        parent_title: None,
    }));
    r.apply(Observation::RequestSeen(obs.clone()));
    for a in r.agents() {
        let n = serde_json::to_vec(&a).unwrap().len();
        assert!(n <= MAX_AGENT_BYTES, "an agent of {n} bytes is published; clients keep at most {MAX_AGENT_BYTES}");
    }
}

//! What `acd` reads for the wake sender (acd-relay/src/client.rs): the
//! `[push]` section of `config.toml`, the relay URL, and the backoff.
//! Invariants: the push setting is `On` only for a clean `relay = "https://.."`
//! (a table with that key alone, a string value); every other case is `Off`
//! with one of a fixed set of reasons, and no reason shows any of the
//! config's text; an accepted URL is https with a host, no userinfo, query or
//! fragment, no trailing slash, and parses to itself again; the backoff is
//! within 10 and 900 seconds, never shrinks as failures grow, and doubles
//! until the cap.
#![no_main]
use acd_relay::client::{PushSetting, RelayUrl, backoff_secs, resolve};
use libfuzzer_sys::fuzz_target;

const REASONS: [&str; 9] = [
    "[push] is not a table",
    "[push] has an unknown key",
    "relay_pin is not supported by this build",
    "[push] relay is not a string",
    "relay is not a valid URL",
    "relay must be an https:// URL",
    "relay has no host",
    "relay must have a host and no userinfo",
    "relay must have no query or fragment",
];

fn check_url(u: &RelayUrl) {
    let s = u.as_str();
    assert!(s.starts_with("https://"), "{s:?}");
    assert!(!s.ends_with('/') && !s.contains('?') && !s.contains('#'), "{s:?}");
    let uri: hyper_free::Parts = hyper_free::split(s);
    assert!(!uri.authority.is_empty() && !uri.authority.contains('@'), "{s:?}");
    assert_eq!(RelayUrl::parse(s).as_ref(), Ok(u), "round trip of {s:?}");
}

/// Just enough of a splitter to look at the authority, independent of the parser under test.
mod hyper_free {
    pub struct Parts<'a> {
        pub authority: &'a str,
    }
    pub fn split(s: &str) -> Parts<'_> {
        let rest = s.strip_prefix("https://").unwrap_or(s);
        Parts { authority: rest.split('/').next().unwrap_or("") }
    }
}

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    match RelayUrl::parse(&text) {
        Ok(u) => check_url(&u),
        Err(e) => assert!(REASONS.contains(&e.as_str()), "reason {e:?}"),
    }
    if let Ok(table) = toml::from_str::<toml::Table>(&text) {
        match resolve(table.get("push")) {
            PushSetting::On(u) => {
                check_url(&u);
                let t = table["push"].as_table().expect("On needs a table");
                assert!(t.keys().all(|k| k == "relay"), "On with another key");
                assert_eq!(t["relay"].as_str().unwrap().trim_end_matches('/'), u.as_str());
            }
            PushSetting::Off(None) => {
                let ok = match table.get("push") {
                    None => true,
                    Some(v) => v.as_table().is_some_and(|t| t.is_empty()),
                };
                assert!(ok, "Off without a reason for {:?}", table.get("push"));
            }
            PushSetting::Off(Some(why)) => assert!(REASONS.contains(&why.as_str()), "reason {why:?}"),
        }
    }
    let n = u32::from_le_bytes([data.first().copied().unwrap_or(0), data.get(1).copied().unwrap_or(0), data.get(2).copied().unwrap_or(0), data.get(3).copied().unwrap_or(0)]);
    let b = backoff_secs(n);
    assert!((10..=900).contains(&b));
    assert!(backoff_secs(n.saturating_add(1)) >= b);
    if (2..=6).contains(&n) {
        assert_eq!(b, 2 * backoff_secs(n - 1));
    }
});

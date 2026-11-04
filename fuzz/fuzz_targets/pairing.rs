//! The pairing string parser (`acd://host:port?pin=...#token`).
//! Invariants: an accepted line holds a valid host, a non-zero port, a
//! canonical pin and token, and round-trips; a rejected host never round-trips.
#![no_main]
use acd_protocol::pairing::{Pairing, pins_equal, spki_pin};
use arbitrary::Arbitrary;
use fuzzlib::host_ok;
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
struct Host {
    host: String,
    port: u16,
    bracket: bool,
}

fn check_accepted(p: &Pairing) {
    assert_ne!(p.port, 0);
    assert!(host_ok(&p.host), "accepted host {:?}", p.host);
    assert!(
        p.host.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':')),
        "accepted host {:?} holds a URL metacharacter",
        p.host
    );
    assert!(pins_equal(&p.pin, &p.pin), "accepted pin is not canonical");
    assert_eq!(p.token.len(), 43);
    let line = p.to_line();
    assert_eq!(Pairing::parse(&line).as_ref(), Ok(p), "round trip of {line:?}");
}

fuzz_target!(|data: &[u8]| {
    // Mode 1: any text.
    if let Ok(s) = std::str::from_utf8(data) {
        let parsed = Pairing::parse(s);
        assert_eq!(parsed, Pairing::parse(s.trim()), "trim is part of parse");
        if let Ok(p) = parsed {
            check_accepted(&p);
        }
    }
    // Mode 2: an arbitrary host in an otherwise valid line, and a Pairing
    // built from an arbitrary host. A rejected host never round-trips.
    let mut u = arbitrary::Unstructured::new(data);
    let Ok(h) = Host::arbitrary(&mut u) else { return };
    let pin = spki_pin(b"fuzz");
    let token = pin.strip_prefix("sha256:").unwrap().to_string();
    let address = if h.bracket { format!("[{}]", h.host) } else { h.host.clone() };
    let line = format!("acd://{address}:{}?pin={pin}#{token}", h.port);
    if let Ok(p) = Pairing::parse(&line) {
        assert!(host_ok(&h.host), "parsed {line:?} with the rejected host {:?}", h.host);
        assert_eq!(p.host, h.host);
        assert_eq!(p.port, h.port);
        check_accepted(&p);
    }
    let built = Pairing { host: h.host.clone(), port: h.port, pin, token };
    if let Ok(back) = Pairing::parse(&built.to_line()) {
        assert!(host_ok(&h.host), "round-tripped the rejected host {:?}", h.host);
        assert_eq!(back, built);
    }
});

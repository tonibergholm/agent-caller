//! Sealed handles and the sealing-keys file (acd-relay/src/handle.rs).
//! Invariants: `open` never panics on any text and never accepts a handle
//! nobody sealed; a sealed handle opens to exactly what was sealed before its
//! expiry and is `Gone` from the expiry on, and fits the handle cap; flipping
//! any one bit of the decoded bytes, cutting a byte off or adding one is
//! never accepted; changing one character of the text is accepted only if it
//! decodes to the same bytes; the highest kid seals and dropping a kid
//! revokes its handles; a keys file that parses seals and opens, one that
//! does not gives a fixed message that holds none of its input, and `Debug`
//! of the keys and of every error holds no key material.
#![no_main]
use acd_relay::handle::{Keys, OpenError, Sealed, open, seal};
use acd_relay::wire::{AddressKind, Platform, address_ok, app_ok, MAX_HANDLE_LEN};
use arbitrary::Arbitrary;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
struct Input {
    key_a: [u8; 32],
    key_b: [u8; 32],
    kid_a: u8,
    kid_b: u8,
    kind: u8,
    app: String,
    address: String,
    issued_at: u32,
    expires_at: u32,
    now: u32,
    flip: u32,
    swap: (u16, u8),
    cut: u8,
    raw: Vec<u8>,
}

const FIXED_ERRORS: [&str; 5] = [
    "invalid keys file",
    "invalid key encoding",
    "key must be 32 bytes",
    "no sealing keys",
    "duplicate kid",
];

fn is_ok(r: &Result<Sealed, OpenError>) -> bool {
    r.is_ok()
}

fuzz_target!(|data: &[u8]| {
    // 1. The keys file parser, on any bytes.
    match Keys::from_json(data) {
        Ok(keys) => {
            let s = Sealed {
                platform: Platform::Android,
                kind: AddressKind::FcmToken,
                app: "a.b".into(),
                address: "tok".into(),
                issued_at: 1,
                expires_at: 10,
            };
            assert_eq!(open(&keys, &seal(&keys, &s), 5), Ok(s));
            let dbg = format!("{keys:?}");
            assert!(dbg.starts_with("Keys(") && dbg.ends_with(" keys)"), "{dbg:?}");
        }
        Err(e) => {
            assert!(FIXED_ERRORS.contains(&e.as_str()), "unexpected message {e:?}");
        }
    }

    // 2. Any text as a handle is refused (nobody sealed it) and never panics.
    let fixed = Keys::new(vec![(1, [7u8; 32]), (2, [8u8; 32])]).unwrap();
    let text = String::from_utf8_lossy(data);
    let r = open(&fixed, &text, 0);
    assert!(!is_ok(&r), "accepted a handle nobody sealed: {text:?}");
    assert!(matches!(format!("{r:?}").as_str(), "Err(Invalid)" | "Err(Gone)"));

    // 3. Seal, open and tamper.
    let mut u = arbitrary::Unstructured::new(data);
    let Ok(i) = Input::arbitrary(&mut u) else { return };
    let (kind, platform) = match i.kind % 3 {
        0 => (AddressKind::FcmToken, Platform::Android),
        1 => (AddressKind::FcmFid, Platform::Android),
        _ => (AddressKind::Apns, Platform::Ios),
    };
    if !app_ok(&i.app) || !address_ok(&i.address) {
        return;
    }
    let keys = if i.kid_a == i.kid_b {
        Keys::new(vec![(i.kid_a, i.key_a)])
    } else {
        Keys::new(vec![(i.kid_a, i.key_a), (i.kid_b, i.key_b)])
    }
    .unwrap();
    let sealed = Sealed { platform, kind, app: i.app, address: i.address, issued_at: i.issued_at, expires_at: i.expires_at };
    let handle = seal(&keys, &sealed);
    assert!(handle.len() <= MAX_HANDLE_LEN, "handle of {} bytes", handle.len());
    let now = u64::from(i.now);
    let opened = open(&keys, &handle, now);
    if u64::from(i.expires_at) > now {
        assert_eq!(opened, Ok(sealed.clone()));
    } else {
        assert_eq!(opened, Err(OpenError::Gone), "expired at {} and now {now}", i.expires_at);
    }
    let raw = B64.decode(&handle).unwrap();
    assert_eq!(raw[1], i.kid_a.max(i.kid_b), "the highest kid seals");

    // One bit of the decoded bytes, anywhere: never accepted.
    let bit = i.flip as usize % (raw.len() * 8);
    let mut bad = raw.clone();
    bad[bit / 8] ^= 1 << (bit % 8);
    let r = open(&keys, &B64.encode(&bad), 0);
    assert!(!is_ok(&r), "bit {bit} flipped and still accepted");
    // Cut or extended.
    let n = raw.len() - 1 - (i.cut as usize % raw.len().min(40));
    assert!(!is_ok(&open(&keys, &B64.encode(&raw[..n]), 0)), "cut to {n}");
    let mut longer = raw.clone();
    longer.push(i.cut);
    assert!(!is_ok(&open(&keys, &B64.encode(&longer), 0)), "extended");
    // One character of the text replaced.
    let mut chars: Vec<u8> = handle.bytes().collect();
    let at = i.swap.0 as usize % chars.len();
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let c = alphabet[i.swap.1 as usize % alphabet.len()];
    if chars[at] != c {
        chars[at] = c;
        let t = String::from_utf8(chars).unwrap();
        if let Ok(back) = open(&keys, &t, 0) {
            assert_eq!(B64.decode(&t).ok(), Some(raw.clone()), "a different encoding was accepted");
            assert_eq!(Some(back), open(&keys, &handle, 0).ok());
        }
    }
    // Dropping the sealing kid revokes the handle; the other key never opens it.
    if i.kid_a != i.kid_b {
        let only_other = if raw[1] == i.kid_a { (i.kid_b, i.key_b) } else { (i.kid_a, i.key_a) };
        let only_other = Keys::new(vec![only_other]).unwrap();
        assert_eq!(open(&only_other, &handle, 0), Err(OpenError::Gone));
    }
    // No key material in what a log could print.
    for k in [i.key_a, i.key_b] {
        let hex: String = k.iter().map(|b| format!("{b:02x}")).collect();
        let dbg = format!("{keys:?}");
        assert!(!dbg.contains(&hex) && !dbg.contains(&B64.encode(k)));
    }
    let _ = i.raw;
});

//! Property tests of sealed handles (spec §5.3.3): seal and open round-trip,
//! the expiry edge, and that no change to a handle is ever accepted.
mod proptest_support;

use acd_relay::handle::{Keys, OpenError, Sealed, open, seal};
use acd_relay::wire::{AddressKind, MAX_ADDRESS_LEN, MAX_APP_LEN, MAX_HANDLE_LEN, Platform, address_ok, app_ok};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use proptest::prelude::*;
use proptest_support::check;

fn text(alphabet: &'static [u8], max: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(alphabet.to_vec()), 1..=max)
        .prop_map(|v| String::from_utf8(v).unwrap())
}

fn app() -> impl Strategy<Value = String> {
    text(b"abcxyzABCXYZ0189_.-", MAX_APP_LEN)
}

fn address() -> impl Strategy<Value = String> {
    text(b"abcxyzABCXYZ0189_:.-", MAX_ADDRESS_LEN)
}

fn sealed() -> impl Strategy<Value = Sealed> {
    (0u8..3, app(), address(), any::<u32>(), any::<u32>()).prop_map(|(k, app, address, issued_at, expires_at)| {
        let (kind, platform) = match k {
            0 => (AddressKind::FcmToken, Platform::Android),
            1 => (AddressKind::FcmFid, Platform::Android),
            _ => (AddressKind::Apns, Platform::Ios),
        };
        Sealed { platform, kind, app, address, issued_at, expires_at }
    })
}

fn keys() -> impl Strategy<Value = (Keys, Vec<u8>)> {
    // One to three keys with distinct kids; the highest kid seals.
    prop::collection::btree_map(any::<u8>(), any::<[u8; 32]>(), 1..4).prop_map(|m| {
        let kids: Vec<u8> = m.keys().copied().collect();
        (Keys::new(m.into_iter().collect()).unwrap(), kids)
    })
}

#[test]
fn a_sealed_handle_opens_to_what_was_sealed_until_it_expires() {
    check("handle_round_trip", 400, (keys(), sealed(), any::<u32>()), |((keys, kids), s, now)| {
        prop_assert!(app_ok(&s.app) && address_ok(&s.address));
        let h = seal(&keys, &s);
        prop_assert!(h.len() <= MAX_HANDLE_LEN, "{} bytes", h.len());
        prop_assert_eq!(B64.decode(&h).unwrap()[1], *kids.last().unwrap(), "the highest kid seals");
        let now = u64::from(now);
        if u64::from(s.expires_at) > now {
            prop_assert_eq!(open(&keys, &h, now), Ok(s.clone()));
        } else {
            prop_assert_eq!(open(&keys, &h, now), Err(OpenError::Gone));
        }
        // The expiry edge, exactly.
        let e = u64::from(s.expires_at);
        prop_assert_eq!(open(&keys, &h, e), Err(OpenError::Gone));
        if e > 0 {
            prop_assert_eq!(open(&keys, &h, e - 1), Ok(s));
        }
        Ok(())
    });
}

#[test]
fn flipping_any_one_bit_cutting_or_extending_is_never_accepted() {
    check("handle_tamper", 300, (keys(), sealed(), any::<prop::sample::Index>(), any::<u8>()), |((keys, _), mut s, at, extra)| {
        s.expires_at = s.expires_at.max(1);
        let h = seal(&keys, &s);
        let raw = B64.decode(&h).unwrap();
        // Every bit of a short handle, a random one of a long one: the loop
        // covers each position often enough over the cases.
        let bits = raw.len() * 8;
        let picks: Vec<usize> = if bits <= 1600 { (0..bits).collect() } else { vec![at.index(bits)] };
        for bit in picks {
            let mut bad = raw.clone();
            bad[bit / 8] ^= 1 << (bit % 8);
            prop_assert!(open(&keys, &B64.encode(&bad), 0).is_err(), "bit {} accepted", bit);
        }
        let cut = at.index(raw.len());
        prop_assert!(open(&keys, &B64.encode(&raw[..cut]), 0).is_err(), "cut at {}", cut);
        let mut longer = raw.clone();
        longer.push(extra);
        prop_assert!(open(&keys, &B64.encode(&longer), 0).is_err(), "extended");
        Ok(())
    });
}

#[test]
fn a_handle_opens_only_under_the_key_it_was_sealed_with() {
    let map = || prop::collection::btree_map(any::<u8>(), any::<[u8; 32]>(), 1..4);
    check("handle_keys", 300, (map(), 0usize..32, sealed()), |(m, at, mut s)| {
        s.expires_at = s.expires_at.max(1);
        let all = |m: &std::collections::BTreeMap<u8, [u8; 32]>| Keys::new(m.iter().map(|(k, v)| (*k, *v)).collect()).unwrap();
        let sealing = *m.keys().next_back().unwrap();
        let h = seal(&all(&m), &s);
        // Same keys: opens to the same value (the success case).
        prop_assert_eq!(open(&all(&m), &h, 0), Ok(s.clone()));
        // The SAME kid with DIFFERENT key bytes, everything else equal: refused.
        // A derivation that ignored the sealing secret would open it.
        let mut changed = m.clone();
        changed.get_mut(&sealing).unwrap()[at % 32] ^= 0x01;
        prop_assert_eq!(open(&all(&changed), &h, 0), Err(OpenError::Invalid), "a different key under the sealing kid opened it");
        // A different kid: unknown, so gone.
        let mut dropped = m.clone();
        dropped.remove(&sealing);
        let fresh = (0..=u8::MAX).find(|k| !m.contains_key(k)).unwrap_or(sealing);
        if fresh != sealing {
            dropped.insert(fresh, [9u8; 32]);
        }
        if !dropped.is_empty() {
            prop_assert_eq!(open(&all(&dropped), &h, 0), Err(OpenError::Gone));
        }
        Ok(())
    });
}

#[test]
fn no_text_nobody_sealed_is_accepted() {
    check("handle_garbage", 400, (keys(), prop::collection::vec(any::<u8>(), 0..700)), |((keys, _), bytes)| {
        let as_text = String::from_utf8_lossy(&bytes).into_owned();
        prop_assert!(open(&keys, &as_text, 0).is_err());
        prop_assert!(open(&keys, &B64.encode(&bytes), 0).is_err());
        Ok(())
    });
}

//! Sealed handles (spec §5.3.3).
//!
//! Layout: `version(1) | kid(1) | salt(24) | ChaCha20-Poly1305(payload) | tag(16)`,
//! base64url without padding. The AEAD key is HKDF-SHA256 over the sealing
//! key and the random salt, so every handle has its own key and the nonce is
//! a constant (a key never seals twice).

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, Nonce, UnboundKey};
use ring::hkdf;
use ring::rand::{SecureRandom, SystemRandom};

use crate::wire::{AddressKind, MAX_ADDRESS_LEN, MAX_APP_LEN, MAX_HANDLE_LEN, Platform};

const VERSION: u8 = 1;
const SALT_LEN: usize = 24;
const TAG_LEN: usize = 16;
const HEADER: usize = 2 + SALT_LEN;
const INFO: &[u8] = b"acd-relay handle v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    pub platform: Platform,
    pub kind: AddressKind,
    pub app: String,
    pub address: String,
    pub issued_at: u32,
    pub expires_at: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenError {
    /// Malformed, tampered, truncated, unknown version: HTTP 400.
    Invalid,
    /// Expired, or sealed under a key that is no longer listed: HTTP 404.
    Gone,
}

/// The sealing keys by `kid`. The highest `kid` seals; every listed key opens.
#[derive(Clone)]
pub struct Keys {
    keys: Vec<(u8, [u8; 32])>,
}

impl std::fmt::Debug for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Keys({} keys)", self.keys.len())
    }
}

impl Keys {
    pub fn new(keys: Vec<(u8, [u8; 32])>) -> Result<Keys, String> {
        if keys.is_empty() {
            return Err("no sealing keys".into());
        }
        let mut seen = std::collections::HashSet::new();
        for (kid, _) in &keys {
            if !seen.insert(*kid) {
                return Err("duplicate kid".into());
            }
        }
        Ok(Keys { keys })
    }

    /// `{"keys":[{"kid":1,"key":"<base64url of 32 bytes>"}]}`.
    pub fn from_json(raw: &[u8]) -> Result<Keys, String> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct File {
            keys: Vec<Entry>,
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Entry {
            kid: u8,
            key: String,
        }
        let f: File = serde_json::from_slice(raw).map_err(|_| "invalid keys file".to_string())?;
        let mut keys = Vec::new();
        for e in f.keys {
            let b = B64
                .decode(e.key.trim())
                .map_err(|_| "invalid key encoding".to_string())?;
            let k: [u8; 32] = b.try_into().map_err(|_| "key must be 32 bytes".to_string())?;
            keys.push((e.kid, k));
        }
        Keys::new(keys)
    }

    fn active(&self) -> (u8, &[u8; 32]) {
        let (k, v) = self.keys.iter().max_by_key(|(k, _)| *k).expect("non-empty");
        (*k, v)
    }

    fn get(&self, kid: u8) -> Option<&[u8; 32]> {
        self.keys.iter().find(|(k, _)| *k == kid).map(|(_, v)| v)
    }
}

struct Len32;
impl hkdf::KeyType for Len32 {
    fn len(&self) -> usize {
        32
    }
}

fn derive(ikm: &[u8; 32], salt: &[u8]) -> LessSafeKey {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(ikm);
    let okm = prk.expand(&[INFO], Len32).expect("hkdf length");
    let mut k = [0u8; 32];
    okm.fill(&mut k).expect("hkdf fill");
    LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, &k).expect("key length"))
}

fn encode_payload(s: &Sealed) -> Vec<u8> {
    let mut p = Vec::with_capacity(4 + 4 + 3 + s.app.len() + 2 + s.address.len());
    p.extend_from_slice(&s.issued_at.to_be_bytes());
    p.extend_from_slice(&s.expires_at.to_be_bytes());
    p.push(s.platform as u8);
    p.push(s.kind as u8);
    p.push(s.app.len() as u8);
    p.extend_from_slice(s.app.as_bytes());
    p.extend_from_slice(&(s.address.len() as u16).to_be_bytes());
    p.extend_from_slice(s.address.as_bytes());
    p
}

fn decode_payload(p: &[u8]) -> Option<Sealed> {
    let issued_at = u32::from_be_bytes(p.get(0..4)?.try_into().ok()?);
    let expires_at = u32::from_be_bytes(p.get(4..8)?.try_into().ok()?);
    let platform = Platform::from_u8(*p.get(8)?)?;
    let kind = AddressKind::from_u8(*p.get(9)?)?;
    let app_len = *p.get(10)? as usize;
    if app_len == 0 || app_len > MAX_APP_LEN {
        return None;
    }
    let app = std::str::from_utf8(p.get(11..11 + app_len)?).ok()?.to_string();
    let o = 11 + app_len;
    let addr_len = u16::from_be_bytes(p.get(o..o + 2)?.try_into().ok()?) as usize;
    if addr_len == 0 || addr_len > MAX_ADDRESS_LEN || p.len() != o + 2 + addr_len {
        return None;
    }
    let address = std::str::from_utf8(&p[o + 2..]).ok()?.to_string();
    if kind.platform() != platform {
        return None;
    }
    Some(Sealed { platform, kind, app, address, issued_at, expires_at })
}

/// Seal `s` under the active key. Panics only if the OS RNG fails.
pub fn seal(keys: &Keys, s: &Sealed) -> String {
    let (kid, ikm) = keys.active();
    let mut salt = [0u8; SALT_LEN];
    SystemRandom::new().fill(&mut salt).expect("os rng");
    let key = derive(ikm, &salt);
    let mut buf = encode_payload(s);
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key([0u8; 12]),
        Aad::from([VERSION, kid]),
        &mut buf,
    )
    .expect("seal");
    let mut out = Vec::with_capacity(HEADER + buf.len());
    out.push(VERSION);
    out.push(kid);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&buf);
    B64.encode(out)
}

pub fn open(keys: &Keys, handle: &str, now_secs: u64) -> Result<Sealed, OpenError> {
    if handle.is_empty() || handle.len() > MAX_HANDLE_LEN {
        return Err(OpenError::Invalid);
    }
    let raw = B64.decode(handle).map_err(|_| OpenError::Invalid)?;
    if raw.len() < HEADER + TAG_LEN + 1 || raw[0] != VERSION {
        return Err(OpenError::Invalid);
    }
    let kid = raw[1];
    let ikm = keys.get(kid).ok_or(OpenError::Gone)?;
    let key = derive(ikm, &raw[2..HEADER]);
    let mut buf = raw[HEADER..].to_vec();
    let plain = key
        .open_in_place(
            Nonce::assume_unique_for_key([0u8; 12]),
            Aad::from([VERSION, kid]),
            &mut buf,
        )
        .map_err(|_| OpenError::Invalid)?;
    let s = decode_payload(plain).ok_or(OpenError::Invalid)?;
    if u64::from(s.expires_at) <= now_secs {
        return Err(OpenError::Gone);
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> Keys {
        Keys::new(vec![(1, [7u8; 32])]).unwrap()
    }
    fn sample() -> Sealed {
        Sealed {
            platform: Platform::Android,
            kind: AddressKind::FcmToken,
            app: "com.example.agentcaller".into(),
            address: "tok:abc-DEF_123.xyz".into(),
            issued_at: 1_000,
            expires_at: 5_000,
        }
    }

    #[test]
    fn seal_open_round_trip() {
        let h = seal(&keys(), &sample());
        assert_eq!(open(&keys(), &h, 2_000).unwrap(), sample());
        assert!(h.len() <= MAX_HANDLE_LEN);
    }

    #[test]
    fn sealing_twice_gives_different_handles() {
        assert_ne!(seal(&keys(), &sample()), seal(&keys(), &sample()));
    }

    fn flip(h: &str, idx: usize) -> String {
        let mut raw = B64.decode(h).unwrap();
        raw[idx] ^= 1;
        B64.encode(raw)
    }

    #[test]
    fn tampered_ciphertext_is_invalid_handle() {
        let h = seal(&keys(), &sample());
        let n = B64.decode(&h).unwrap().len();
        for idx in [HEADER, HEADER + 5, n - 1, 2] {
            assert_eq!(open(&keys(), &flip(&h, idx), 2_000), Err(OpenError::Invalid), "idx {idx}");
        }
    }

    #[test]
    fn truncated_handle_is_invalid_handle() {
        let h = seal(&keys(), &sample());
        let raw = B64.decode(&h).unwrap();
        for cut in [0, 1, 2, HEADER, HEADER + TAG_LEN, raw.len() - 1] {
            let t = B64.encode(&raw[..cut]);
            assert_eq!(open(&keys(), &t, 2_000), Err(OpenError::Invalid), "cut {cut}");
        }
    }

    #[test]
    fn base64_garbage_is_invalid_handle() {
        assert_eq!(open(&keys(), "!!!not base64!!!", 0), Err(OpenError::Invalid));
        assert_eq!(open(&keys(), "", 0), Err(OpenError::Invalid));
        assert_eq!(open(&keys(), &"A".repeat(MAX_HANDLE_LEN + 1), 0), Err(OpenError::Invalid));
    }

    #[test]
    fn unknown_version_is_invalid_handle() {
        let h = seal(&keys(), &sample());
        assert_eq!(open(&keys(), &flip_byte0(&h, 2), 2_000), Err(OpenError::Invalid));
        assert_eq!(open(&keys(), &flip_byte0(&h, 0), 2_000), Err(OpenError::Invalid));
    }

    fn flip_byte0(h: &str, v: u8) -> String {
        let mut raw = B64.decode(h).unwrap();
        raw[0] = v;
        B64.encode(raw)
    }

    #[test]
    fn unknown_kid_is_gone() {
        let h = seal(&keys(), &sample());
        let other = Keys::new(vec![(2, [9u8; 32])]).unwrap();
        assert_eq!(open(&other, &h, 2_000), Err(OpenError::Gone));
    }

    #[test]
    fn expired_handle_is_gone() {
        let h = seal(&keys(), &sample());
        assert_eq!(open(&keys(), &h, 5_000), Err(OpenError::Gone));
        assert_eq!(open(&keys(), &h, 9_999), Err(OpenError::Gone));
        assert!(open(&keys(), &h, 4_999).is_ok());
    }

    #[test]
    fn kid_is_authenticated() {
        // Same key bytes under two kids: only the kid in the AAD differs.
        let both = Keys::new(vec![(1, [7u8; 32]), (2, [7u8; 32])]).unwrap();
        let one = Keys::new(vec![(1, [7u8; 32])]).unwrap();
        let h = seal(&one, &sample());
        let mut raw = B64.decode(&h).unwrap();
        raw[1] = 2;
        assert_eq!(open(&both, &B64.encode(raw), 2_000), Err(OpenError::Invalid));
    }

    #[test]
    fn rotated_in_key_seals_and_old_key_still_opens() {
        let old = Keys::new(vec![(1, [7u8; 32])]).unwrap();
        let h_old = seal(&old, &sample());
        let rotated = Keys::new(vec![(1, [7u8; 32]), (2, [8u8; 32])]).unwrap();
        assert_eq!(B64.decode(seal(&rotated, &sample())).unwrap()[1], 2);
        assert!(open(&rotated, &h_old, 2_000).is_ok());
        // Removing the old key revokes its handles.
        let only_new = Keys::new(vec![(2, [8u8; 32])]).unwrap();
        assert_eq!(open(&only_new, &h_old, 2_000), Err(OpenError::Gone));
    }

    #[test]
    fn longest_handle_fits_512_bytes() {
        let s = Sealed {
            app: "a".repeat(MAX_APP_LEN),
            address: "x".repeat(MAX_ADDRESS_LEN),
            ..sample()
        };
        let h = seal(&keys(), &s);
        assert_eq!(B64.decode(&h).unwrap().len(), 374);
        assert!(h.len() <= MAX_HANDLE_LEN, "{}", h.len());
        assert_eq!(open(&keys(), &h, 2_000).unwrap(), s);
    }
}

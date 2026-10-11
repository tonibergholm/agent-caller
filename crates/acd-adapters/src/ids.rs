//! Bounded agent ids (spec §5). The acd/1 engine rejects any id over 128
//! bytes, so an adapter must never publish one a client cannot send back.
use sha2::{Digest, Sha256};

/// Longest id acd/1 accepts; mirrors `acd_core::MAX_ID_BYTES`.
pub const MAX_ID_BYTES: usize = 128;

/// Marks a hashed id body. A raw body that starts with it is hashed too, so a
/// raw id can never collide with a hashed one.
const HASH_MARK: &str = "h:";

/// `{prefix}{raw}` when it fits and is unambiguous; otherwise
/// `{prefix}h:{sha256(raw)[..20]}` as 40 hex chars (160 bits). The short form
/// is one-way: callers keep a map from the id back to the source's own id.
pub fn bounded(prefix: &str, raw: &str) -> String {
    if prefix.len() + raw.len() <= MAX_ID_BYTES && !raw.starts_with(HASH_MARK) {
        return format!("{prefix}{raw}");
    }
    let digest = Sha256::digest(raw.as_bytes());
    let hex: String = digest.iter().take(20).map(|b| format!("{b:02x}")).collect();
    format!("{prefix}{HASH_MARK}{hex}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_ids_are_unchanged() {
        assert_eq!(bounded("t3:", "abc"), "t3:abc");
        let fit = "a".repeat(MAX_ID_BYTES - 3);
        assert_eq!(bounded("t3:", &fit), format!("t3:{fit}"));
    }

    #[test]
    fn long_ids_hash_within_limit_and_stay_distinct() {
        let base = "x".repeat(200);
        let a = bounded("t3:", &format!("{base}1"));
        let b = bounded("t3:", &format!("{base}2"));
        assert!(a.len() <= MAX_ID_BYTES && b.len() <= MAX_ID_BYTES);
        assert_ne!(a, b);
        assert_eq!(a, bounded("t3:", &format!("{base}1")), "stable");
        let over = "a".repeat(MAX_ID_BYTES - 2);
        assert!(bounded("t3:", &over).starts_with("t3:h:"));
    }

    #[test]
    fn raw_ids_that_look_hashed_are_hashed() {
        assert_ne!(bounded("t3:", "h:abc"), "t3:h:abc");
    }
}

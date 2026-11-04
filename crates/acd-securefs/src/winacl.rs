//! The Windows owner-only rules that need no Windows API: SDDL strings, the
//! judgement of a DACL, and nothing else. It is compiled on every target so
//! the Linux tests pin the decisions; `windows.rs` feeds it the facts it reads
//! from the OS.
//!
//! The rule: a file or directory in the config dir is private when its owner
//! is the current user's SID and its DACL has at least one entry, every entry
//! is an "access allowed" entry, and every entry names the current user's SID.
//! A missing (NULL) DACL means "everyone", an empty DACL gives the user
//! nothing, and any deny, audit, object or unknown entry is refused: the
//! check fails closed rather than reason about shapes it does not know.
#![cfg_attr(not(windows), allow(dead_code))]

/// Longest SID string Windows produces is 184 characters (15 sub-authorities).
const MAX_SID_LEN: usize = 184;

/// True for the canonical `S-1-<digits>(-<digits>)*` form. The SID is pasted
/// into SDDL, so anything else (parentheses, semicolons, letters other than
/// the leading `S`) must never get through.
pub fn valid_sid_string(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("S-1-") else {
        return false;
    };
    s.len() <= MAX_SID_LEN
        && !rest.is_empty()
        && rest
            .split('-')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// SDDL for a new private file: owner and only ACE are the user, DACL
/// protected (`P`) so nothing is inherited from the parent.
pub fn private_file_sddl(user_sid: &str) -> Option<String> {
    valid_sid_string(user_sid).then(|| format!("O:{user_sid}D:P(A;;FA;;;{user_sid})"))
}

/// SDDL for a new private directory: as for a file, and the one ACE is
/// inherited by files (`OI`) and subdirectories (`CI`) created inside.
pub fn private_dir_sddl(user_sid: &str) -> Option<String> {
    valid_sid_string(user_sid).then(|| format!("O:{user_sid}D:P(A;OICI;FA;;;{user_sid})"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AceKind {
    /// ACCESS_ALLOWED_ACE_TYPE.
    Allow,
    /// Deny, audit, object, callback, unknown: all refused.
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ace {
    pub kind: AceKind,
    /// The trustee, in SID string form. Empty when the SID was not valid.
    pub sid: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    /// The owner is not the current user.
    Owner,
    /// The DACL lets someone else in, or cannot be trusted. The text names the
    /// reason and never a SID.
    Open(&'static str),
}

/// Judges a security descriptor. `dacl` is `None` for a NULL DACL.
pub fn judge(owner_sid: &str, user_sid: &str, dacl: Option<&[Ace]>) -> Verdict {
    if !valid_sid_string(user_sid) || !owner_sid.eq_ignore_ascii_case(user_sid) {
        return Verdict::Owner;
    }
    let Some(aces) = dacl else {
        return Verdict::Open("it has no DACL, which grants everyone access");
    };
    if aces.is_empty() {
        return Verdict::Open("its DACL is empty");
    }
    for a in aces {
        if a.kind != AceKind::Allow {
            return Verdict::Open("its DACL has an entry that is not a plain allow entry");
        }
        if !a.sid.eq_ignore_ascii_case(user_sid) {
            return Verdict::Open("its DACL grants access to someone other than you");
        }
    }
    Verdict::Ok
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "S-1-5-21-1004336348-1177238915-682003330-1001";
    const SYSTEM: &str = "S-1-5-18";
    const ADMINS: &str = "S-1-5-32-544";
    const EVERYONE: &str = "S-1-1-0";

    fn allow(sid: &str) -> Ace {
        Ace { kind: AceKind::Allow, sid: sid.into() }
    }

    #[test]
    fn sid_strings_are_canonical_or_refused() {
        for ok in [ME, SYSTEM, ADMINS, EVERYONE] {
            assert!(valid_sid_string(ok), "{ok}");
        }
        for bad in [
            "", "S-1-", "S-1-5-", "S-1-5--18", "s-1-5-18", "S-2-5-18", "S-1-5-18)",
            "S-1-5-18;", "S-1-5-18(A;;FA;;;WD)", "S-1-5-1 8", "S-1-5-0x12", "S-1-5-18\n",
            "WD", "S-1-5-21-é",
        ] {
            assert!(!valid_sid_string(bad), "{bad:?}");
        }
        let long = format!("S-1-{}", vec!["1"; 100].join("-"));
        assert!(!valid_sid_string(&long));
    }

    #[test]
    fn sddl_strings_are_exact() {
        assert_eq!(
            private_file_sddl(ME).unwrap(),
            "O:S-1-5-21-1004336348-1177238915-682003330-1001D:P(A;;FA;;;S-1-5-21-1004336348-1177238915-682003330-1001)"
        );
        assert_eq!(
            private_dir_sddl("S-1-5-18").unwrap(),
            "O:S-1-5-18D:P(A;OICI;FA;;;S-1-5-18)"
        );
    }

    #[test]
    fn sddl_refuses_a_sid_that_could_inject_entries() {
        for bad in ["S-1-5-18)(A;;FA;;;WD", "WD", "", "S-1-5-18;;"] {
            assert_eq!(private_file_sddl(bad), None, "{bad:?}");
            assert_eq!(private_dir_sddl(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_dacl_with_only_the_user_is_private() {
        assert_eq!(judge(ME, ME, Some(&[allow(ME)])), Verdict::Ok);
        // Two entries for the user (for example a file and a dir ACE) are fine.
        assert_eq!(judge(ME, ME, Some(&[allow(ME), allow(ME)])), Verdict::Ok);
    }

    #[test]
    fn any_other_trustee_opens_the_file() {
        for other in [SYSTEM, ADMINS, EVERYONE] {
            assert!(matches!(judge(ME, ME, Some(&[allow(ME), allow(other)])), Verdict::Open(_)), "{other}");
            assert!(matches!(judge(ME, ME, Some(&[allow(other), allow(ME)])), Verdict::Open(_)), "{other}");
        }
    }

    #[test]
    fn a_wrong_owner_is_refused_even_with_a_perfect_dacl() {
        for owner in [SYSTEM, ADMINS, ""] {
            assert_eq!(judge(owner, ME, Some(&[allow(ME)])), Verdict::Owner, "{owner:?}");
        }
        // An unusable current-user SID can never match anything.
        assert_eq!(judge("", "", Some(&[allow("")])), Verdict::Owner);
        assert_eq!(judge("junk", "junk", Some(&[allow("junk")])), Verdict::Owner);
    }

    #[test]
    fn null_empty_deny_and_unknown_entries_fail_closed() {
        assert!(matches!(judge(ME, ME, None), Verdict::Open(_)));
        assert!(matches!(judge(ME, ME, Some(&[])), Verdict::Open(_)));
        let deny = Ace { kind: AceKind::Other, sid: EVERYONE.into() };
        assert!(matches!(judge(ME, ME, Some(&[allow(ME), deny])), Verdict::Open(_)));
        // Even a non-allow entry that names the user is refused.
        let odd = Ace { kind: AceKind::Other, sid: ME.into() };
        assert!(matches!(judge(ME, ME, Some(&[odd])), Verdict::Open(_)));
        // An entry whose SID could not be read.
        assert!(matches!(judge(ME, ME, Some(&[allow(ME), allow("")])), Verdict::Open(_)));
    }

    #[test]
    fn sid_comparison_ignores_case_only() {
        let lower = ME.to_ascii_lowercase();
        assert_eq!(judge(&lower, ME, Some(&[allow(&lower)])), Verdict::Ok);
    }

    #[test]
    fn verdict_texts_carry_no_sid() {
        for v in [
            judge(ME, ME, None),
            judge(ME, ME, Some(&[allow(EVERYONE)])),
        ] {
            if let Verdict::Open(t) = v {
                assert!(!t.contains("S-1-"), "{t}");
            }
        }
    }
}

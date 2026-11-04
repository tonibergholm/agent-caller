//! Tests added from the second cargo-mutants run over the idempotency table,
//! its quotas, the registry and the id bound
//! (docs/research/2026-11-04-mutation-testing.md). Each test names the mutant
//! it kills.
use std::time::{Duration, Instant};

use acd_core::testing::*;
use acd_core::*;
use acd_protocol::*;

const TTL: Duration = Duration::from_secs(900);

fn limits() -> IdemLimits {
    IdemLimits {
        capacity: usize::MAX,
        network: usize::MAX,
        local: usize::MAX,
        per_client: usize::MAX,
        per_source: usize::MAX,
        client_ids_per_source: usize::MAX,
    }
}

fn done() -> CommandResult {
    CommandResult::delivered("1")
}

/// idem.rs:164 `&&` -> `||` in `conflicts`: the same payload never conflicts,
/// pending or answered; only a different one does.
#[test]
fn the_same_payload_is_not_a_conflict() {
    let t = Instant::now();
    let mut c = IdemCache::new(TTL);
    assert!(matches!(c.begin("cl", "1", [1; 32], t), Begin::Run));
    assert!(!c.conflicts("cl", "1", [1; 32], t), "pending, same payload");
    assert!(c.conflicts("cl", "1", [2; 32], t), "pending, other payload");
    c.finish("cl", "1", done(), true, t);
    assert!(!c.conflicts("cl", "1", [1; 32], t), "answered, same payload");
    assert!(c.conflicts("cl", "1", [2; 32], t), "answered, other payload");
}

/// idem.rs:165 `<` -> `<=` in `conflicts`: an entry exactly TTL old is expired,
/// the same boundary `begin_from` applies.
#[test]
fn an_entry_exactly_ttl_old_no_longer_conflicts() {
    let t = Instant::now();
    let mut c = IdemCache::new(TTL);
    assert!(matches!(c.begin("cl", "1", [1; 32], t), Begin::Run));
    c.finish("cl", "1", done(), true, t);
    assert!(c.conflicts("cl", "1", [2; 32], t + TTL - Duration::from_nanos(1)));
    assert!(!c.conflicts("cl", "1", [2; 32], t + TTL));
}

/// idem.rs:187 `<` -> `<=` in `admits`: the local socket holds exactly
/// `local` entries.
#[test]
fn the_local_quota_is_exact() {
    let t = Instant::now();
    let mut c = IdemCache::with_limits(TTL, IdemLimits { local: 2, ..limits() });
    assert!(matches!(c.begin("a", "1", [1; 32], t), Begin::Run));
    assert!(matches!(c.begin("b", "1", [1; 32], t), Begin::Run));
    assert!(matches!(c.begin("c", "1", [1; 32], t), Begin::Full));
}

/// registry.rs:65 `c.is_control()` guard -> false in `cut_cwd`: a control
/// character costs its six-byte JSON escape, so a cwd of them is cut at
/// 8 KiB of encoded bytes (1365 characters), not at 8192 characters.
#[test]
fn control_characters_in_a_cwd_count_as_escapes() {
    let mut r = Registry::new("m".into());
    let mut a = agent_obs("a1");
    a.cwd = Some("\u{1}".repeat(3000));
    r.apply(Observation::AgentUpsert(a));
    let cwd = r.agent("a1").unwrap().cwd.clone().unwrap();
    assert_eq!(cwd.chars().count(), 8 * 1024 / 6);
    assert!(serde_json::to_vec(&cwd).unwrap().len() <= 8 * 1024 + 2);
}

/// registry.rs:82 `>` -> `>=` in `RequestObs::bounded`: a request that encodes
/// to exactly 40 KiB is shown whole; one byte more is read-only.
#[test]
fn a_request_of_exactly_40_kib_is_kept() {
    let size = |o: &RequestObs| {
        serde_json::to_vec(&(&o.title, &o.detail, &o.options, &o.questions, &o.raw_screen)).unwrap().len()
    };
    let mut o = permission_obs("a1", "k1");
    o.detail = String::new();
    let pad = 40 * 1024 - size(&o);
    for (extra, whole) in [(0, true), (1, false)] {
        let mut o = o.clone();
        o.detail = "x".repeat(pad + extra);
        assert_eq!(size(&o), 40 * 1024 + extra);
        let mut r = Registry::new("m".into());
        r.apply(Observation::AgentUpsert(agent_obs("a1")));
        r.apply(Observation::RequestSeen(o));
        let req = r.request(&r.agent("a1").unwrap().pending.as_ref().unwrap().id).unwrap();
        assert_eq!(req.answer_path == AnswerPath::Hook, whole, "+{extra}");
        assert_eq!(req.options.len(), if whole { 2 } else { 0 }, "+{extra}");
    }
}

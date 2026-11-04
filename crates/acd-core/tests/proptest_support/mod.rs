//! Deterministic proptest harness for this crate's `proptest_*.rs` files.
//!
//! Every property runs from a fixed seed, so a run is reproducible. A failure
//! panics with the seed and the shrunk input. Environment variables:
//!
//! - `ACD_PROPTEST_SEED`: base seed (default fixed). Mixed with the test name.
//! - `ACD_PROPTEST_CASES`: cases per property (default set by each test).
//!
//! No regression files are written: the shrunk input is printed instead, and a
//! real bug gets a plain regression test.
#![allow(dead_code)]

use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestError, TestRng, TestRunner};

pub const DEFAULT_SEED: u64 = 0x0000_acd1_0000_1104;

fn env_u64(name: &str) -> Option<u64> {
    let raw = std::env::var(name).ok()?;
    let raw = raw.trim();
    match raw.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => raw.parse().ok(),
    }
}

pub fn base_seed() -> u64 {
    env_u64("ACD_PROPTEST_SEED").unwrap_or(DEFAULT_SEED)
}

pub fn cases(default: u32) -> u32 {
    env_u64("ACD_PROPTEST_CASES")
        .map(|n| n.clamp(1, u32::MAX as u64) as u32)
        .unwrap_or(default)
}

fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// The seed a named property runs from.
pub fn seed_for(name: &str) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in name.bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
    }
    base_seed() ^ h
}

/// Runs `test` against `default_cases` (or `ACD_PROPTEST_CASES`) values of
/// `strategy`, from a seed fixed by the property name.
pub fn check<S: Strategy>(
    name: &str,
    default_cases: u32,
    strategy: S,
    test: impl Fn(S::Value) -> Result<(), TestCaseError>,
) {
    let cases = cases(default_cases);
    let seed = seed_for(name);
    let mut bytes = [0u8; 32];
    let mut state = seed;
    for chunk in bytes.chunks_mut(8) {
        chunk.copy_from_slice(&splitmix(&mut state).to_le_bytes());
    }
    let config = Config {
        cases,
        failure_persistence: None,
        max_shrink_iters: 2048,
        ..Config::default()
    };
    let mut runner = TestRunner::new_with_rng(config, TestRng::from_seed(RngAlgorithm::ChaCha, &bytes));
    if let Err(e) = runner.run(&strategy, test) {
        let what = match &e {
            TestError::Fail(reason, input) => format!("{reason}\nshrunk input: {input:#?}"),
            TestError::Abort(reason) => format!("aborted: {reason}"),
        };
        panic!(
            "property `{name}` failed (seed {seed:#x}; rerun with \
             ACD_PROPTEST_SEED={:#x} ACD_PROPTEST_CASES={cases}):\n{what}",
            base_seed()
        );
    }
}

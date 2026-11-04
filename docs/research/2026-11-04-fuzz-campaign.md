# Fuzz campaign, 2026-11-04

Branch `claude/relay-fuzz-campaign-dv2fpk` (PR #108, stacked on #97). Scope:
fuzz targets for the relay and for code merged recently, a 60-minute run of
every target, fixes for what they found, proptest suites.

## Method

- `fuzz/` (cargo-fuzz 0.13.2, nightly, libFuzzer with ASan). Eleven new
  targets next to the seven from #102. Every target asserts invariants
  against an oracle or model written separately from the code under test
  (a model of the spec for the stateful ones: challenges, limiter, idempotency
  table, notify gate; the fuzz crate's own helpers for text).
- Each of the 18 targets ran **3600 s** (`-max_total_time=3600 -timeout=20
  -rss_limit_mb=2048`), four at a time on a 4-core sandbox, starting from the
  committed corpus (`herdr_screen` also reads `tests/fixtures/herdr`). The
  runs shared the cores with my builds and test runs, so executions per
  second are lower than an idle machine would give, most for the slow
  targets. No target was run in parallel with itself.
- `relay_fcm` was run twice: the first run stopped on a wrong check in the
  target (below) and was repeated for the full hour after the fix.
- Smoke runs of 20 to 60 s before the long runs found six mistakes in my own
  targets (listed below); none was a bug in production code.
- Clippy on the workspace is clean. `cargo test --workspace --locked
  --no-fail-fast` on this sandbox (root, no special mode bits) fails 30 tests
  in `acd` (`cli`, `external_cli`, `harness`, `hooks_cli`) and `acd-inbox`
  `server`; the same names fail on the base commit (the merge of the relay
  branch and main, before this work). One more test,
  `a_view_that_stops_reading_is_disconnected_and_its_countdowns_cancelled`,
  failed in the run that shared the machine with the fuzzers and passed three
  times on an idle one; the base branch has since replaced its sleeps with
  barriers (`c03dae9`).

## Results

Columns: executions, executions per second (average), coverage in edges
(`cov`) at the start, at 10 % and at the end of the run, last time new
coverage appeared (as % of the run), corpus size at the end, peak RSS.
Crashes, hangs, out-of-memory and invariant violations in the final runs:
**none, for all 18 targets.**

| Target | Executions | exec/s | cov start / 10 % / end | last new cov | Corpus | Peak RSS |
| --- | --- | --- | --- | --- | --- | --- |
| `protocol_frames` | 27,488,760 | 7,633 | 14,736 / 18,273 / 19,325 | 100 % | 5,508 / 2.1 MB | 676 MB |
| `pairing` | 67,353,651 | 18,704 | 636 / 743 / 744 | 11 % | 405 / 21 KB | 561 MB |
| `hook_mapping` | 1,055,130 | 293 | 3,571 / 4,277 / 4,595 | 100 % | 2,246 / 5.2 MB | 658 MB |
| `t3_map` | 25,910,585 | 7,195 | 4,748 / 7,150 / 8,484 | 97 % | 5,183 / 3.3 MB | 639 MB |
| `herdr_screen` | 4,950,223 | 1,374 | 2,026 / 3,286 / 4,481 | 96 % | 4,110 / 1.7 MB | 586 MB |
| `inbox_protocol` | 360,494 | 100 | 3,513 / 3,584 / 3,619 | 89 % | 916 / 120 KB | 519 MB |
| `text_rules` | 1,828,390 | 507 | 8,218 / 8,672 / 8,755 | 100 % | 2,361 / 222 KB | 595 MB |
| `relay_wire` | 89,528,400 | 24,862 | 1,838 / 2,136 / 2,185 | 78 % | 2,240 / 134 KB | 577 MB |
| `relay_handle` | 39,410,138 | 10,944 | 2,250 / 2,354 / 2,428 | 84 % | 1,520 / 120 KB | 588 MB |
| `relay_challenge` | 9,873,379 | 2,741 | 852 / 868 / 868 | 1 % | 557 / 221 KB | 575 MB |
| `relay_limiter` | 46,589,853 | 12,938 | 427 / 428 / 428 | 0 % | 436 / 124 KB | 476 MB |
| `relay_fcm` (second run) | 10,587,223 | 2,940 | 579 / 2,169 / 2,455 | 95 % | 1,740 / 497 KB | 503 MB |
| `relay_http` | 5,048,523 | 1,401 | 5,225 / 5,651 / 6,115 | 99 % | 932 / 76 KB | 431 MB |
| `relay_client` | 16,159,589 | 4,487 | 2,723 / 3,162 / 3,480 | 99 % | 5,788 / 1.5 MB | 927 MB |
| `idem_table` | 9,338,439 | 2,593 | 802 / 822 / 845 | 57 % | 805 / 589 KB | 606 MB |
| `registry_keys` | 2,345,512 | 651 | 1,500 / 1,526 / 1,536 | 30 % | 1,073 / 527 KB | 568 MB |
| `notify_gate` | 327,652 | 90 | 473 / 498 / 500 | 15 % | 372 / 47 KB | 865 MB |
| `hook_question` | 1,088,537 | 302 | 2,397 / 2,469 / 2,487 | 71 % | 1,421 / 195 KB | 561 MB |

"Corpus" is the number of inputs and their total size in the work directory.
`relay_limiter`, `relay_challenge`, `pairing` and `notify_gate` stopped finding
new coverage early (a small state machine, saturated by the model's own
bounds); `protocol_frames`, `hook_mapping`, `text_rules`, `t3_map` and the
relay's HTTP and client targets were still growing when time ran out, so
their numbers are a floor, not a plateau. `notify_gate` and `inbox_protocol`
are slow per execution (a 700-key flood per step; large views) and covered
little of their input space.

### Corpus kept

`fuzz/corpus` is 2.2 MB in all (limit 4 MiB), 7,041 files. For each target
the minimised set (`cargo fuzz cmin` of the run) was added largest-first
until the target held 160 KB or 800 files, skipping inputs over 8 KB. Replaying
the committed corpus alone reaches, for example, `hook_mapping` 3,667 edges
(run end 4,595), `relay_http` 6,053 (6,115), `relay_handle` 2,266 (2,428),
`t3_map` 6,014 (8,484). Every committed corpus replays without a crash.

## Bugs found

1. **`relay/fcm`: `expires_in` of the OAuth token response overflowed**
   (`now + ttl`, `crates/acd-relay/src/fcm.rs`). A response with
   `expires_in` near `u64::MAX` panics in a debug build; in a release build
   the sum wraps to an expiry in the past, so the token is never cached and
   every push asks Google for a new one. Found by reading the code while
   designing `relay_fcm`, not by a fuzzer: the token parse is inside an
   async method and the targets reach only public functions. Fixed with
   `saturating_add`. Regression test
   `a_huge_expires_in_neither_panics_nor_defeats_the_token_cache`
   (`crates/acd-relay/tests/fcm_token_expiry_overflow.rs`, fixture
   `tests/fixtures/fuzz/relay_fcm-token-expires-in-max.json`); it fails
   without the fix with `attempt to add with overflow`.

No fuzzer-found crash, panic, hang, out-of-memory or invariant violation in
production code, in 60 minutes per target.

## Mistakes in my own targets (found by the smoke runs)

All were wrong oracles, fixed in the targets; listed because they show what
the real code does:

- `relay_wire`: serde reads a struct from a JSON array too (`["99","x"]`
  parses as a wake request). Not a hole (still exactly the documented fields),
  but the wake, register and confirm bodies are accepted in array form.
- `relay_limiter` (and the same in `proptest_limits`): a key that was dead and
  came back may find the map full of dead entries nobody swept yet, and go to
  the overflow bucket; only the first `cap` distinct keys are certain to be
  resident.
- `registry_keys`: a dialog marked gone while its request is still open stays
  marked after the request resolves, so a replay of a reusable key then
  reopens it (intended); a key's state is dropped with its agent only after
  that agent's request resolved.
- `hook_question`: a read-only question keeps its options (shown as
  information); only the size bound removes them.
- `relay_fcm`: a device address containing the word "approve" is not free text
  in the wake body; `Debug` of an address prints its length only.
- `relay_handle`: a message check that matched its own text.

## Observations, not changed (for the follow-ups)

- **Registry `keys` are never evicted** while the agent lives: one entry per
  distinct `(agent, key)`, kept after the request resolves so a replay is
  ignored. A Claude agent that runs for weeks adds one per prompt id. The
  fuzz target could not see the size (private); a cap would need a rule for
  which resolved keys may be forgotten without letting a replay reopen one.
- **The idempotency table key is `(client id, command id)`, without the
  source.** A second source that sends the same client id and command id
  gets `Wait`, `Done` (same payload hash) or `Mismatch` for the first
  source's entry; it cannot evict it and is charged only for new entries.
  Exploiting it needs the other source's client id, command id and payload
  hash. Read from the code, not tested.
- Not fuzzed: `Config::parse` (reads files from disk), the OAuth token parse
  and `ServiceAccount::from_json`, `WakeSender` response classification
  (private; needs a socket). The hyper request-head parser is exercised
  through `relay_http` only as far as the relay's own settings go.

## Proptest suites (new files)

- `acd-relay/tests/proptest_limits.rs`: limiter equals a window model with
  boundary times (59/60/61, 3599/3600/3601 s), sweep changes nothing, size is
  bounded, a live key is never forgotten under a flood, exact edges.
- `acd-relay/tests/proptest_handle.rs`: seal/open round trip, expiry edge,
  every single-bit flip, cut and extension refused, other key sets, no text
  nobody sealed is accepted.
- `acd-relay/tests/proptest_challenge.rs`: the table equals a model under
  interleaved issue, confirm, cancel and sweep; single use; a threaded run.
- `acd-core/tests/proptest_idem.rs`: the table after #106 equals a model of
  its quotas; a delivered entry lives to its TTL; one source cannot push
  another out or block it; TTL edge.
- Each suite was checked against a deliberate mutation of the code under test
  (limiter `live()` without the hour clause, challenge burn at attempt 4,
  idempotency TTL halved): all failed as they should; the code was restored.

## Reproduce

```sh
cd fuzz
cargo +nightly fuzz run relay_http corpus/relay_http -- -max_total_time=3600 -timeout=20 -rss_limit_mb=2048
cargo test -p acd-relay --test proptest_limits --test proptest_handle --test proptest_challenge
cargo test -p acd-core --test proptest_idem
```

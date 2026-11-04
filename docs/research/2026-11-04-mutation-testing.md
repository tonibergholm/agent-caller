# Mutation testing of the security-critical crates (2026-11-04)

Tool: `cargo-mutants 27.1.0`, run per crate. New tests live in
`mutants_<crate>.rs` files (and `src/mutants_*.rs` for private items); each
test names the mutant it kills. Run with the harness's own test timeout
(auto-set from the unmutated baseline), `-j3`, on Linux.

A **timeout** means the mutant made the tests hang or run past the baseline
time: cargo-mutants counts it as detected, and so does this record. A
**missed** mutant is one every test passed against.

## Second round: the code #102 and #106 changed (acd-core, adapters)

Scope: `crates/acd-core/src/{idem,registry,engine,server}.rs`, which hold the
idempotency table, per-source quotas, registry eviction of resolved keys, the
`Origin` plumbing and the id bound; and the question read-only gate in
`claude_question` (hooks) and `request_obs` (T3).

| Run | Mutants | Caught | Missed | Timeout | Unviable |
| --- | ---: | ---: | ---: | ---: | ---: |
| acd-core, `origin/main` (before this PR's tests) | 357 | 233 | 31 | 25 | 68 |
| acd-core, this PR's first-round tests | 357 | 253 | 9 | 27 | 68 |
| acd-core, plus `mutants_core_quota.rs` and `mutants_pool.rs` | 357 | 259 | 3 | 27 | 68 |
| question gate (`claude_question`, `request_obs`), before | 42 | 39 | 1 | 0 | 2 |
| question gate, plus `mutants_adapters_t3.rs` | 42 | 40 | 0 | 0 | 2 |

The third acd-core row is derived: the nine mutants missed by the second row
were rerun with the new tests (80 mutants in the functions that contain them;
3 missed, all listed below as equivalent), and the six killed ones move from
missed to caught. The question-gate run was made once, with the first-round
tests, and found one miss; the mutant was killed by hand (the guard replaced
by `true` makes the new test fail with an index panic) and was not rerun.

### Killed by the new tests

| Mutant | Test |
| --- | --- |
| `idem.rs` `conflicts`: `&&` -> `\|\|` | `the_same_payload_is_not_a_conflict` |
| `idem.rs` `conflicts`: `<` -> `<=` | `an_entry_exactly_ttl_old_no_longer_conflicts` |
| `idem.rs` `admits`: `by_origin < l.local` -> `<=` | `the_local_quota_is_exact` |
| `registry.rs` `cut_cwd`: control-character guard -> `false` | `control_characters_in_a_cwd_count_as_escapes` |
| `registry.rs` `bounded`: `>` -> `>=` | `a_request_of_exactly_40_kib_is_kept` |
| `server.rs` `SourceSlot::drop`: `== 0` -> `!= 0` | `a_released_slot_leaves_the_others_counted` |
| `t3/map.rs` `request_obs`: guard `!questions.is_empty()` -> `true` | `a_question_request_without_questions_is_not_answerable` |

### Judged equivalent (not killable)

- `engine.rs` `apply_bounded`, delete the `RequestSeen` arm: the registry
  ignores a request for an agent it does not hold, and no agent with an id
  over `MAX_ID_BYTES` can exist because the `AgentUpsert` arm drops it. The
  arm is defence in depth.
- `engine.rs` `apply_bounded`, delete the `AgentMigrated` arm: `migrate`
  returns early unless both agents exist, and the target cannot have an
  oversized id for the same reason. A test that tried (migrate to a 129-byte
  id) passes with and without the arm, so it was not kept.
- `registry.rs` `request_seen`, `k.generation > 0` -> `>= 0`: a key state is
  only created by `claim_key`, which bumps the generation to at least 1, or
  copied by `migrate`; a generation of 0 never occurs.

### Remaining after the new tests, not security-relevant

The first run also reported 22 further misses on `origin/main` that the
first-round tests already closed (for example `Txn` accessors, `LogLimiter`
methods and `Registry::fallback_ended`); they are not repeated here.

### Real bugs found

None in this round. No `#[ignore]`d reproducer was needed.

## First round: tests added per crate

The first round added mutation-driven tests to `acd-securefs`,
`acd-protocol`, `acd-core`, `acd-client`, `acd-voice` and the hook paths,
tickets and T3 detail code in `acd-adapters`. Per-crate mutant counts for
that round were not recorded when the runs were made and were not rerun for
this update, so this file does not state them. The one finding from that
round is recorded as an ignored test: the error kind for a symlinked
directory in `acd-securefs` (`617bea4`).

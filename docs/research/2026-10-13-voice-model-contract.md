# Voice model contract check (plan Task 5)

Date: 2026-10-13. Mac: Apple M3, macOS 27.0.1, Xcode 27.0 (27A266a), Swift 6.4.
`model.state()` before the run: `Available` (Apple Intelligence on).
Plan: `docs/superpowers/plans/2026-10-13-voice-step3-4.md`, Task 5, decision V18.

Command (by hand; the feature keeps the Swift bridge out of the normal test
binary, and the rpath lets this binary find the Swift runtime, which debug
builds reference as `@rpath`):

```sh
RUSTFLAGS="-C link-arg=-Wl,-rpath,/usr/lib/swift" cargo test \
  --manifest-path app/src-tauri/Cargo.toml --locked --features model-contract \
  model_contract -- --ignored --nocapture --test-threads=1
```

The test starts the real bridge, routes 20 phrases through
`acd_voice::router::route` with `MacIntentModel` against a fixed snapshot
(minos/fix: a permission "Fix the build"; minos/docs: a working Codex
"Update the docs"; studio/deploy: a question with options Staging and
Production), and prints tags only. The "plan" column is `ModelPlan`'s own
`Debug`: the step, which fields are set, the text's length. No model text
is shown or kept.

## Result

**passed 5/20, schema-valid 20/20.** The check expects at least 18/20, so
the test fails. This is a finding, not a flake: the three runs below agree.

| Run | First (cold) | Warm p50 | Warm p95 | Passed |
|---|---|---|---|---|
| 1, first run after the build | 3950 ms | 1237 ms | 1331 ms | 5/20 |
| 2 (the table below) | 2648 ms | 1215 ms | 1337 ms | 5/20 |
| 3, after 5 idle minutes | 3016 ms | 1180 ms | 1287 ms | 5/20 |

| # | Phrase | Result | Route / refusal | Model plan | ms |
|---|---|---|---|---|---|
| 1 | tell the minos codex to update the readme | FAIL | refused / DidNotUnderstand | step: Send, agent: true, request: true, option: false, text 17 chars, machine: false, candidates: None | 2648 |
| 2 | ask the codex to run clippy | FAIL | refused / DidNotUnderstand | step: Send, agent: true, request: true, option: false, text 4 chars, machine: false, candidates: None | 1127 |
| 3 | send run the tests to the codex on minos | FAIL | refused / TargetMismatch | step: Send, agent: true, request: true, option: false, text 35 chars, machine: false, candidates: None | 1271 |
| 4 | tell the build agent on minos to try again | FAIL | which_one | step: Send, agent: true, request: true, option: false, text 9 chars, machine: false, candidates: None | 1178 |
| 5 | answer the deploy question with staging | pass | confirm | step: Answer, agent: true, request: true, option: true, text: None, machine: false, candidates: None | 1160 |
| 6 | for the deploy, choose production | FAIL | none | step: Select, agent: true, request: true, option: true, text 16 chars, machine: false, candidates: None | 1306 |
| 7 | tell studio to deploy to staging | pass | confirm | step: Answer, agent: true, request: true, option: true, text 17 chars, machine: false, candidates: None | 1304 |
| 8 | approve the build on minos | FAIL | which_one | step: Answer, agent: true, request: true, option: true, text 3 chars, machine: false, candidates: None | 1275 |
| 9 | deny the minos request | FAIL | none | step: Cancel, agent: true, request: true, option: true, text: None, machine: false, candidates: None | 1152 |
| 10 | read the minos request | FAIL | which_one | step: Read, agent: true, request: true, option: false, text: None, machine: false, candidates: None | 927 |
| 11 | what is the deploy question | FAIL | confirm | step: Answer, agent: true, request: true, option: true, text: None, machine: false, candidates: None | 1196 |
| 12 | summarize minos | pass | none | step: Summarize, agent: true, request: false, option: false, text: None, machine: true, candidates: Some(2) | 1255 |
| 13 | what's waiting on studio | FAIL | none | step: Read, agent: true, request: true, option: false, text: None, machine: false, candidates: Some(2) | 1337 |
| 14 | give me a summary of everything | FAIL | none | step: Summarize, agent: true, request: false, option: false, text: None, machine: true, candidates: Some(2) | 1240 |
| 15 | show the deploy agent | FAIL | which_one | step: Select, agent: true, request: true, option: false, text: None, machine: false, candidates: Some(2) | 1238 |
| 16 | go to the codex | FAIL | refused / TargetMismatch | step: Send, agent: true, request: true, option: false, text 15 chars, machine: false, candidates: None | 1175 |
| 17 | tell claude to keep going | FAIL | none | step: Cancel, agent: true, request: true, option: true, text: None, machine: false, candidates: None | 1160 |
| 18 | what's the weather like | pass | refused / DidNotUnderstand | step: None, agent: true, request: true, option: true, text 3 chars, machine: false, candidates: None | 1314 |
| 19 | play some music | pass | refused / DidNotUnderstand | step: None, agent: true, request: true, option: true, text 15 chars, machine: false, candidates: None | 1215 |
| 20 | tell the omarchy agent to stop | FAIL | none | step: Cancel, agent: true, request: true, option: false, text: None, machine: false, candidates: Some(1) | 1196 |

## What it shows

- **The plumbing works.** The bridge starts from a test binary, the model is
  available, every reply decodes as a `ModelPlan`, and latency is fine.
- **Safety held.** No phrase produced a mutation without Confirm, and
  nothing reached a countdown. Rust refused, asked which one, or chose a
  step that changes nothing (read, select, summarize, cancel).
- **The model fills every optional field.** `agent` and `request` are both
  set in 18 of 20 plans, `option` in 9 of 20 (5 on steps that do not use
  it), and `candidates` in 5. When the two ids point at different cards,
  `validate` correctly refuses the plan as `DidNotUnderstand` (phrases 1 and
  2; for 3 and 16 the refusal is `TargetMismatch`). That is the validator doing its job on a plan
  that contradicts itself.
- **Step choice is poor for a bare prompt.** "deny the minos request" came
  back as `cancel` (9), "play some music" and "tell the omarchy agent to
  stop" as `cancel` (17, 20), "for the deploy, choose production" as `select`
  (6). Phrases 18 and 19 (`none`) were right.
- **Some expectations are strict.** Rust's own name matching asks which one
  when a machine name matches two cards (4, 8, 10, 15), and the checks do not
  allow that. The step and ids the model chose are judged separately from
  Rust's target resolution in the follow-up.

## Timeout

Warm p95 is 1.3 s. The slowest cold call was 3.95 s, so the plan's rule
(the larger of warm p95 plus 1 s and the slowest cold call plus 1 s, capped
at 8 s) gives 4.95 s: **keep `MODEL_TIMEOUT` at 5 s** at this point. (Task 5b raised it to 6 s once the model also writes a restatement.) No cold call exceeded 8 s.

## Follow-up (done: see [the quality note](2026-10-13-voice-model-quality.md))

Do not tune `INSTRUCTIONS` inside this task (plan Task 5). The result is a
contract problem for every model, so it gets its own task: a guided type
that cannot set unused fields (one case per step, each with only its own
payload), clearer instructions, and a held-out phrase set beside this one so
the fix is not fitted to these 20 phrases.

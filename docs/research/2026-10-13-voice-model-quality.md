# Voice intent model: making the on-device model usable (plan Task 5b)

Date: 2026-10-13. Mac: Apple M3, macOS 27.0.1, Xcode 27.0. Follows
[the Task 5 contract check](2026-10-13-voice-model-contract.md), which found
5 of 20 phrases routed as expected with schema-valid output every time.

## Method

One by-hand test (`model_contract`, the command is in `app/src-tauri/src/voice/model.rs`)
routes two sets of 20 phrases through the real Foundation Models bridge and
`acd_voice::router::route`, against a fixed snapshot with one agent per machine:
minos (a Claude with a permission request), studio (a Claude with a question),
forge (a working Codex). It prints tags and the shape of the model's plan,
never its words.

- **Dev set:** the 20 phrases from Task 5, remapped to the new fixture.
- **Held-out set:** 20 phrases in different words, written before any tuning. The
  change under test was judged by the held-out number, so a gain that only fit the
  dev phrases would show as a gap between the two.

Every run is one cold call plus warm calls on the same process. Numbers are one
run each, not averages, and the model is greedy (`samplingMode: .greedy`), so
repeat runs of the same build agreed to within a phrase or two.

## What each step bought

| Step | Dev | Held-out | Warm p50 |
|---|---|---|---|
| Baseline: flat struct with optional fields, terse instructions (new fixture) | 7/20 | 6/20 | 1.25 s |
| Guided enum, one case per step with only its own fields | 11/20 | 7/20 | 1.40 s |
| Longer instructions that say what each step is for | 12/20 | 10/20 | 1.65 s |
| Plus example phrases in the instructions | 11/20 | 10/20 | 1.93 s |
| Case order changed (`answer` last) | 12/20 | 11/20 | 1.88 s |
| **A one-sentence `want` before the step** | **16/20** | **16/20** | 2.19 s |
| Checks fixed for `CannotReply`; summarize note in the Swift interface | 19/20 | 18/20 | 2.23 s |
| Examples removed again (same accuracy, simpler) | 19/20 | 18/20 | 2.19 s |

The last row is what ships: the guided enum, the `want` step, and the short
instructions without examples.

## What mattered

- **A flat struct with optional fields lets the model fill every field.** In 18
  of 20 plans it set both `agent` and `request`, often to different cards. Rust
  rightly refuses a plan that contradicts itself. A case per step cannot do that.
  This also fixed the class of refusal that looked like the model being wrong.
- **The model leaned hard on `answer`.** About three quarters of the remaining
  failures after the enum were an `answer` for something that was not an answer
  ("ask the codex to run clippy", "show the deploy agent"). Instructions,
  examples and case order moved it by a phrase or two each.
- **A short restatement before the step fixed most of it** (11 and 10 to 16 and
  16). Guided generation decodes in property order, so the model first says in
  its own words what the user wants, then picks the step. `want` is never sent
  to Rust and is never kept.
- **Examples did not matter** once `want` was there, and cost tokens.

## Corrections to the checks, made after seeing failures

Four checks expected a confirmed message to an agent that has a request pending.
The model had chosen the right step and agent; Rust refused with `CannotReply`,
because a blocked agent cannot take a message (spec §6.1: anything not listed is
refused). Reading the refusal showed the check was wrong, so those four now accept
`CannotReply` as well as a confirmed step. That is the change from 16/16 to
19/18, together with the sharper summarize note. It was made after the failures
were visible, so treat the final figure as slightly generous and the 16/16 row
as the clean comparison.

## What still goes wrong

Three phrases of 40, all with a safe outcome: "give me a summary of everything"
and "how many things need me" came back as `summarize` for a machine, not for
everything (it only reads counts aloud), and "pick staging" came back as `select`
(it only moves the panel selection). Nothing the model produced was a mutation
without Confirm.

## Latency and the timeout

Warm p50 is 2.2 s and p95 2.5 s, up from 1.2 s and 1.3 s, because the model now
writes the `want` sentence. The cold first call took 3.7 s to 4.0 s. The plan's
rule (the larger of warm p95 plus 1 s and the slowest cold call plus 1 s, capped
at 8 s) gives about 5.0 s, so `MODEL_TIMEOUT` is **6 s**. A press prewarms the model
(Task 4), which shortens the first call after an idle period.

## Not done

- The sets are small (20 and 20). A 5-point difference is one phrase.
- English only; phrases are typed text, not speech-recognizer output, which has
  its own errors.
- A real-use trial in the app (the typed command field) is the next check, in the
  Task 12 manual list.

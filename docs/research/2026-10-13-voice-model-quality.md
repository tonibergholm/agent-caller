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
- **Held-out set:** 20 phrases in different words, written before any tuning. It
  started as a check against fitting the dev phrases. **It stopped being clean
  during the work:** the step descriptions and the Swift interface note were written
  after reading held-out failures, and two of the relaxed checks below are held-out
  phrases. Treat the 16/16 row (before those edits) as the only clean comparison, and
  the final 19/19 as a second development number, not a held-out one.

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
| **Shipped:** the `candidates` sentence removed from the prompt (see below), the send check tightened | **19/20** | **19/20** | 2.02 s, 2.05 s |

The last row is what ships: the guided enum, the `want` step, and the short
instructions without examples or a `candidates` sentence. It was run twice, with
the same 19 and 19 both times (cold first call 4.2 s and 2.3 s; warm p95 2.3 s and
2.4 s).


## Final per-phrase results (the shipped build, first of two runs)

**Dev set**

| # | Phrase | Result | Route | Model step |
|---|---|---|---|---|
| 1 | tell the forge codex to update the readme | pass | confirm | send |
| 2 | ask the codex to run clippy | pass | confirm | send |
| 3 | send run the tests to the codex on forge | pass | confirm | send |
| 4 | tell the build agent on minos to try again | pass | refused / CannotReply | send |
| 5 | answer the deploy question with staging | pass | confirm | answer |
| 6 | for the deploy, choose production | pass | confirm | answer |
| 7 | tell studio to deploy to staging | pass | refused / CannotReply | send |
| 8 | approve the build on minos | pass | confirm | answer |
| 9 | deny the minos request | pass | confirm | answer |
| 10 | read the minos request | pass | none | read |
| 11 | what is the deploy question | pass | none | read |
| 12 | summarize minos | pass | none | summarize |
| 13 | what's waiting on studio | pass | none | summarize |
| 14 | give me a summary of everything | FAIL | none | summarize |
| 15 | show the deploy agent | pass | none | select |
| 16 | go to the codex | pass | none | select |
| 17 | tell claude to keep going | pass | refused / DidNotUnderstand | none |
| 18 | what's the weather like | pass | refused / DidNotUnderstand | none |
| 19 | play some music | pass | refused / DidNotUnderstand | none |
| 20 | tell the omarchy agent to stop | pass | refused / CannotReply | send |

**Held-out set**

| # | Phrase | Result | Route | Model step |
|---|---|---|---|---|
| 1 | allow the build request | pass | confirm | answer |
| 2 | decline that build request on minos | pass | confirm | answer |
| 3 | pick staging | pass | confirm | answer |
| 4 | message the codex agent to commit the changes | pass | confirm | send |
| 5 | let the docs agent know it should push | pass | confirm | send |
| 6 | tell the studio agent to use the production runner | pass | refused / CannotReply | send |
| 7 | read me the build request | pass | none | read |
| 8 | what does the deploy agent want | pass | none | read |
| 9 | how many things need me | FAIL | none | summarize |
| 10 | what's going on at minos | pass | none | summarize |
| 11 | focus the docs agent | pass | none | select |
| 12 | open the studio one | pass | none | select |
| 13 | order me a pizza | pass | refused / DidNotUnderstand | none |
| 14 | what time is it | pass | refused / DidNotUnderstand | none |
| 15 | write a poem about rust | pass | refused / DidNotUnderstand | none |
| 16 | say hello to the forge codex | pass | confirm | send |
| 17 | go ahead with the fix | pass | confirm | answer |
| 18 | no, don't allow the build change | pass | confirm | answer |
| 19 | tell the claude on studio to go on | pass | refused / CannotReply | send |
| 20 | show me the minos agent | pass | none | select |

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
as the clean comparison. Two of the four are held-out phrases ("tell the studio
agent to use the production runner", "tell the claude on studio to go on").
The pass thresholds in the test were set from these runs (17 of 20 dev, 15 of 20
held-out; the dev threshold was 18 in Task 5), so they will not catch a drop of a
phrase or two, only a real regression. The send check was tightened afterwards: a
confirmed step on a blocked agent now has to be a send or a typed answer.

## What still goes wrong

Two phrases of 40 in the shipped run, with a safe outcome: "give me a summary of
everything" and "how many things need me" came back as `summarize` for a machine,
not for everything (a summary only reads counts and titles aloud). An earlier
build also mis-picked "pick staging" as `select`; the shipped one did not in
either run, which is within the run-to-run spread. Nothing the model produced was
a mutation without Confirm.

## A deliberate deviation: no `candidates` from the on-device model

Spec §5.4 lets the model return `candidates` (agent ids) when it cannot tell
which agent is meant, and Rust turns that into a "which one" list. The guided
enum has no such field: every case carries only its own payload, which is what
removed the contradictory plans. Consequences:

- The model picks the most likely agent. Rust still resolves names from the
  words, and asks which one when the transcript is ambiguous (the Rust-led
  `WhichOne`). Every model-produced mutation is still Confirm, so the user
  confirms a guessed target on the HUD instead of choosing between candidates.
- The model-led "which one" branch of `validate` cannot be reached on-device. It
  stays in Rust for a model that offers `candidates`; the wire format and
  `ModelPlan` are unchanged.
- The fixture has one agent per machine, so these 40 phrases do not measure the
  cost. Two Claude agents on one machine is the case to try in the app.
- Rust's `INSTRUCTIONS` no longer mentions `candidates` (a test pins that), so the
  model is not told to use a field it does not have.

## Latency and the timeout

Warm p50 is 2.2 s and p95 2.5 s, up from 1.2 s and 1.3 s, because the model now
writes the `want` sentence. The cold first call took 3.7 s to 4.0 s. The plan's
rule (the larger of warm p95 plus 1 s and the slowest cold call plus 1 s, capped
at 8 s) gives 5.0 s. `MODEL_TIMEOUT` is **6 s**: the plan's value rounded up for a
margin on a slower machine, not what the rule says. A press prewarms the model
(Task 4), which shortens the first call after an idle period.

## Not done

- The sets are small (20 and 20). A 5-point difference is one phrase.
- English only; phrases are typed text, not speech-recognizer output, which has
  its own errors.
- A real-use trial in the app (the typed command field) is the next check, in the
  Task 12 manual list.

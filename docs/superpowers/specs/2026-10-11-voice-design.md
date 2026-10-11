# Voice control for agent-caller

Status: draft for review, 2026-10-11 (revised after an independent
review). Extends the v1 spec (`2026-10-07-agent-inbox-design.md`), which
lists the voice router under "Not in v1". Visual language:
`docs/design/obsidian-cockpit.md`.

## 1. Purpose

Drive the inbox by voice: answer pending requests, send instructions to
agents, and hear summaries, without leaving the app you are working in.
Voice is a second input method for what the keyboard can already do. It
adds no new power over agents.

Success means:
- "approve", "deny", "option two" on the selected or only pending card
  show the proposed action in the HUD within about a second of releasing
  the key; delivery follows the normal 2 s countdown;
- "tell the minos Claude to run the tests" reaches the right agent after
  the user confirms it, or asks which one;
- "what's blocked?" is answered aloud;
- nothing is sent unless the HUD showed the complete target, action and
  payload, with a way to cancel.

## 2. Scope

### 2.1 First slice (this spec's delivery steps 1–4)

- macOS 26 or later, on a Mac that supports SpeechTranscriber. Apple
  frameworks through a Swift bridge (backend A).
- The platform-neutral core (`crates/acd-voice`).
- One step per utterance:
  - fast-path answers on the selected or only pending card;
  - model-resolved answers, sends and typed answers, always confirmed;
  - `cancel`, `read`, `select`;
  - templated summaries and alerts (no model text in speech).
- Local processing by default; an optional cloud model.

### 2.2 Later, separate plans

- Multi-step plans ("approve this and tell Claude to run clippy").
- Model-written summaries.
- Backend B (Linux and Windows: `cpal`, `vad-rs`, `transcribe-rs`,
  Ollama, Piper or system TTS). It needs a Linux or Windows app first.
- Agent chat logs (transcript browsing). A separate spec; it needs new
  acd/1 support.

Out of scope: starting new agents, wake words, always-on listening,
voice biometrics.

## 3. Principles

1. **Client only.** acd and acd/1 do not change. Voice acts only through
   the app's existing operations: `pick`, `answer_text`, `reply`
   (`agent.send`) and the 2 s undo countdown in `acd-client`.
2. **No new power.** Every step passes the same client checks as a key
   press: request id and revision, `answerable`, `free_text`, text rules
   (`acd_protocol::text`), machine online, role visibility.
3. **The model proposes, Rust decides.** The model never decides
   eligibility, target provenance or the confirmation level, and it has
   no tools. Everything it returns is checked against a frozen snapshot.
4. **Agent text is untrusted.** Agent titles, request titles, details
   and option labels are data, never instructions (§5.5).
5. **Nothing is heard unless the user holds or toggles the trigger.**
6. **Audio never leaves the machine.** Transcription is on-device only;
   there is no fallback to a server recognizer.
7. **No application-managed persistence of voice content.** Transcripts,
   plans and spoken text are never written to logs, files or settings,
   and never appear in tracing, `Debug` output, error strings or the
   frontend console. Logs carry tags, lengths, timings and outcomes
   only.

## 4. Architecture

```
 trigger ─► capture ─► transcriber ─► router ──► safety ─► executor ─► acd-client
 (keys)     (mic)      (on-device)    │  ▲        (levels)  (one step)  (pick, answer_text,
                                      ▼  │                               reply, countdown,
                                  intent model                           action outcomes)
                                  (local / cloud)
 speaker (TTS) ◄── templated summaries, read-back, alerts
```

### 4.1 Components and interfaces

The core is a Rust crate with no UI and no platform code. Platform parts
sit behind traits; the app provides the implementations.

| Part | Trait (in `acd-voice`) | macOS (A) | Linux/Windows (B, later) |
|---|---|---|---|
| Trigger | events `Start`, `Stop`, `Cancel` | listen-only `CGEventTap` for the hold key; a registered shortcut for the toggle | `handy-keys` / `rdev` |
| Capture | audio frames, level | AVAudioEngine | `cpal` + `vad-rs` |
| Transcriber | final text; partial text optional | SpeechAnalyzer + SpeechTranscriber | `transcribe-rs` |
| Intent model | (transcript, context) → plan JSON | Foundation Models, or Anthropic | Ollama, or Anthropic |
| Speaker | `say(text)`, `stop` | AVSpeechSynthesizer | Piper or system TTS |

Backend capabilities are explicit: each backend reports availability
(with a reason when unavailable), whether it streams partial text, and
its audio format. Backends that cannot stream partials still work; the
HUD then shows "listening" without live text.

`acd-voice` modules:
- `snapshot`: the presentation snapshot (§4.3) and the inbox context
  built from it. `acd-voice` has its own types and does not depend on
  `acd-client`.
- `fast`: the deterministic fast path (§5.3).
- `router`: target resolution, model prompt, plan validation (§5).
- `safety`: confirmation levels (§6.1).
- `plan`: the step type and the executor state machine (§5.6).
- `speech`: templated summary, read-back and alert text (§7).

### 4.2 Swift bridge

A Swift package compiled into the app, with a C ABI (or `swift-rs`).
- Swift owns every `Task`, `AsyncSequence` and framework object. Rust
  holds opaque session handles and receives typed callbacks.
- Every callback carries a session generation id; results from an older
  generation are discarded.
- Queues between Swift and Rust are bounded. The audio callback never
  blocks on Rust or model work.
- AppKit work runs on the main thread, as `panel.rs` does today.
- Buffer ownership, callback lifetime and cancellation are part of the
  bridge contract and its tests.

### 4.3 Presentation snapshot

The panel state that voice depends on lives in Svelte today (selection,
machine filter, collapsed Done, visible order). The frontend publishes it
to Rust as a versioned snapshot:
- selected card key;
- visible card keys in display order;
- machine filter and Done collapse state.

Voice freezes the snapshot when recording starts, together with the
`CardView` list and, per card, the machine's connection generation, the
agent id, and the pending request's id, revision and option effects.
Ordinals ("the second one") and "this" resolve against the frozen
snapshot. If a card disappears or changes before a step is enqueued, the
step is dropped; voice never retargets silently.

### 4.4 HUD

A second panel, separate from the inbox panel class, top centre of the
active screen, 18% from the top (`DESIGN.md`, floating overlays).
- It never activates the app. While listening, thinking, speaking or
  showing an error, it does not take keyboard focus either.
- It becomes key (takes keyboard focus, without activating the app) only
  while it waits for input: confirm, which-one, or a running countdown.
  Then Enter confirms, digits pick a candidate, and Esc cancels. When
  that state ends, focus returns to the previous window.
- Every input also has a clickable control in the HUD.
- Pressing the voice trigger again while a voice countdown runs cancels
  that countdown at once, before any new recognition.
- It fades 1.5 s after the step fires or speech ends. When the inbox
  panel is open, the HUD sits above it.

## 5. Commands

### 5.1 Steps

| Step | Example | Executes as |
|---|---|---|
| `answer(request, option)` | "approve", "option two", "deny the minos one" | `pick` |
| `answer_text(request, text)` | "answer: use the Hetzner runner" | `answer_text` (T3 free text only) |
| `send(agent, text)` | "tell the minos Claude to run the tests" | `reply` (`agent.send`) |
| `cancel` | "cancel", "undo that" | cancel the voice action by id |
| `read(request)` | "read the question" | speech only |
| `summarize(scope)` | "what's blocked?", "summarize minos" | speech only (templated) |
| `select(card)` | "show the omarchy one" | panel selection |

One step per utterance in the first slice.

### 5.2 Target resolution and provenance

Rust resolves the target and records how it was found (provenance):
1. **Named:** the transcript, matched in Rust, contains a machine name,
   agent kind, words from the agent title or project, or an ordinal over
   the frozen visible order. The model's opinion does not count as named.
2. **Selected:** "this", "that", or nothing named, and a card is
   selected.
3. **Only pending:** exactly one visible card needs the user.
4. **Inferred:** the model picked a target that none of the above
   produced.
5. **Ambiguous:** the HUD lists numbered candidates (digits, a click, or
   "the first"); nothing runs until one is chosen.

Visibility: subagent cards while subagents are hidden are never targets.
A card hidden only by the machine filter can be targeted only when named;
such a mutation always needs confirmation and the HUD shows its machine.
A read or select on it needs none (§6.1). A
card in a collapsed Done section, and an idle agent hidden by the idle
setting, can be the target of `send` only, and only when named; the step
always needs confirmation and the HUD shows the full target and payload.
Answers, reads and selections never target them. (Decided 2026-10-11 by
the orchestrator; revisit with the user. It replaces "cards in a collapsed
Done section are never targets".)

### 5.3 Fast path

Phrases resolved in Rust without a model, applied to a selected or only
pending target: "approve", "yes", "allow", "deny", "no", "option N",
"cancel", "undo", "read it", "next", "previous".
- Options are mapped by their effect (`allow`, `deny`), never by label or
  position, except "option N", which uses the displayed number.
- "Yes always" maps only to an option with effect `allow_always` and is
  always confirmed (§6.1). Plain "yes" never selects it.
- English first; the phrase table is per locale.

### 5.4 Model contract

Input: the transcript and a bounded context of eligible candidates only:
- agents: short id, machine, kind, title, project, state, role;
- pending requests: short id, agent id, kind, title, option ids and
  labels, `free_text`;
- the selected card id.

Each untrusted string is truncated (titles 120 characters, labels 60),
and the whole context is capped (about 4 KB). Request details are sent to
local models, and to the cloud model only when the user enables it
(§6.3).

Output: JSON with a fixed schema, one step, using only the given ids.
When unsure which agent is meant, the step carries a `candidates` list
of agent ids instead of `agent` or `request`; there is no separate
`ambiguous` step. Local models use guided generation
(Foundation Models `@Generable`); the cloud model uses a JSON schema.
The model has no tools and cannot claim provenance or confirmation.

### 5.5 Untrusted agent text

- The prompt keeps instructions and data in separate, labelled sections;
  agent strings are quoted data.
- Validation (in Rust): the ids exist in the frozen snapshot and are
  eligible; the option exists; the request is `answerable` (and
  `free_text` for `answer_text`); text passes `acd_protocol::text`.
- Every model-produced mutation (`answer`, `answer_text`, `send`) is
  confirmed (§6.1), so a schema-valid plan produced by injected text
  still needs the user to approve it with the full payload on screen.
- Tests include adversarial context (titles and labels with
  instructions), target substitution and invented payloads.

### 5.6 Executor

One step per utterance in the first slice.
- The step goes through the existing client operation and gets the normal
  2 s countdown, shown in the HUD.
- `acd-client` gains a public action-outcome stream: for each action id,
  `fired`, then `delivered`, `rejected` (with the code), `uncertain` or
  `cancelled`. The HUD reports the outcome. (Multi-step plans, later,
  advance only on `delivered`.)
- Cancel uses the voice action's id (`cancel(action_id)`), not the global
  Esc that cancels the newest action. A step already sent cannot be
  undone; cancel stops only unsent work.
- Before enqueueing, the step is checked again against the live inbox: the
  card, agent, request id and revision, and machine generation must still
  match the frozen snapshot.

## 6. Safety and privacy

### 6.1 Confirmation levels

The table is exhaustive. Any case not listed is refused.

| Level | Cases | User action |
|---|---|---|
| None | `cancel`, `read`, `summarize`, `select` | none; `cancel` acts at once, the others change nothing |
| Countdown | fast-path `answer` with effect `allow` or `deny` (or "option N") on a selected or only-pending target | none; 2 s countdown, the HUD shows the full command or question; Esc, a click, or the voice trigger cancels |
| Confirm, then countdown | every model-produced `answer`, `answer_text` or `send`; any `allow_always` option; any mutation on a target hidden by the machine filter; any `send` to a named card in a collapsed Done section or a hidden idle agent | Enter, a click, or "confirm"; the HUD shows target, action and full payload (including the whole persistent rule for `allow_always`) |
| Refused | read-only or non-answerable request; offline machine; subagent hidden; anything but a named `send` to a collapsed Done card or hidden idle agent; snapshot changed; validation failure | the HUD says why |

A fast-path "approve" is equivalent to pressing 1 on the selected card;
it carries the same risk as the keyboard, and the HUD shows the full
command during the countdown.

### 6.2 Recording

- Recording happens only while the hold key is held, or between the two
  presses of the toggle shortcut. Releasing ends it.
- A recording longer than 60 s stops on its own (a lost key-up must not
  leave the microphone on).
- A live microphone indicator shows in the HUD and the menu bar icon.
- Utterances under 0.3 s or silent are ignored.
- Locking the screen or sleep stops capture and speech.

### 6.3 Data

- Audio stays on the machine; transcription is on-device only.
- Cloud model (off by default): receives the transcript and the bounded
  context of §5.4, which includes agent and request titles, machine and
  project names and option labels; these can contain sensitive text.
  Request details only with "Include request details for the cloud
  model" (off by default). Settings states exactly which fields are sent.
  The API key is in the Keychain.
- Provider error bodies, Swift errors and model output are never logged
  or shown verbatim; the HUD shows a classified error.
- The HUD keeps the last 5 utterances in memory; they are cleared when
  voice is turned off, the screen locks, or the app quits. Model sessions
  are discarded the same way.
- This governs what agent-caller stores. Agents themselves may store what
  they receive (for example a sent instruction), and the OS may page
  memory to swap.

### 6.4 Permissions and signing

Per backend: Microphone (`NSMicrophoneUsageDescription` in the bundle),
Speech Recognition if the SpeechTranscriber path requires it, and Input
Monitoring for the listen-only event tap (hold key only). Each is
requested when voice is first turned on, with an explanation in
Settings. The toggle shortcut works without Input Monitoring.

macOS ties these grants to the app's code signature. With ad-hoc signing
every rebuild is a new identity, so grants do not survive rebuilds. Voice
development and delivery use a stable signing identity (an Apple
Development certificate), which also stops the Keychain prompt after each
rebuild. Step 2 begins with a spike on the real bundle: permission
prompts, persistence across rebuilds, SpeechTranscriber and Foundation
Models availability.

## 7. Spoken output

All spoken text is built from templates and sanitized fields; no model
text is spoken in the first slice.
- On request: `summarize` (counts and titles of what needs the user, per
  machine), `read` (the request's question, or its command for a
  permission).
- Alerts (off by default): "minos Claude needs you: bash approval".
  Alerts respect the subagent and machine visibility rules, are coalesced
  (one per 5 s), are silent while recording, and speak titles truncated
  to 60 characters.
- Esc, a click, or a new recording stops speech.

## 8. Settings

A Voice section: voice on or off (off by default); hold key (default
Right Option; Space while the inbox panel is open and no text field has
focus) and toggle shortcut; speech language (default English, others
only if SpeechTranscriber supports them); model (local, or cloud with API
key), with the list of fields sent to the cloud; "Include request details
for the cloud model"; spoken alerts on or off, voice and rate;
microphone (system default or a device).

## 9. Error handling

Nothing is sent in any of these cases.

| Situation | Behaviour |
|---|---|
| macOS older than 26, or SpeechTranscriber unsupported | Voice section shows "Not available on this Mac" with the reason |
| Permission denied | HUD names it and links to the System Settings pane; without Input Monitoring the hold key is off and the toggle still works |
| No microphone, or it disappears | recording stops, "Microphone unavailable" |
| Speech assets for the language missing | offer the system download; English until installed |
| Local model unavailable (Apple Intelligence off, model not ready) | fast path still works; other phrases: "Model unavailable" with the reason |
| Cloud model timeout (5 s) or error | same fallback; a classified error, never the provider body or key |
| Invalid plan (bad JSON, unknown ids, no step) | "Didn't understand" with the transcript; no retry loop |
| Ineligible plan (valid, but the target or option cannot take the step) | the HUD shows the specific reason (for example "Read-only request", "No such option"); no retry loop |
| Snapshot changed before the step is enqueued | step dropped, HUD says so |
| Event tap disabled by the system | hold key off until re-enabled; HUD says so |

## 10. Testing

- `acd-voice` unit tests: fast-path phrase tables (positive and
  negative), effect-based option mapping, target provenance, visibility
  rules, validation (bad ids, stale revision, text rules), the
  confirmation table (every row, plus default-deny), and templated speech.
  A fake `IntentModel` returns fixed plans, including malformed and
  malicious ones.
- Adversarial tests: injected instructions in titles, labels and details;
  target substitution; invented payloads. Each must end confirmed or
  refused, never sent at countdown level.
- Replay tests: recorded (transcript, snapshot) pairs through the router
  with the fake model give the expected step and provenance.
- `acd-client` tests for the action-outcome stream and
  `cancel(action_id)`.
- Executor tests against the fake daemon used by the existing inbox
  tests: snapshot changes, cancel, rejection and uncertain outcomes.
- Model contract tests (ignored by default, run by hand): about 20
  phrases against Foundation Models and Anthropic produce schema-valid
  plans.
- Swift bridge: XCTest smoke tests (capture starts and stops; the
  transcriber transcribes a bundled clip; synthesis runs; late callbacks
  from an old generation are dropped).
- Event tap: both Option keys, shortcut chords, secure input, tap
  disablement, sleep, lost key-up (the 60 s limit).
- App: vitest for HUD states, focus behaviour and the Settings section.
- Manual checklist on the production bundle: answer, send, cancel by key
  and by trigger, ambiguous target, summary, alert, each missing
  permission, rebuild with the stable signing identity.

## 11. Delivery steps

1. `acd-voice` core: snapshot, fast path, router with a fake model,
   safety table, templated speech; the `acd-client` action-outcome stream
   and `cancel(action_id)`; the snapshot published from the frontend; the
   HUD driven by typed text (a debug input), so behaviour is testable
   before audio.
2. Spike on the real bundle with a stable signing identity: permissions,
   their persistence across rebuilds, SpeechTranscriber and Foundation
   Models availability. Then the Swift bridge for capture and the
   transcriber, the event tap and toggle triggers.
3. Intent models: Foundation Models; the optional cloud model.
4. Spoken output: templated summaries, read-back, alerts.
5. Later plans: multi-step plans, model-written summaries, backend B.

## 12. Open questions

- Foundation Models needs Apple Intelligence enabled and the model ready.
  If it is unavailable, only the fast path and the cloud model work.
  Confirm on the user's Macs.
- SpeechTranscriber locale coverage for Finnish: check the supported
  locales on the user's Mac before offering it.
- Whether the SpeechTranscriber path requires the Speech Recognition
  permission (step 2 spike).

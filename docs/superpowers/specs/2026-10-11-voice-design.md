# Voice control for agent-caller

Status: draft for review, 2026-10-11. Extends the v1 spec
(`2026-10-07-agent-inbox-design.md`), which lists the voice router under
"Not in v1". Visual language: `docs/design/obsidian-cockpit.md`.

## 1. Purpose

Drive the inbox by voice: answer pending requests, send instructions to
agents, and hear summaries, without leaving the app you are working in.
Voice is a second input method for what the keyboard can already do. It
adds no new power over agents.

Success means:
- "approve", "deny", "option two" on the selected or only pending card
  act within about a second of releasing the key;
- "tell the minos Claude to run the tests" reaches the right agent, or
  asks which one;
- "what's blocked?" is answered aloud;
- nothing is ever sent that the user did not see in the HUD with a
  chance to cancel.

## 2. Scope

In scope:
- macOS: the Tauri app, with Apple frameworks through a Swift bridge
  (backend A).
- The platform-neutral core (`crates/acd-voice`) used by every
  platform.
- Local processing by default; an optional cloud model.

Later, separate plans:
- Backend B (Linux and Windows: `cpal`, `vad-rs`, `transcribe-rs`,
  Ollama, Piper or system TTS). It needs a Linux or Windows app first.
- Agent chat logs (transcript browsing). A separate spec; it needs new
  acd/1 support.

Out of scope: starting new agents, wake words, always-on listening,
voice biometrics, voice history on disk.

## 3. Principles

1. **Client only.** acd and acd/1 do not change. Voice acts only through
   the app's existing operations: `pick`, `answer_text`, `reply`
   (`agent.send`) and the 2 s undo countdown in `acd-client`.
2. **No new power.** Every step passes the same client checks as a key
   press: request id and revision, `answerable`, `free_text`, text rules
   (`acd_protocol::text`), machine online, role visibility.
3. **Nothing is heard unless the user holds or toggles the trigger.**
4. **Audio never leaves the machine.** Transcription is always local.
5. **Transcripts, plans and spoken text never reach logs or disk.** Logs
   carry tags, lengths, timings and outcomes only (as for T3 text).

## 4. Architecture

```
 trigger ─► capture ─► transcriber ─► router ──► safety ─► executor ─► acd-client
 (keys)     (mic)      (local STT)    │  ▲        (levels)  (steps)     (pick, answer_text,
                                      ▼  │                               reply, countdown)
                                  intent model
                                  (local / cloud)
 speaker (TTS) ◄── summaries, read-back, alerts
```

### 4.1 Components and interfaces

The core is a Rust crate with no UI and no platform code. Platform parts
sit behind traits; the app provides the implementations.

| Part | Trait (in `acd-voice`) | macOS (A) | Linux/Windows (B, later) |
|---|---|---|---|
| Trigger | `Trigger` events: `Start`, `Stop`, `Cancel` | global key monitor (hold Right Option, toggle shortcut) | `handy-keys` / `rdev` |
| Capture | `Capture`: audio frames, level | AVAudioEngine | `cpal` + `vad-rs` |
| Transcriber | `Transcriber`: partial and final text | SpeechAnalyzer | `transcribe-rs` |
| Intent model | `IntentModel`: (transcript, context) → plan JSON | Foundation Models, or Anthropic | Ollama, or Anthropic |
| Speaker | `Speaker`: say(text), stop | AVSpeechSynthesizer | Piper or system TTS |

`acd-voice` modules:
- `context`: the inbox context, built by the app from the current
  `CardView` list plus the panel's selected card. `acd-voice` has its own
  context types and does not depend on `acd-client`, so it stays testable
  and reusable.
- `fast`: the deterministic fast path (§5.3).
- `router`: target resolution, model prompt, plan validation (§5).
- `safety`: confirmation levels (§6).
- `plan`: the plan and step types, and the executor state machine (§5.5).
- `speech`: summary and alert text (§7).

The Swift bridge is a Swift package compiled into the app (via
`swift-rs` or an equivalent C ABI). It exposes only the macOS backends.
It holds no routing logic.

The app (`app/src-tauri`) wires it together: it owns the trigger, the
bridge, the executor's calls into `acd-client`, and the HUD window.

### 4.2 HUD

A second non-activating NSPanel, top centre of the active screen, 18%
from the top (`DESIGN.md`, floating overlays). It never takes focus from
the frontmost app. States: listening (level meter, live partial
transcript), thinking, confirm, which-one, counting down, speaking,
error. It fades 1.5 s after the last step fires or speech ends. When the
inbox panel is open, the HUD sits above it.

## 5. Commands

### 5.1 Steps

| Step | Example | Executes as |
|---|---|---|
| `answer(request, option)` | "approve", "option two", "deny the minos one" | `pick` |
| `answer_text(request, text)` | "answer: use the Hetzner runner" | `answer_text` (T3 free text only) |
| `send(agent, text)` | "tell the minos Claude to run the tests" | `reply` (`agent.send`) |
| `cancel` | "cancel", "undo that" | Esc |
| `summarize(scope)` | "what's blocked?", "summarize minos" | speech only |
| `read(request)` | "read the question" | speech only |
| `select(card)` | "show the omarchy one" | panel selection |

A plan has at most 3 steps. Any other shape is invalid.

### 5.2 Target resolution

In order:
1. Named: machine name, agent kind (Claude, Codex), words from the
   agent title or project, or an ordinal ("the first", "the second")
   counting cards as shown.
2. The selected card, for "this", "that", or when nothing is named.
3. The only card that needs the user, if exactly one does.
4. Otherwise ambiguous: the HUD lists numbered candidates (keys 1–9 or
   "the first"); nothing runs until one is chosen.

Hidden cards (subagents while hidden, filtered-out machines, collapsed
Done) are not targets. A step may target a card hidden only by the
machine filter if it is named explicitly; the HUD shows its machine.

### 5.3 Fast path

Phrases that resolve without a model, applied to the target from §5.2
steps 2–3: "approve", "yes", "allow", "deny", "no", "option N", "yes
always", "cancel", "undo", "read it", "next", "previous". English first;
the phrase table is per locale.

### 5.4 Model contract

Input: the transcript and a compact context:
- agents: short id (`a1`…), machine, kind, title, project, state, role
  (main or subagent);
- pending requests: short id (`r1`…), agent id, kind, title, option ids
  and labels, `free_text`;
- the selected card id.

Request details (command lines, question text) are included for local
models always and for the cloud model only when the user enables it
(§6.3).

Output: JSON with a fixed schema: `{ "steps": [...], "needs_target":
bool, "candidates": [...] }`, steps using only the given ids. Local
models use guided generation (Foundation Models `@Generable`); the cloud
model uses a tool or JSON schema.

Validation (in Rust, after the model): ids exist, the option exists for
the request, the request is `answerable` (and `free_text` for
`answer_text`), text passes `acd_protocol::text`, at most 3 steps. Each
step is pinned to the request revision seen at planning time. A plan that
fails validation is not run; the HUD shows "Didn't understand" and the
transcript.

### 5.5 Executor

Steps run in order through the existing client operations, so each step
gets the normal 2 s undo countdown shown in the HUD. Step N+1 is
submitted only after step N's action has fired. Esc or "cancel" cancels
the current countdown and drops the remaining steps. If a step's request
changed (revision) or vanished, that step and the rest are dropped and the
HUD says so.

## 6. Safety and privacy

### 6.1 Confirmation levels

The plan takes the highest level of its steps.

| Level | When | User action |
|---|---|---|
| Countdown | fast path on the selected or only pending card; `cancel` | none; 2 s countdown, Esc or "cancel" stops it |
| None | `read`, `summarize`, `select` | none (they change nothing) |
| Confirm, then countdown | "yes always" options; a target the model inferred (not named, not selected, not the only pending card); `send` or `answer_text` longer than 120 characters; plans with more than one step | say "confirm" or press Enter |
| Refused | read-only request, offline machine, hidden card, plan changed since shown | HUD shows why |

### 6.2 Recording

- Recording happens only while the hold key is held or between the two
  presses of the toggle shortcut. Releasing ends it; there is no
  continued listening.
- A live microphone indicator shows in the HUD and the menu bar icon.
- Utterances under 0.3 s or silent are ignored.

### 6.3 Data

- Audio stays on the machine. Transcription is local.
- Cloud model (off by default): receives the transcript and the compact
  context from §5.4 without request details unless "Include request
  details for the cloud model" is on (off by default). The API key is in
  the Keychain.
- No transcripts, plans or spoken text in logs or on disk. The HUD keeps
  the last 5 utterances in memory.

### 6.4 Permissions

Microphone, Speech Recognition, and Input Monitoring (only for a
modifier-only hold key). Each is requested when voice is first turned
on, with an explanation in Settings. The toggle shortcut works without
Input Monitoring.

## 7. Spoken output

- On request: `summarize` (what needs the user, per machine), `read`
  (the request's question or command).
- Alerts (off by default): a short line when a new request arrives, such
  as "minos Claude needs you: bash approval". Alerts respect the subagent
  and machine visibility rules, are coalesced (one per 5 s), and are
  silent while recording or while the user is speaking.
- Esc or a new recording stops speech.

## 8. Settings

A Voice section: voice on or off (off by default); hold key (default
Right Option; Space while the panel is open) and toggle shortcut; speech
language (default English); model (local, or cloud with API key);
"Include request details for the cloud model"; spoken alerts on or off,
voice and rate; microphone (system default or a device).

## 9. Error handling

Nothing is sent in any of these cases.

| Situation | Behaviour |
|---|---|
| Permission denied | HUD names it and links to the System Settings pane; hold-to-talk off without Input Monitoring, toggle still works |
| No microphone, or it disappears | recording stops, "Microphone unavailable" |
| Language or speech assets missing | offer the system download; fall back to English |
| Local model unavailable | fast path still works; other phrases: "Model unavailable" with the reason |
| Cloud model timeout (5 s) or error | same fallback; the key is never shown |
| Invalid plan | "Didn't understand" with the transcript; no retry loop |
| Inbox changed before a step fires | that step and the rest dropped, HUD says so |

## 10. Testing

- `acd-voice` unit tests: fast-path phrase tables (positive and
  negative), target resolution, validation (bad ids, stale revision,
  text rules, step limit), confirmation levels, summary text. A fake
  `IntentModel` returns fixed plans, including malformed ones.
- Replay tests: recorded (transcript, context) pairs through the router
  with the fake model must give the expected plan. Wrong-target cases are
  the priority.
- Executor tests against `acd-client` with the fake daemon used by the
  existing inbox tests: sequencing, cancel, revision change mid-plan.
- Model contract tests (ignored by default, run by hand): about 20
  phrases against Foundation Models and Anthropic must produce
  schema-valid plans.
- Swift bridge: XCTest smoke tests (capture starts; SpeechAnalyzer
  transcribes a bundled clip; synthesis runs).
- App: vitest for HUD states, keys and the Settings section.
- Manual checklist: answer, follow-up, cancel, ambiguous target, summary,
  alert, each missing permission.

## 11. Delivery steps

1. `acd-voice` core with the fast path and a fake model; HUD driven by
   typed text (a hidden debug input), so behaviour is testable before
   audio.
2. Swift bridge: capture and SpeechAnalyzer; hold and toggle triggers;
   permissions.
3. Intent models: Foundation Models; the optional cloud model.
4. Spoken output: summaries, read-back, alerts.
5. Backend B (Linux and Windows), with its own plan.

## 12. Open questions

- Foundation Models availability depends on Apple Intelligence being
  enabled; if it is off, only the fast path and the cloud model work.
  Confirm on the user's machines.
- SpeechAnalyzer locale coverage for Finnish: check `supportedLocales`
  on the user's Mac before offering it.
- Whether Space as a hold key inside the panel conflicts with typing in
  the answer box (it must not trigger while a text field has focus).

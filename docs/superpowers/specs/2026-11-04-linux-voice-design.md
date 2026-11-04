# Linux voice backend for the Omarchy inbox

Status: draft for review, 2026-11-04. Documents only; no code is changed by this
change. Extends the voice spec (`2026-10-11-voice-design.md`, "backend B", §2.2 and
§4.1) and the Omarchy inbox spec (`2026-10-12-omarchy-inbox-design.md`, §2.2 "Voice on
Linux"). Evidence and sources: `docs/research/2026-11-04-linux-voice-backend.md` (labels
V/C/U are used there; this spec cites sections instead of repeating them).

## 1. Decisions that are the owner's

Nothing below is settled until the owner picks. Each row has my recommendation.

| # | Decision | Options | Recommendation |
|---|---|---|---|
| O1 | Languages in the first slice | English only; English + Finnish | **English only first.** The fast-path table is `Locale::En` only; Finnish needs new phrase tables and a safety review, and rules out Moonshine. Decide before step 2, because Finnish moves the fallback engine to Parakeet v3. |
| O2 | Primary speech engine | whisper.cpp via `whisper-rs`; Parakeet v3 via sherpa-onnx; Moonshine streaming | **whisper.cpp** (§4.1), **provisional** until the step-2 bake-off on the owner's machine. |
| O3 | Hold key and toggle key | any `SUPER`-style chord | **No default is written.** Omarchy dictation owns Right Alt and Alt+Space (research §1). The installer prints a snippet; the owner chooses the keys. |
| O4 | Model downloads | explicit command only; installer prompt | **Explicit command only** (§8): `acd-inbox voice models fetch`. Never at build, install or first use. |
| O5 | Reuse voxtype's model files | read-only reuse by hash; own copy | **Own copy by default**; reuse only if the owner sets a path and the SHA-256 matches our manifest. Never reuse the voxtype daemon. |
| O6 | Licence tolerance | CC-BY attribution (Parakeet); GPLv3 engines as **child processes** (Piper, eSpeak NG); none | **Allow GPL only as an optional child process**, never linked. CC-BY only if O1 needs Parakeet. This repository is MIT. |
| O7 | Local LLM intent model in scope | fast path only; Ollama; embedded llama.cpp | **Fast path only first** (§9). The model slice is its own plan with its own 20+20 phrase contract test. |
| O8 | Run voice in-process or as a separate unit | inside `acd-inbox`; second user unit with no network | **Inside `acd-inbox` first**; the crate boundary makes the split possible later (§2.3). |
| O9 | TTS | eSpeak NG child process; Piper child process; none | **eSpeak NG** first (always small, Finnish capable); Piper as the opt-in upgrade. |
| O10 | Spoken alerts | on/off default | **Off by default**, as on the Mac (spec §7). |

## 2. Architecture

### 2.1 Where each part runs

```
 Hyprland key ─► Quickshell plugin ──(press/release/toggle)──┐
 (GlobalShortcut)   Panel.qml / Hud.qml  ◄──(hud, listening)─┤  acd-inbox socket (JSON lines,
                                                             │  $XDG_RUNTIME_DIR, 0600)
                                              acd-inbox ◄────┘
                                  Controller (acd-voice-client) ── AcdPort ── acd-client Inbox
                                       │   SpeechInput / Speaker  (acd-voice-linux)
                                       ▼
                    capture (PipeWire) → 16 kHz mono → gate → engine → ListenEvent
                    speaker: child process (eSpeak NG | Piper)
```

- `acd` and acd/1 are unchanged. The `acd-inbox` socket protocol (`PROTOCOL = 1` in
  `crates/acd-inbox/src/protocol.rs`) **does** gain messages (§5). That is a local,
  same-user protocol; it needs a version bump decision (O-impl, step 5).
- `acd-voice` (rules) and `acd-voice-client` (`Controller`) are reused as they are, except
  for the enum additions in §3.4.
- `InboxPort` is **not** implemented by the new crate. `AcdPort` already adapts `acd-client`'s
  `Inbox`; the Linux host passes the same `Inbox` the Omarchy service already owns.

### 2.2 New crate `acd-voice-linux`

Depends on `acd-voice` (traits and `VoiceText`, `ListenEvent`, `Capabilities`) and on nothing
that talks to the daemon: no `acd-client`, no `acd-voice-client`, no Tauri. Linux-only at
runtime, but the pure parts (resampler, gate, state machine, manifest verification, event
queue) compile and test everywhere, as `acd-voice` does. Heavy native dependencies are
behind **off-by-default features** (§3.2).

### 2.3 Process split (O8)

One process first: `acd-inbox` already owns the secrets, the `Inbox` and the Quickshell
socket. If the owner wants "audio code can never touch the network", a later split moves
`acd-voice-linux` behind a second user unit with `RestrictAddressFamilies=AF_UNIX` and
`IPAddressDeny=any` (plus the audio and D-Bus sockets it needs), talking to `acd-inbox` over a socket. The `BackendSink` seam (§3.3) is the
cut line. Not in the first slice.

## 3. Interface to implement

There is no single "port trait". The Swift bridge fills these seams; Linux fills the same ones.

### 3.1 Mapping from the C ABI

| Swift/C (`voice_bridge.h`) | Rust seam today | Linux equivalent |
|---|---|---|
| `vb_init(ctx, on_event)` | `bridge.rs` `Link` + pump | `LinuxBackend::start(config, sink)` |
| `vb_session_start(gen, max_ms, speech_required)` | `SpeechInput::start(gen, max) -> Result<(), ListenError>` | same trait; `speech_required` has no Linux meaning (no per-app speech permission) |
| `vb_session_stop(gen)` | `SpeechInput::stop` | same |
| `vb_session_cancel(gen)` | `SpeechInput::cancel` | same |
| `vb_query_capabilities`, `VB_EV_CAPS` | `SpeechInput::capabilities() -> Capabilities` | same; `sample_rate = 16000`, `streams_partials = false` in slice 1 |
| `vb_install_assets`, `VB_EV_ASSETS` | none | **not** a seam: model install is the explicit CLI (§8); a missing model is `Unavailable::AssetsMissing` |
| `VB_EV_PARTIAL / FINAL / ERROR` | `ListenEvent::{Partial, Final{audio, voiced}, Error}` → `Controller::speech_event` | `BackendSink::listen(ListenEvent)` |
| `VB_EV_CAPTURE` | `Controller::capture(gen, on)` | `BackendSink::capture(gen, on)`; **must** be emitted from the real device start/stop, not from `start()` returning |
| `VB_EV_OVERFLOW` | `Controller::overflow()` | `BackendSink::overflow()` |
| `VB_EV_SYSTEM` (lock, sleep) | `Controller::system_stop()` / `system_resume()` | host observes lock state as specified in §3.5 (the session's `LockedHint` property, not the `Lock`/`Unlock` signals) and `PrepareForSleep`, and calls `system_stop()` / `system_resume()` |
| `vb_tap_start/stop`, `VB_EV_HOLD`, `VB_EV_OTHER_KEY`, `VB_EV_TAP_DISABLED` | `Controller::hold(down, panel_open)`, `other_key()`, `hold_lost()` | the socket verbs in §5; `other_key` is **never called** on Linux (research §6); `hold_lost()` is called on socket disconnect |
| `vb_permission_*`, `VB_EV_PERMISSION` | `permissions.rs` | none: Linux has no per-app prompts (U on the machine). A device that cannot be opened is `ListenError::MicrophoneUnavailable` |
| `vb_model_*`, `VB_EV_MODEL*` | `IntentModel` | out of slice 1 (O7); `Controller::start(.., model: None, ..)` |
| `vb_speech_say(gen, text, voice, rate)` | `Speaker::say` | same trait; completion → `Controller::spoken(gen)` |
| `vb_speech_stop` | `Speaker::stop` (must silence **before returning**) | same: kill the child and reap it synchronously |
| `vb_speech_voices`, `VB_EV_VOICES` | `status::VoiceOption` list | `acd-inbox voice voices` (eSpeak `--voices`, Piper installed voices) |
| `VB_EV_PROBE`, `vb_probe_run` | spike only | replaced by `acd-inbox voice doctor` (§8) |

### 3.2 Traits and types (signatures only)

```rust
// acd-voice-linux. Implements acd_voice::listen::SpeechInput and acd_voice::speak::Speaker.
pub struct LinuxBackend { /* worker threads, bounded queues */ }

impl LinuxBackend {
    /// Starts the worker threads. Does not open the microphone and does no network I/O.
    pub fn start(cfg: BackendConfig, sink: Arc<dyn BackendSink>) -> Result<Arc<Self>, StartError>;
}
impl acd_voice::listen::SpeechInput for LinuxBackend { /* start, stop, cancel, capabilities */ }
impl acd_voice::speak::Speaker for LinuxBackend { /* say, stop */ }

/// Where the backend reports. Called from backend threads, never under any lock of the
/// host, never re-entrantly, must not block (same contract as `vb_event_fn`).
pub trait BackendSink: Send + Sync {
    fn listen(&self, ev: ListenEvent);          // Partial/Final/Error, generation tagged
    fn capture(&self, generation: u64, on: bool);
    fn spoken(&self, generation: u64);
    fn capabilities(&self, caps: Capabilities); // pushed when it changes
    fn overflow(&self);                          // an event was dropped (queue full)
}

// Pluggable engine seam, so the bake-off and the fallback are one-file changes.
pub trait Transcriber: Send {
    fn name(&self) -> &'static str;                         // log tag only
    fn languages(&self) -> &[Lang];
    /// 16 kHz mono f32. Blocking; runs on the transcription thread, never the audio thread.
    /// Must poll `ctl.should_stop()` at least every engine step and return
    /// `Err(EngineError::Cancelled)` or `Err(EngineError::Deadline)` promptly once it is true.
    fn transcribe(&mut self, audio: &[f32], hint: Lang, ctl: &StopToken) -> Result<Transcript, EngineError>;
}
/// Shared with the session worker. True once the generation was cancelled/superseded or its deadline passed.
pub struct StopToken { /* AtomicBool cancel + deadline Instant from an injected Clock */ }
pub struct Transcript { pub text: VoiceText, pub no_speech: Option<f32> /* engine's own signal */ }

pub trait AudioSource: Send {                // real: cpal/PipeWire. Test: WAV file.
    fn open(&mut self, device: Option<&str>) -> Result<Format, ListenError>;
    fn read(&mut self, out: &mut Vec<f32>);   // non-blocking; the producer is the callback
    fn close(&mut self);
}

pub trait Gate: Send { fn voiced(&self, audio: &[f32]) -> Duration; }   // §4.3
```

`BackendConfig` holds only settings: device name, language, model manifest path, engine
choice, voice id, `Rate`. No secrets and no text.

`Engine` features (all **off by default**, so `cargo test --workspace --locked` on Ubuntu and
macOS CI needs no CMake, no `libpipewire`, no models; research §9): `capture-pipewire`
(cpal), `engine-whisper`, `engine-onnx`. The default build contains the pure parts, a
`FakeTranscriber`, a `WavSource` and a `NullSpeaker`.

### 3.3 Concurrency and contract rules (kept from the Swift header)

1. Every method of `SpeechInput` and `Speaker` returns at once and never calls back into the
   controller synchronously (the controller calls them under its core lock).
2. Threads: **audio callback** (only pushes samples into a lock-free ring; never blocks, never
   allocates); **session worker** (resample, gate, buffer, owns the state machine);
   **transcription thread** (the only caller of `Transcriber`); **speaker thread**. Queues are
   bounded; a full queue sets an overflow latch and the host calls `Controller::overflow()`, which
   invalidates everything (fail closed), exactly as `bridge.rs` does.
3. Every event carries the generation it was started with. `start(g2)` cancels `g1` first and
   `g1` produces nothing afterward. `cancel(g)` produces no `Final`/`Error`, only `capture(g, false)`.
4. `stop(g)` always ends in exactly one `Final` or `Error` for `g`, or a `cancel`.
   `Controller::speech_event` already ignores a `Final` that arrives before release, and the
   recorder times out a missing final after `FINAL_TIMEOUT` (5 s), so the engine must also
   have its own deadline shorter than that or return `Error(Failed)`.
5. `max` is enforced by the backend as well as the controller (`MAX_RECORDING` 60 s): a
   lost release must not leave the microphone on.
6. **Transcription is supervised for every engine.** Each job carries a `StopToken` (cancel flag plus a deadline
   shorter than `FINAL_TIMEOUT`, taken from an injected clock). `start(g2)`, `cancel(g1)`, lock and shutdown set the
   token of the running job. A cooperative abort hook is an optimisation, never the guarantee: the transcription thread
   itself never blocks inside an engine call. Every engine instance runs on its own **disposable worker** (a thread, or a
   child process for an engine that cannot be unwound), and the transcription thread only waits on that worker's result
   channel and on a **watchdog**.
   - **With an abort hook** (whisper.cpp: `whisper_full_params` has `abort_callback` / `abort_callback_user_data`, checked in
     `include/whisper.h` v1.8.2; the header `whisper-rs` 0.16.0 pins is **not yet checked**, so step 2 verifies it and the
     setter): the hook reads the token and the worker is given a bounded **grace** (`ABORT_GRACE`, injected, well under
     `FINAL_TIMEOUT`) to return.
   - **Without one** (a sherpa-onnx offline recognizer, unverified): grace is zero, the job is abandoned at cancel.
   - **Watchdog:** if the worker has not returned when the grace ends (hook present but ignored or slow, or no hook), the
     instance is **abandoned**: its result and buffer are dropped on arrival (the generation check discards it), it is never
     reused, and a **replacement** instance is built (model load on a fresh worker) for the next job. Generation 2 never waits
     behind generation 1: until the replacement is ready it reports `Error(Failed)` at once, it does not queue.
   - **Bound:** at most `MAX_ABANDONED` (default 2) abandoned workers may be alive. Hitting the bound puts the transcription
     backend in a **fail-closed** state: every new job gets `Error(Failed)`, `capabilities` reports `Unavailable`
     (engine wedged), no new worker or instance is created, and only a process restart clears it. Abandoned workers that do
     return are joined and decrement the count. A worker that is a child process is killed and reaped instead of abandoned.
   - Audio: the job's buffer is zeroed and dropped when the engine call returns or when the worker is killed; a cancelled or
     abandoned job never emits `Final`.
   - **Test (both engine kinds, controlled clock, gates the test releases, no sleeps):** two doubles, one that polls the token
     and one with no hook. (a) Hook double that honours the token: returns within `ABORT_GRACE`, no abandonment. (b) Hook
     double that ignores the token and stays busy, and (c) no-hook double that stays busy: after cancel and the clock advanced
     past the grace, generation 2 gets its `Final` or `Error` while generation 1 is still blocked on its gate, generation 1
     emits nothing, the count of abandoned workers is 1, and the replacement instance serves generation 2. (d) Repeat until
     `MAX_ABANDONED` is reached: the next job yields `Error(Failed)`, no new worker starts, `capabilities` is `Unavailable`.
     (e) Releasing a gate joins that worker and its late result is dropped. (f) The deadline path returns `Error(Failed)`
     before `FINAL_TIMEOUT` on the controlled clock.
7. No text in `Debug`, errors, tracing or panics: all text is `VoiceText`; engine and library
   errors are mapped to `ListenError` codes at the boundary and the original is dropped.

### 3.5 Lock and sleep observation (fail closed)

`org.freedesktop.login1` documents `Lock` and `Unlock` (signals on the session and manager) as
**requests** to the session's locker to lock or unlock; they are not reports that the screen is
locked. A shell-initiated lock (a locker started directly, an idle daemon calling it) need not emit
them at all, and `PrepareForSleep(false)` (resume) proves nothing about whether the screen is locked.
So none of these signals is treated as state. Reference: logind D-Bus API,
<https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.login1.html> (`LockedHint`,
`SetLockedHint`, `Lock`, `Unlock`, `PrepareForSleep`). Not re-fetched in the authoring session
(freedesktop.org was not reachable); the first implementation step re-reads it and `doctor` proves the behaviour on the machine.

- **State source:** the `LockedHint` boolean property of this process's own session
  (`Manager.GetSessionByPID`, then `org.freedesktop.DBus.Properties.Get` and `PropertiesChanged`).
  It is only as truthful as the client that calls `SetLockedHint`, which is the locker or shell. It is
  an observation, not an enforcement.
- **Second source (optional, when the locker offers one):** the compositor or locker's own state signal
  (for example an `ext-session-lock-v1` surface present, or a locker-specific signal). Which of these
  Omarchy's locker provides is **unverified**; it is an open item for the hardware step. When two
  sources exist, "locked" from either wins.
- **Initial state:** read `LockedHint` synchronously before the backend accepts any `hold`. Until that read
  succeeds the state is **locked**.
- **Locked means:** no microphone is opened, any open capture is closed (`system_stop()`), no content is
  spoken (`Speaker::stop()`, alerts suppressed), and every countdown is cancelled. Unlocking resumes
  nothing by itself; a new press is required.
- **Fail closed when observation is lost:** the D-Bus connection drops, the property read errors, the
  session cannot be resolved, or `PropertiesChanged` stops arriving (the connection to the bus closes): the state becomes
  **locked** and stays so until a fresh successful property read reports `false`. Reconnect attempts re-read first.
- **`doctor` check:** locks the session through the configured locker, and confirms `LockedHint`
  turns true and false again within the check. If no locker sets `LockedHint` and no second source exists, lock state is
  unobservable and voice reports `Unavailable` rather than assuming unlocked.
- **Tests:** a fake lock-state source (no D-Bus): initial unread = locked; `Lock` signal without a hint change leaves
  state unchanged; hint true cancels capture, speech and countdowns; source error or close flips to locked; resume from
  sleep with the hint unread stays locked. The loop waits on the fake's events, not on sleeps.

### 3.4 Changes to `acd-voice` (design decision, not yet made)

`Unavailable`, `ModelUnavailable` and `ListenError` are Mac-shaped (`NotMac`, `OsTooOld`,
`SdkTooOld`, `SpeechDenied`). Options:

- **A (recommended): add variants.** `Unavailable::{NoAudioServer, ModelMissing, ModelCorrupt,
  EngineUnsupportedCpu}` and, for symmetry, `NotLinux`/`NotMac` renamed `WrongPlatform`. Each needs
  an arm in `Refusal::from` (`refusal.rs`), the HUD copy, and the TypeScript mirror in
  `app/src/lib` (the Mac Settings panel serialises `Capabilities`).
- B: reuse `AssetsMissing` for model missing/corrupt and `TranscriberUnsupported` for the rest.
  No code change in `acd-voice`, but the HUD cannot tell the user which fix applies.

Either way this is **its own commit with its own tests** (the match in `refusal.rs` must stay
exhaustive). The owner decides A or B before step 4. The Mac frontend then needs a matching
change, which cannot be verified on Linux (CLAUDE.md platform limits).

## 4. Pipeline and the recommended engines

### 4.1 Engines

- **Primary: whisper.cpp through `whisper-rs` 0.16.0**, English `base.en` or `small.en` first
  (MIT; 142 MiB/~388 MB or 466 MiB/~852 MB; research §3.1), CPU first, Vulkan for the iGPU as an
  option. Reasons: the only candidate with a stable C library, MIT licence, vendor-neutral GPU
  path, a Finnish-capable multilingual variant of the same code (O1), and the model class
  Omarchy's own dictation already ships (research §3.4). Risks: the Rust wrapper changed forge and
  has one maintainer; Whisper-family models emit stock words on silence (§4.3); fixed-window
  cost on short clips is unmeasured.
- **Fallback: Parakeet TDT 0.6B v3 int8** (25 European languages including Finnish per the
  carried-over model card), through **sherpa-onnx 2.0.1** (Apache-2.0, own runtime, Rust API) or,
  if its Rust API lacks offline transducers, `transcribe-rs` 0.3.12. Cost: ~670 MB, CC-BY
  attribution (O6), and the `ort` release-candidate or sherpa 2.0 risk (research §3.2).
- **Watch list, not planned:** Moonshine streaming (English, MIT) if live partial text in
  the HUD becomes a requirement; it is the only candidate designed for it.
- **Not used:** Vosk (stale Rust binding), faster-whisper (Python), the voxtype daemon.

The choice is made by the **step-2 bake-off** with the metrics in §10.3, not by this document.
If the owner needs Finnish (O1), start step 2 with Parakeet and Whisper-multilingual side by side.

### 4.2 Flow of one utterance

1. `start(g, max)`: open the device (`AudioSource::open`), arm the ring, start the `max` timer;
   emit `capture(g, true)` when the first buffer arrives. Errors: no audio server or device →
   `ListenError::MicrophoneUnavailable`.
2. While held: audio callback fills the ring. No ASR runs (slice 1: `streams_partials = false`,
   the HUD shows "Listening…" without text, as the spec §4.1 allows).
3. `stop(g)`: close the device first (so `capture(g, false)` precedes the result), take the
   buffer, resample to 16 kHz mono f32, trim, compute `voiced`, then run the gate (§4.3).
4. Gate passes → `Transcriber::transcribe` on the transcription thread → `Final{text, audio,
   voiced}`. Gate fails → `Final` with empty text and the measured `voiced` (the controller's
   `judge` already turns this into `Ignored::Silent`/`Empty`/`TooShort`), never a guessed word.
5. Buffers are dropped (and zeroed) after the event; nothing is written to disk.

### 4.3 The silence gate (Linux-specific safety)

`fast.rs` maps bare "yes"/"no" to countdown-level answers, and Whisper-family models can output
a short word from noise (hypothesis, research §3.3). Rules:

1. `voiced` comes from the **samples** (`Gate`), never from the ASR text. Slice 1: RMS energy over
   20 ms frames against a threshold relative to the measured noise floor; Silero VAD only if the §10.2
   noise fixtures fail.
2. No engine call unless `voiced >= MIN_VOICED` (150 ms) and `audio >= MIN_UTTERANCE` (300 ms).
3. If the engine reports a no-speech score above the configured threshold, the result is dropped.
4. Single-word results "yes"/"no" are not specially blocked here (that would hide a real bug in
   the gate); instead the fixture suite pins zero fast-path intents on non-speech audio.
5. The 2 s countdown and full-command HUD stay mandatory (spec §6.1).
6. ASR mishearing a real command is safe by construction: `fast::parse` is an exact-match table
   after `normalize`, so "option to" or "approved" do not match and fall to the model or a refusal;
   the dangerous direction is only a *different exact phrase* ("no" for "go"), and confirmation
   plus the HUD cover it. The test in §10.2 enumerates the minimal pairs.

## 5. Triggers, socket protocol and the Quickshell side

### 5.1 Triggers

Recommended: **`GlobalShortcut` in the Quickshell plugin** (Hyprland `global` dispatcher;
`pressed()`/`released()` signals; no portal; research §6) forwards `voice.hold {down}` and
`voice.toggle` over the existing socket. Fallbacks, in order: a Hyprland bind that runs
`acd-inbox voice hold-down|hold-up|toggle` against the same socket (needs the Lua release-bind
form confirmed, U); the GlobalShortcuts portal via `ashpd`; opt-in evdev.

- Do not register the same `appid:name` from both Quickshell and a portal client: Quickshell's
  own header says the last registrant crashes.
- The installer **prints** a `bindings.lua` snippet using Omarchy's `o.bind(...)` form. It never edits the file
  (the 2026-10-12 plugin research did the same).
- No chord detection: `Controller::other_key()` is never called on Linux; this is a recorded
  difference from the Mac (spec §6.2 + plan S11).

### 5.2 Socket additions (JSON lines, same socket, same peer-UID check)

Client (Quickshell) → service:

- `voice.hold {down}`, `voice.toggle`, `voice.escape` (→ `escape_listening()`), `voice.cancel {id?}`,
  `voice.confirm {id}`, `voice.choose {choice_id, index}`, `voice.shown {id}` (the HUD acknowledgement the
  controller waits `DISPLAY_TIMEOUT` = 1 s for), `voice.stop_speaking {seq?}`.
- `voice.presentation {epoch, version, selected, visible, machine_filter, done_collapsed}`
  (the existing `Presentation` struct; the Mac frontend publishes the same thing).
- `voice.enable {on}`; `voice.text {text}` (typed command, as the Mac debug input; refused while recording).

Service → client:

- `voice.hud {seq, hud}` (`acd_voice::hud::Hud` JSON, as the Tauri HUD receives), `voice.listening {on}`,
  `voice.speech_ended {seq}`, `voice.status {capabilities, model, voices}`.

Rules: a command whose reply would carry a transcript carries no transcript on the command line
or in logs; HUD frames are sent only to the connection that holds the HUD role (one at a time);
**a lost connection** calls `Controller::hold_lost()` and `escape_listening()`/`cancel(None)`, so
invisible confirmation can never authorise execution (October doc §6, spec §4.4). The `PROTOCOL`
constant and the plugin's version check move together.

### 5.3 HUD (Quickshell)

Spec §4.4 applies: never takes focus while listening/thinking/speaking/error; takes keyboard focus only for
confirm, which-one and countdown; Esc cancels; clicks everything. Whether a Quickshell window can be
non-activating with a focus handoff is **not probed** (Quickshell docs unreachable; research §7). Step 8
is that spike. If a layer-shell overlay cannot take focus on demand, the fallback is mouse-only
confirm plus the already-bound Esc binding.

## 6. Speech output

`Speaker` for Linux spawns the engine as a **child process** per utterance (eSpeak NG first; Piper opt-in),
piping PCM to a **PipeWire playback child** (`pw-cat`/`pw-play`; which one is U on the machine). There is no in-process
output stream: cpal 0.18.2 has no stream drain callback (stated by the round-3 review; not independently checked in this session), so a cpal output cannot say when the last frame was heard and
is not an option. The synthesizer exiting is **not** playback completion: PCM can still sit in the pipe, in the playback
process, or in the audio server's buffer.

- **Pipeline ownership:** one `PlaybackPipeline` per utterance owns the synthesizer child, the playback child and the pipe
  between them, started in one process group. Nothing outlives it.
- **Completion:** `spoken(g)` is emitted only when the playback child has exited 0 after its stdin closed (it drains before
  exit; the `pw-cat` drain-on-EOF behaviour is verified on the machine, §13, and if it does not hold the playback command is
  not usable), **and** the synthesizer exited. A synthesizer failure with nothing played also emits `spoken(g)` so the
  controller does not wait for the speech cap. A failure of the playback process emits `spoken(g)` too (nothing more will be heard).
- **`say(g, ..)`** stops the previous pipeline (below) before starting the next one.
- **`stop()` silences the whole pipeline and says so synchronously.** It kills the whole process group (synthesizer and
  playback), discards queued PCM, and reaps every child before it returns. It returns `Ok(())` only when the whole pipeline is
  confirmed silent (every child reaped); otherwise `Err(SilenceFailed)`. A stopped pipeline emits **no** event.
  A callback or overflow latch cannot carry this result: callbacks are asynchronous, the controller calls the speaker under
  its core lock, and `start_listening` would already have opened capture before the callback could be handled.
- **Required change to the port trait (`acd-voice/src/speak.rs`):** `fn stop(&self)` becomes
  `fn stop(&self) -> Result<(), SilenceFailed>` (a unit-like error carrying no text). The doc comment keeps "before it returns".
  Every existing implementor changes with it: the Swift-bridge adapter in `app/src-tauri` (the Swift `stop` is documented to run to
  completion before returning, so it maps to `Ok(())`; a bridge-level failure to call it maps to `Err`), and the test
  doubles in `acd-voice/src/speak.rs` and `crates/acd-voice-client/tests/speaking.rs`. The Mac change is mechanical and cannot be
  verified on Linux (CLAUDE.md platform limits).
- **Required change to the controller (`acd-voice-client`):** `Controller::silence` and `stop_speech` return that result.
  `start_listening` calls `silence` and, on `Err`, **refuses to start capture**: it does not call `input.start`, it
  invalidates voice exactly as `overflow()` does (epoch bump, recorder cancelled, HUD error), and reports a refusal through
  `listen_failed`. Other callers of `silence`/`stop_speech` (lock, disable, new speech) treat `Err` the same way: invalidate
  and open no capture until a later press re-checks. Because `start_listening` already holds the core lock across
  `silence` and `input.start`, the synchronous result is the only mechanism that can gate capture; no asynchronous event is relied on.
- **Tests:**
  1. Speaker (pipeline doubles): a fast synthesizer double (writes all PCM and exits at once) and a deliberately slow playback
     double (reads slowly and exits only when its input is drained, released by the test). Asserts: no `spoken` while playback
     runs even after the synthesizer exited; `spoken` once playback is released; `stop()` during that gap returns `Ok` only after
     both doubles are dead, produces no `spoken`, and a following capture request sees no playback process alive.
  2. Speaker, unkillable playback: a playback double that refuses to die (ignores the kill, never exits) with a fast
     synthesizer. `stop()` returns `Err(SilenceFailed)` and does not report success while the child lives.
  3. Controller (`acd-voice-client`): a `Speaker` double whose `stop` returns `Err`, speech in progress, then a hold press:
     `input.start` is **never** called (the `SpeechInput` double records zero starts), the HUD shows the error, and a later
     press after `stop` returns `Ok` starts capture normally. This test fails without the change (today `stop` returns `()` and
     capture opens).
  All synchronised on the doubles' events and a controlled clock, not on sleeps.

Playback is the part of "kill the child" still to prove on the machine (§13). Text goes to the child on **stdin**,
never on the command line (visible in `ps`). Rate maps to the engine's speed argument. Voices list from the engine.
Alerts keep the Mac rules (silent while recording, coalesced, off by default; `acd-voice` already does this).

## 7. Settings

The Mac Settings panel has no Omarchy twin yet. Slice 1 reads a TOML file next to the existing
`acd-inbox` prefs (`prefs.rs`): `enabled`, `language`, `engine`, `model`, `device`, `voice`, `rate`, `alerts`.
Voice is **off by default**. A setting never contains a transcript.

## 8. Models, packaging and privacy

- **Manifest** compiled into the binary: id, URL, size, SHA-256, licence, attribution, engine, language,
  source host. No other URL is ever contacted.
- **Commands:** `acd-inbox voice models list` (offline), `fetch <id>` (the only network use; prints the URL, size and hash
  first, requires `--yes` or a tty confirmation), `verify` (re-hash), `remove`, `doctor` (device, engine load, a canned
  WAV through the gate and engine, timings; **no transcript printed**).
- **Storage:** `$XDG_DATA_HOME/acd/voice/models/<sha256>/` created 0700, files 0600 through `acd-securefs`;
  downloads land in a temp name in the same directory and are renamed only after the hash matches.
  A hash mismatch deletes the file and reports `ModelCorrupt`; a missing file reports `ModelMissing`.
- **Load-time check:** size and a stored "verified" marker bound to size+mtime; full hash on `fetch`, `verify`, `doctor`.
  A swapped file with the same size and mtime is out of the threat model (same-user attacker already owns the daemon).
- **No silent network (policy, not isolation):** backend policy is that voice code makes no network connections and the
  only network use is the explicit `fetch` command. That is a code-review and test-by-behaviour policy (the backend crate has no
  HTTP client dependency outside the `fetch` feature, and its tests run with a fake sink). It is **not** process isolation:
  in the first slice voice runs inside the network-connected `acd-inbox` (O8), which also needs audio and D-Bus access.
  A grep for network types is not proof and is not used as one. Kernel enforcement exists only with the optional split unit
  (`RestrictAddressFamilies=AF_UNIX`, which then also needs the audio and D-Bus sockets allowed, so it is `AF_UNIX` plus the
  specific paths), not before.
- **Packaging:** native Arch package first, PKGBUILD pins crate versions and the manifest; optional features
  built into a separate package so the default package needs no GPU stack. The release workflow is out of scope here.
- **Privacy:** audio only in a bounded in-memory buffer between press and the engine's return; nothing on disk, in logs,
  journald, clipboard or argv. Third-party library logging is redirected to a null sink at startup; a log-capture
  test (as the repo already does for agent text) must show no transcript, even with debug logging on.
  This governs what agent-caller stores (spec §6.3); the OS may swap memory.

## 9. Intent model (out of slice 1)

The fast path alone covers approve/deny/option N/cancel/read/next/previous and `confirm`/`choose`. Free
speech ("tell the minos Claude to run the tests") needs an `IntentModel`. When O7 says yes, it gets its own plan,
the same `ModelPlan` JSON schema and the Mac contract test (two sets of 20 phrases, ≥ 17/20 dev and ≥ 15/20 held out,
every mutation Confirm-level), and the lesson from the Mac model work (a case per step; a one-sentence `want` first).

## 10. Test plan (no microphone)

### 10.1 Layers

1. **Pure unit tests (default CI, every OS):** resampler (known sine in/out, exact length and RMS within 1 %),
   gate (synthetic speech-like bursts vs noise at set SNRs), the session state machine with `FakeTranscriber` and
   `WavSource` (generation drop, cancel, stop-without-final timeout, cap, overflow → `overflow()`), manifest and
   hash verification with a temp file, the speaker child lifecycle with a fake `sleep`/`true` binary (kill-on-stop of the whole pipeline,
   synchronous reap, `spoken` only after playback drains or on failure, none on kill; fast synthesizer plus slow playback).
2. **Contract tests against `acd-voice-client`:** the real `Controller` with `AcdPort` over the existing fake
   daemon helpers, with `LinuxBackend` + `WavSource` + `FakeTranscriber`: a WAV "approve" → countdown HUD; silence WAV →
   `Ignored`; a second `start` cancels the first; a 70 s WAV is cut at 60 s and discarded; a late `Final` for an old
   generation changes nothing. These mirror `crates/acd-voice-client/tests/speech.rs`.
3. **Socket protocol tests in `acd-inbox`:** each new message, disconnect → `hold_lost` + cancel, HUD ack timeout fails closed,
   second HUD connection refused, log-capture shows no transcript.
4. **Engine tests (ignored by default, or behind `--features engine-whisper` plus `ACD_VOICE_MODEL_DIR`):** the golden set
   below through the real engine. CI cannot download models (research §9), so these run by hand and, optionally, in a
   cached-model job the owner opts into.
5. **Cargo hygiene:** the default workspace build must not pull CMake, libpipewire or ONNX. A CI check
   (`cargo tree -p acd-voice-linux --locked` has none of them without features) guards it.

### 10.2 Fixtures and golden transcripts

- **Fixtures** live in `tests/fixtures/voice/` as 16 kHz mono 16-bit WAV, each with a `.json` sidecar (`text`,
  `intent`, `kind`, `source`, `sha256`). Keep the set small (≤ 5 MB total).
- **Licence-clean sources:** clips synthesised at build-of-fixtures time with eSpeak NG / Piper (the tool and voice
  recorded in the sidecar), plus the owner's own recordings **only if the owner agrees** (voice is personal data;
  default is synthetic only). Third-party corpora (e.g. a command-word set) only after their licence is checked (U; not done here).
  Synthetic speech is easier than real speech, so it is a regression guard, not an accuracy claim; the real-voice set
  is the hardware step.
- **Categories:** (a) command phrases from `fast.rs` (every row), "option 1..9", `confirm`, which-one ordinals;
  (b) minimal pairs: approve/prove, deny/the-night, no/go/know, yes/guess, option two/option to, option for/four, cancel/counsel;
  (c) **non-speech: digital silence, room noise at several levels, keyboard and mouse clicks, a cough, music, a door slam,
  speech at 10 dB SNR**; (d) 0.2 s and 0.3 s clips (boundary of `MIN_UTTERANCE`); (e) a 3 s phrase with leading and trailing silence.
- **Assertions (this is where tolerance lives):**
  - Intent level, **zero tolerance:** for every clip, `fast::parse(normalize(text))` equals the sidecar `intent`
    (`None` for non-speech and for minimal pairs that must *not* match). A single wrong fast-path intent fails the suite,
    whatever the WER.
  - Non-speech: no `Final` text at all, or an empty one; > 0 hallucinated words is a failure.
  - Free text (phrases outside the fast table): normalised word error rate ≤ 10 % per clip and ≤ 5 % on average over the
    set (thresholds are placeholders to be set from the first measured run, as the Mac doc did; they will not be loosened to pass).
  - Timing: not asserted in CI; recorded by `doctor` and the hand run.
- **Goldens** are stored as the *sidecar text*, compared after `normalize` (case, punctuation, whitespace), never as a raw
  string diff. A model upgrade that changes a golden is a reviewed change to the sidecar.

### 10.3 Bake-off protocol (hand run, step 2)

Candidates: Whisper base.en, small.en (and multilingual if O1), Parakeet v3 int8, optionally Moonshine small streaming.
For each: cold load time, warm and cold release-to-text time at 1 s, 3 s, 8 s clips (p50/p95 of ≥ 30 runs), peak RSS, CPU
use, the §10.2 intent table, the non-speech table, and the fixed-window question (does a 1 s clip cost like a 30 s one).
The decision rule is stated before running: zero wrong fast-path intents on the fixtures, ≤ 1 % wrong intent on the
owner's real recordings, then lowest p95 release-to-text, with a target of about 1 s for fast-path clips excluding the
2 s countdown (research: the 10-12 doc's budget).

## 11. Work breakdown (about a day each)

| # | Step | Output | Needs hardware |
|---|---|---|---|
| 1 | Crate skeleton, `BackendSink`, `Transcriber`, `AudioSource`, `Gate`, `WavSource`, `FakeTranscriber`; resampler and RMS gate with tests; features off by default; CI `cargo tree` guard | `acd-voice-linux` builds and tests on Ubuntu and macOS | no |
| 2 | **Bake-off harness** (`doctor` bench mode, fixture runner) and fixtures; run on the owner's machine; decide O2 | measured table in `docs/research/` | **yes** |
| 3 | Session state machine + contract tests against the real `Controller` (generation, cancel, cap, overflow, stop timeout) | green contract tests with fakes | no |
| 4 | Enum additions in `acd-voice` (§3.4) with `Refusal` mapping and tests | its own commit | no (Mac TS mirror unverifiable here) |
| 5 | Capture: cpal/PipeWire `AudioSource`, device selection, `capture(g,on)` from real start/stop, loss handling | records on the machine | **yes** |
| 6 | Engine: chosen `Transcriber` (feature-gated), engine deadline, no-speech drop, log silencing + log-capture test | ignored golden tests pass by hand | **yes** |
| 7 | Models: manifest, `models list/fetch/verify/remove`, `doctor`, storage with `acd-securefs`, no-network test | explicit-download flow | download host reachable |
| 8 | Socket protocol + `Controller` wiring in `acd-inbox` (hold/toggle/presentation/confirm/choose/ack, disconnect fail-closed) | protocol tests green | no |
| 9 | Quickshell: `GlobalShortcut`, HUD window, focus-handoff spike, plugin tests (`node --test`) | working HUD | **yes** |
| 10 | Speaker child process (eSpeak NG), alerts wiring, voices list, installer snippet and docs | spoken summary | **yes** |
| 11 | Live check and write-up (below); follow-ups into the follow-ups doc by its owner | `docs/research/` record | **yes** |

Optional slices after: Piper upgrade, partials (Moonshine), Finnish tables (`Locale::Fi`), local intent model (O7), process split (O8).

## 12. Risks

1. **Hallucinated "yes"/"no" from noise** triggers a countdown approve: gate (§4.3), fixtures (§10.2), mandatory countdown.
2. **Engine unmeasured on the target CPU/iGPU:** the bake-off decides; the trait keeps the swap cheap.
3. **`whisper-rs` maintenance** (forge change, single maintainer) and **`ort` still a release candidate**: pin exact versions;
   `Transcriber` isolates both; sherpa-onnx 2.0 is brand new.
4. **Native build in CI:** features off by default; a `cargo tree` guard; a separate opt-in job later.
5. **Hold key collisions** with Omarchy dictation (Right Alt, Alt+Space): no default; documented.
6. **No chord cancel** on Linux: documented difference; cap, `hold_lost`, countdown remain.
7. **HUD focus on Wayland** may not match spec §4.4: spike in step 9; fallback is mouse-only confirm.
8. **GPL tools** (Piper, eSpeak NG): child processes only (O6); owner confirms the reading.
9. **Library logging transcripts:** redirect and prove with a log-capture test; unreviewed for whisper.cpp today (U).
10. **Protocol churn in `acd-inbox`:** version bump and plugin check land together (step 8).
11. **Finnish:** if wanted, it is a language slice (tables, review), not a model swap.

## 13. What needs the owner's Omarchy machine

Everything marked "yes" in §11, concretely: microphone capture through PipeWire and the actual device;
engine latency, memory and accuracy on the real CPU/iGPU (and Vulkan or OpenVINO if the iGPU is Intel);
the real-voice fixture recordings (consent); the Hyprland key (`GlobalShortcut` and the Lua bind) including
release order, reload and lock/sleep; the Quickshell HUD focus behaviour; `pw-cat`/playback and the TTS voices;
lock/sleep via logind; a long-run idle check (no microphone activity outside the gesture); and the no-transcript
log audit with journald.

## 14. Not done by this document

No code, no measurements, no model download, no change to `docs/superpowers/plans/2026-10-07-p1-followups.md`
(another session owns it). Open items and unverified claims are listed in research §10 and in the PR body.

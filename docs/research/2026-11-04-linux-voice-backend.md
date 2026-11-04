# Linux voice backend: research for the Omarchy side

Research date: **2026-11-04**. Documents only; no product code changed. Companion
design: [`../superpowers/specs/2026-11-04-linux-voice-design.md`](../superpowers/specs/2026-11-04-linux-voice-design.md).
It refreshes [`2026-10-12-voice-backend-b.md`](2026-10-12-voice-backend-b.md)
(three weeks old) for Linux/Omarchy only, and reads the shipped Mac backend as the
contract ([voice spec](../superpowers/specs/2026-10-11-voice-design.md),
[model contract](2026-10-13-voice-model-contract.md),
[model quality](2026-10-13-voice-model-quality.md)).

## 0. How to read this

Evidence labels used throughout:

- **V** = verified today (2026-11-04) from the cited primary page.
- **C** = carried over from the 2026-10-12 doc, **not re-verified today**.
- **U** = I could not verify; stated as an assumption or a question.

Network limits of this session: `huggingface.co` and `quickshell.org` did not
resolve (the session's allowed-network list must include them to re-check model
cards and Quickshell docs). `wiki.hypr.land` pages and the Hyprland wiki paths I
guessed on GitHub returned 404. Everything below that depends on those hosts is **C** or **U**.
GitHub pages, `raw.githubusercontent.com` and the `crates.io` API worked. Where a
GitHub page shows a day and month but no year (releases list), the year is
inferred from adjacent asset timestamps or from the 10-12 doc, and I say so.

**No accuracy figure for short command phrases exists in any primary source I
could reach.** Published WERs below are leaderboard or dictation averages. They do
not measure "approve" vs "deny". Choosing an engine on them alone would be a guess;
the design therefore makes the bake-off on the owner's machine the deciding step.

## 1. Changes since the 2026-10-12 doc

| Item | 2026-10-12 | Today | Evidence |
|---|---|---|---|
| Hyprland | 0.56.2 (2026-08-05) | **0.57.1, 2026-11-03** (0.57.0 on 28 Oct); `.conf` (hyprlang) configs get deprecation notices (0.56.1, 0.57.0); Lua is the live path. No removal date stated. | V: github.com/hyprwm/Hyprland/releases |
| xdg-desktop-portal-hyprland | 1.4.1 | **1.5.0, 28 Oct** (year inferred). 1.4.0 had "globalshortcuts: race-safe session lifetimes". | V: .../xdg-desktop-portal-hyprland/releases |
| voxtype | 1.1.0 (2026-09-24) | **1.1.1, 15 Oct 2026** (asset timestamps). New: OpenVINO engine runs Whisper on Intel CPU/iGPU/NPU; Quickshell OSD that follows the Omarchy theme. | V: github.com/peteonrails/voxtype/releases |
| whisper-rs | "0.16.0, Unlicense" | Same version and licence, but the **GitHub repository is archived (30 Jul 2025) and moved to Codeberg**; crates.io 0.16.0 (2026-03-12) points at `codeberg.org/tazz4843/whisper-rs`. Author says no plan to discontinue. | V: github.com/tazz4843/whisper-rs; crates.io API |
| whisper.cpp | v1.9.5, 2026-10-06 | unchanged, still latest | V |
| sherpa-onnx | 1.13.8 (2026-09-10) | **2.0.1, 26 Oct 2026** (major bump; crates.io `sherpa-onnx` 2.0.1 updated 2026-10-27, Apache-2.0) | V |
| `ort` | rc.9 / rc.12 | **2.0.0-rc.15, 2026-10-19; there is still no stable 2.0.0** | V: crates.io API |
| transcribe-rs | 0.3.12 | same (2026-10-02, MIT) | V: crates.io API |
| Omarchy dictation | "voxtype is installed" (user fact) | Omarchy ships three dictation backends (Superwhisper default, Voxtype, Hex); **default keys are hold Right Alt and Alt+Space toggle**, set in `~/.config/hypr/bindings.lua` | V: basecamp/omarchy `manual/11-text-extraction-dictation.md` |

Consequences: (1) the voice hold key must not default to Right Alt or Alt+Space;
(2) the October doc's `whisper-rs` "maintained" assumption needs a caveat;
(3) any ONNX path built on the `ort` crate sits on a release candidate.

## 2. What the backend has to be (from the Mac contract)

Read from code, not from the plans (details and mapping in the design):

- `acd_voice::listen::SpeechInput` (`start(gen, max)`, `stop(gen)`, `cancel(gen)`,
  `capabilities()`): capture and transcription are one seam, events return as
  `ListenEvent::{Partial, Final{audio, voiced}, Error}`; all methods return at once.
- `acd_voice::speak::Speaker` (`say`, `stop`) plus a later `Controller::spoken(gen)`.
- `acd_voice::router::model::IntentModel` (optional; fast path works without it).
- Triggers are not a trait: the host calls `Controller::hold / other_key / hold_lost /
  toggle / capture / overflow / system_stop / system_resume`.
- `InboxPort` is already implemented over `acd-client` (`AcdPort`); the backend does not touch it.
- Rules the Swift header pins and the Linux backend must keep: bounded queue plus an
  overflow latch (`VB_EV_OVERFLOW`), generation numbers on every event, stale
  generations dropped, errors are codes, never text, no callback under the controller lock,
  a 60 s recording cap, `voiced` measured by the backend.

The existing fast path (`crates/acd-voice/src/fast.rs`) maps the **bare words "yes"
and "no"** to countdown-level Approve/Deny. That matters for engine choice (§3.3).

## 3. Speech to text

### 3.1 Candidates

| Engine | Licence | Languages | Size / memory | Streaming | Rust | Notes |
|---|---|---|---|---|---|---|
| **whisper.cpp 1.9.5** (06 Oct) | MIT (V, README badge) | multilingual models include Finnish (C); `.en` excludes it | disk/RAM: tiny 75 MiB/~273 MB, base 142 MiB/~388 MB, small 466 MiB/~852 MB, medium 1.5 GiB/~2.1 GB (V, README) | `whisper-stream` example samples ~every 0.5 s, needs SDL2 (V); chunked re-decode, not true streaming | `whisper-rs` 0.16.0, Unlicense, repo moved to Codeberg (V) | CPU, Vulkan (vendor-neutral iGPU), OpenVINO encoder on Intel CPU/iGPU (V). Built-in `--vad` with Silero (V). Known to emit short words on silence/noise (C, §3.3). |
| **Parakeet TDT 0.6B v3**, ONNX int8 | CC-BY-4.0, attribution (C) | 25 European languages incl. Finnish (C) | ~670 MB int8 (C) | offline; NeMo chunked streaming exists but not via these Rust adapters (C) | `transcribe-rs` 0.3.12 (MIT, 2026-10-02, V), `parakeet-rs` 0.4.4 (MIT OR Apache-2.0, 2026-10-31, V), sherpa-onnx (V: Parakeet TDT English example script exists) | `ort` crate is rc.15 only (V). `transcribe-rs` reports ~20x real time on a Ryzen 5700X, ~5x on an i5-6500 (C). Model card could not be re-fetched. |
| **Moonshine v0.1.5** (24 Aug 2026) | MIT for streaming and English models; Community (non-commercial) for Korean/Ukrainian/deprecated (V) | English streaming tiny/small/medium; also de, es, ar, ja, zh, vi, tl. **No Finnish** (V, `available-models.md`) | 34M / 123M / 245M parameters (V); file sizes not listed there | streaming models (V) | via `transcribe-rs` (`StreamingModel`; the README example is still file-based, V) or sherpa-onnx (Moonshine v2 script, V) | English WER 12.00 / 7.84 / 6.65 % = Open ASR Leaderboard average over 8 datasets (V). |
| **sherpa-onnx 2.0.1** (26 Oct) | Apache-2.0 (V) | per model | per model | streaming and offline ASR, TTS, VAD, keyword spotting in one runtime (V) | Rust is one of 12 listed languages (V); crate `sherpa-onnx` 2.0.1 (V) | Example scripts for Parakeet TDT (en), Whisper, Moonshine v2, Silero VAD and Kokoro/VITS/Matcha TTS exist (V, `rust-api-examples` listing shows `run-*.sh` only). **U:** whether the Rust crate exposes every one of those, whether its build downloads a prebuilt native library, whether its Piper/VITS "phonemize" TTS links espeak-ng (GPL). |
| **faster-whisper 1.2.1** | MIT (C) | Whisper languages | n/a | segments only | none (Python + CTranslate2) | Not a candidate for a Rust daemon. Useful only as an offline comparator. |
| **Vosk 0.3.50** (22 Apr; year C) | Apache-2.0 engine; per-model licences (C) | no Finnish in the official catalog (C) | small English ~40 MB (C) | yes, with grammar restriction (C) | crate `vosk` **0.3.1, last updated 2024-10-27** (V): stale | Grammar-constrained decoding is attractive for a fixed command set, but the binding is two years old. |
| voxtype 1.1.1 | MIT (C) | Whisper, Parakeet, Moonshine, SenseVoice, Paraformer, Omnilingual, Cohere (V) | n/a | n/a | n/a (app, not a library) | Shared dictation daemon that types/pastes output; see §3.4. |

### 3.2 Reading the table

- **Finnish decides the engine family.** English-only: Moonshine streaming or Whisper
  `.en` fit. Finnish plus English: Whisper multilingual or Parakeet v3. The existing
  fast-path table is English only (`Locale::En`), so Finnish commands need new table
  entries and a second review of the safety rules, not just a different model.
- **ONNX path risk.** Both `transcribe-rs` and `parakeet-rs` rest on the `ort` crate, which has
  never had a stable 2.0.0 (V). sherpa-onnx brings its own runtime and is on a fresh
  2.0 line (V); a major release three weeks old is a risk of its own.
- **Whisper path risk.** whisper.cpp itself is active (V); the Rust wrapper is a thin,
  single-maintainer project that changed forge (V). Pin the exact crate and keep the
  engine behind a small trait so a different wrapper (or a thin FFI to whisper.cpp) is a
  one-file change.
- **Whisper cost on short clips (U).** Whisper encodes a fixed 30 s window, so a one-second
  command may cost close to the same encoder time as a long one on CPU. I could not find this in
  a source I reached today; the bake-off must measure it, and whisper.cpp may expose a
  shorter audio context (not checked).
- **Throughput numbers do not answer the question.** "20x real time" (C) says a
  10 s clip takes 0.5 s; our budget is a 1-3 s utterance.

### 3.3 The Linux-specific safety risk: short words from silence

On the Mac, `SpeechTranscriber` gates what it returns, and Swift measures `voiced`
from audio (`MIN_VOICED` 150 ms, `MIN_UTTERANCE` 300 ms in `listen.rs`). A
Whisper-family model has no such gate: the 10-12 doc already says "noise/silence
hallucinations need VAD and admission checks" (C), and it is widely reported that these
models output short stock phrases on silence (U: I did not find a primary source
today; treat as a hypothesis to test, not a fact). Because `fast.rs` turns a bare
"yes" or "no" into a countdown-level answer, a hallucinated "Yes." on a bumped
key or a quiet room is the worst case for this backend. Required mitigations
(carried into the design):

1. `voiced` is computed from the **samples** by a VAD in the backend, never from the ASR text.
2. No engine call is made, and no text is returned, when the voiced time is under `MIN_VOICED`, or when the engine's own
   no-speech signal says so (the backend then emits a `Final` with **empty text** and the measured `voiced`, as the design §4.2 specifies, so the controller can classify it as silent, empty or too short; never a guessed word) (U: whisper.cpp exposes a no-speech threshold; confirm the
   parameter name and behaviour on the pinned version).
3. A silence/noise/cough/music fixture set that must produce **no fast-path intent**, with zero tolerance.
4. The 2 s countdown and the full-command HUD stay mandatory (already so in spec §6.1).

### 3.4 Reusing voxtype

Omarchy's own dictation defaults to hold Right Alt / Alt+Space (V), and offers Voxtype with
a ~150 MB English base model (V). Reuse is limited to **read-only model files**, checked by
SHA-256 against our manifest, chosen by the owner. The shared daemon is not reused: the 10-12
doc found it logs full transcripts at INFO (C, source lines cited there), types or pastes
output, and has no generation-tagged in-memory transcript API. Voxtype 1.1.1 notes (V) do not
mention changing transcript logging, so that finding is unresolved, not fixed.

## 4. Text to speech (read-aloud)

Spoken text is templated and short ("minos Claude needs you: bash approval"; spec §7), so
naturalness is a comfort feature, not a requirement.

| Engine | Licence | Quality / languages | Integration | Notes |
|---|---|---|---|---|
| **eSpeak NG 1.52** (12 Dec 2024, V) | GPLv3 (C; the releases page names no licence) | robotic, tiny, English and Finnish (C) | subprocess (`espeak-ng`), kill to stop | Small, but availability per distribution and start latency are not verified or measured. As a child process the GPL does not reach our MIT code (U: owner's licence reading). |
| **Piper** (`OHF-Voice/piper1-gpl` v1.8.0, 04 Sep, V) | **GPLv3** (V, release notes: "Change license to GPLv3" at v1.3.0) | natural neural voices; Finnish "Harri medium" ~63 MB (C, HF card, not re-fetched) | subprocess; or `piper-rs` 0.2.0 (MIT crate, 2026-05-21, V: does it embed GPL code? U) | Voice models carry their own licences (C). |
| **Kokoro-82M** | Apache-2.0 (V, repo README) | 82M parameters; languages: en-US, en-GB, es, fr, hi, it, ja, pt-BR, zh (V). **No Finnish.** | Python package needs espeak-ng for out-of-dictionary English and some languages (V); sherpa-onnx has Kokoro English scripts (V) | Good English, not Finnish. |
| Speech Dispatcher 0.12.1 (C) | daemon GPL, client LGPL (C) | whatever module is configured; some modules are cloud | SSIP socket | Adds a daemon and a module-choice privacy question. Not needed. |
| sherpa-onnx TTS (VITS, Matcha, Kokoro, Kitten) | Apache-2.0 runtime (V); model licences vary | English-centric in the listing (V) | same runtime as ASR if chosen | U: espeak-ng dependency of the "phonemize" variants. |

Latency (first audio) is **not measured** for any engine; that is a hardware step.
Playback is a subprocess or a library writing to PipeWire; `pw-cat`/`pw-play`
availability on Omarchy is U.
Silence after a stop is **not** the same as the playback process being dead: PCM already handed to the audio
server or device can stay audible after the child is reaped. The design therefore requires the pipeline's
PipeWire node to leave the `running` state (gone, `idle`, `suspended` or `error`) within a bound before a stop
counts as silent; for an in-process stream, `pw_stream_flush(stream, true)` plus the `drained` event
(C: PipeWire API docs, not re-fetched this session). Whether `pw-dump --monitor` or a `pipewire-rs` registry
listener is the observer, the drain bound, and the node names `pw-cat` creates are U until measured on the machine.

## 5. Audio capture, VAD

- **cpal 0.18.2** (16 Aug 2026, Apache-2.0, V). 0.18.0 added native PipeWire and
  PulseAudio hosts behind `pipewire` and `pulseaudio` features; with several compiled in,
  selection is PipeWire, then PulseAudio, then ALSA (V). 0.18.2 fixed streams starting before
  `play()`, real-time promotion blocking the callback, and device-removal rerouting (V). MSRV for
  0.18.x not stated on the page (U); the workspace is 1.90. docs.rs cannot build the PipeWire
  feature (V), so API docs come from source.
- **pipewire-rs 0.10.1** (2026-08-19, MIT, repo on gitlab.freedesktop.org, V via crates.io).
  Lower level; keep as the alternative if cpal cannot express device routing.
- Format: capture the device format, downmix and resample off the audio callback to **16 kHz
  mono f32**. The audio callback must never block (Swift header rule).
- **VAD.** Hold-to-talk bounds the utterance, so a VAD is not needed to find the end of speech.
  It is needed to decide "was anything said" (§3.3). Options: an RMS/energy gate (no model, no
  dependency, tunable; the Mac side does the same with a dBFS threshold), Silero VAD (MIT, ~2 MB
  model, 8 and 16 kHz, "<1 ms for a 30+ ms chunk on one CPU thread", V) via ONNX, or the VAD
  built into the chosen runtime (whisper.cpp `--vad` with Silero, V; sherpa-onnx VAD, V).
  Start with the energy gate plus the engine's no-speech signal; add Silero only if the
  noise fixtures fail.
- Permissions: native PipeWire/PulseAudio access follows the logged-in session (C); there is no
  per-app prompt on Omarchy (U: check on the machine). Flatpak is out of scope.

## 6. Global push-to-talk on Wayland/Hyprland

| Option | Release events | Privilege | Status |
|---|---|---|---|
| **Quickshell `GlobalShortcut`** (Hyprland) | yes: `pressed` property, `pressed()` and `released()` signals | none; bound with `bind = <mods>, <key>, global, <appid>:<name>` | V from `quickshell-mirror/quickshell` source (`global_shortcuts/qml.hpp`). It uses `hyprland_global_shortcuts_v1` and **bypasses xdg-desktop-portal**. Doc comment warns: duplicate `appid:name` across instances **or with XDPH, the last client to register crashes**. Hyprland-only. |
| Hyprland native bind that runs a command on press and on release | yes via the release-flag bind form | none | **U today**: the wiki path 404'd. The 10-12 doc cites the Binds page. Omarchy writes bindings as Lua (`o.bind("SUPER + CTRL + L", "Lock system", "omarchy-system-lock")`, V: raw `default/hypr/bindings/utilities.lua`); the Lua form of a release bind was not found. |
| xdg-desktop-portal GlobalShortcuts | `Activated` / `Deactivated` signals; interface **version 2** (adds `ConfigureShortcuts`) (V, flatpak XML) | none | `ashpd` 0.13.13 (MIT, 2026-07-17, V). XDPH ignores `preferred_trigger` and has no picker (C); XDPH 1.5.0 not re-read. Portable beyond Hyprland, more moving parts. |
| evdev (`evdev` 0.13.2, C) | yes, plus chords | needs `/dev/input` access (group `input`): reads every keystroke | opt-in fallback only, not installed by default. |
| `rdev`, `global-hotkey` (MIT, V for rdev) | X11 only on Linux (C) | | not Wayland. |

Observations:
- With a compositor- or shell-level bind, the app never sees **other keys**, so the Mac
  "chord cancels" rule (`Hold::other_key`) has no Linux input. The remaining safeguards are the 60 s
  cap, `hold_lost()` on any disconnect, lock/sleep, and the countdown. The design says so explicitly.
- Quickshell (the Omarchy shell) already hosts the plugin, so a `GlobalShortcut` in
  the plugin forwards `pressed`/`released` over the existing `acd-inbox` socket. This needs the
  shell to have the Hyprland module available (U: not confirmed on 0.3.2; the repo mirror is
  `master`, not the 0.3.2 tag).
- Conflicts: Omarchy dictation owns Right Alt and Alt+Space by default (V). Avoid both.

## 7. HUD in Quickshell

What exists (V from `docs/research/2026-10-12-omarchy-plugin-api.md`, probed on the
owner's machine): Quickshell 0.3.2; `Socket` with SplitParser works for line JSON over a
Unix socket; peer close raises an error and `connected` can be toggled to reconnect; the
notification server is Quickshell's own. **Not probed:** a non-activating overlay window and
keyboard focus handoff for the confirm/countdown states (the spec §4.4 behaviour). The
HUD must be a plugin surface that (a) never takes focus while listening/thinking, (b)
takes keyboard focus only for confirm, which-one and countdown, (c) returns focus on exit.
That is a spike on real hardware (design step 8), and the Quickshell docs host was not
reachable here.

## 8. Intent model (optional, after the fast path)

Unchanged from the 10-12 doc (C): Ollama structured output with the real JSON schema,
Qwen3 1.7B/4B, thinking off; or an embedded llama.cpp grammar. The Mac result to beat or match is
**19/20 and 19/20** on two phrase sets with Foundation Models after the guided-enum and `want` step
(V, repo doc 2026-10-13), 2.0 s warm p50; the first contract check was 5/20. A Linux model starts
from the 5/20 situation until its prompt and schema are tuned the same way, and the Mac doc's
lesson (per-step cases, a one-sentence `want` before the step) applies. Everything the model
produces stays Confirm-level, so a weak model costs usability, not safety. Whether this is in scope
is the owner's call (design §1).

## 9. Packaging, models, privacy

- Native Arch/Omarchy package first (C). The daemon is a systemd user unit (`acd-inbox install`
  exists). Build needs: whisper.cpp needs a C++ toolchain and CMake (U, standard for the sys
  crate); cpal's PipeWire feature needs `libpipewire` development files (U). CI runs the
  workspace on Ubuntu and macOS (`CLAUDE.md`), so native-heavy dependencies must be behind
  off-by-default features (design §3.2).
- Models: never bundled in git; not downloaded unless the owner runs an explicit command; pinned by URL,
  size and SHA-256 in a manifest compiled into the binary; verified before use and on
  every load (cheap size check; full hash on install and on `voice doctor`). Hosts to allow for
  download: `huggingface.co` (Parakeet, Piper voices, many Whisper ggml files; U exact repos) and
  GitHub releases (sherpa-onnx models; V that they are distributed there).
- Privacy: audio exists only in a bounded in-memory buffer between press and the end of
  transcription and is zeroed/dropped after; no audio or transcript to disk, logs,
  journald, clipboard or a command line (spec §3.7). Third-party library logging (whisper.cpp
  prints to stderr by default in many builds; U) must be redirected to a null sink and the
  log-capture test must prove it. No network access in the voice process after install: enforce
  with a `systemd` `IPAddressDeny=any`/`RestrictAddressFamilies=AF_UNIX` on a separate voice
  unit if voice is split into its own process (design §2.3, owner's call).

## 10. Not verified (carry into the PR)

- Parakeet v3 licence, language list, int8 size, Kokoro and Piper voice cards: HF host blocked (C only).
- Quickshell non-activating overlay API, keyboard-focus behaviour, GlobalShortcut availability on 0.3.2.
- Hyprland release-bind syntax in the current Lua config.
- All latency, memory and accuracy on the owner's CPU/iGPU; short-command accuracy for every engine.
- sherpa-onnx Rust API coverage and build-time behaviour; its TTS dependency on espeak-ng.
- Whisper no-speech threshold name, initial-prompt biasing, shorter audio context on the pinned version.
- Whether PipeWire gives a mic permission prompt on Omarchy; `pw-cat` availability.
- How long a stopped `pw-cat` stream stays `running` in the PipeWire registry (sets the drain bound), and the node name it gets.
- Years inferred from asset timestamps: whisper.cpp 1.9.5, Moonshine 0.1.5 (stated in asset
  timestamps), XDPH 1.5.0, voxtype 1.1.1, sherpa-onnx 2.0.1, Hyprland 0.57.1/0.57.0.

## Source index (accessed 2026-11-04 unless marked C)

- whisper.cpp: <https://github.com/ggml-org/whisper.cpp/releases>, <https://github.com/ggml-org/whisper.cpp/blob/v1.9.5/README.md>
- whisper-rs: <https://github.com/tazz4843/whisper-rs>, <https://crates.io/api/v1/crates/whisper-rs>
- transcribe-rs: <https://github.com/cjpais/transcribe-rs>, <https://crates.io/api/v1/crates/transcribe-rs>
- parakeet-rs: <https://crates.io/api/v1/crates/parakeet-rs>; `ort`: <https://crates.io/api/v1/crates/ort>
- Moonshine: <https://github.com/moonshine-ai/moonshine/releases>, <https://github.com/moonshine-ai/moonshine/blob/v0.1.5/docs/models/available-models.md>
- sherpa-onnx: <https://github.com/k2-fsa/sherpa-onnx>, <https://github.com/k2-fsa/sherpa-onnx/releases>, <https://github.com/k2-fsa/sherpa-onnx/tree/master/rust-api-examples>, <https://crates.io/api/v1/crates/sherpa-onnx>
- Vosk: <https://github.com/alphacep/vosk-api/releases>, <https://crates.io/api/v1/crates/vosk>
- Silero VAD: <https://github.com/snakers4/silero-vad>
- Kokoro: <https://github.com/hexgrad/kokoro>; Piper: <https://github.com/OHF-Voice/piper1-gpl/releases>, <https://crates.io/api/v1/crates/piper-rs>; eSpeak NG: <https://github.com/espeak-ng/espeak-ng/releases>
- cpal: <https://github.com/RustAudio/cpal/releases>, <https://crates.io/api/v1/crates/cpal>; pipewire-rs: <https://crates.io/api/v1/crates/pipewire>
- Quickshell GlobalShortcut: <https://github.com/quickshell-mirror/quickshell/blob/master/src/wayland/hyprland/global_shortcuts/qml.hpp>
- Portal: <https://github.com/flatpak/xdg-desktop-portal/blob/main/data/org.freedesktop.portal.GlobalShortcuts.xml>; `ashpd`: <https://crates.io/api/v1/crates/ashpd>
- Hyprland: <https://github.com/hyprwm/Hyprland/releases>; XDPH: <https://github.com/hyprwm/xdg-desktop-portal-hyprland/releases>
- Omarchy: <https://raw.githubusercontent.com/basecamp/omarchy/master/manual/11-text-extraction-dictation.md>, <https://raw.githubusercontent.com/basecamp/omarchy/master/default/hypr/bindings/utilities.lua>
- voxtype: <https://github.com/peteonrails/voxtype/releases>
- Carried over (C): everything cited in [`2026-10-12-voice-backend-b.md`](2026-10-12-voice-backend-b.md).

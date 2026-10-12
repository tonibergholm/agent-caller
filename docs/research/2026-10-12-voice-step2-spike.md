# Voice step 2 spike: permissions and SpeechTranscriber in the signed bundle

Date: 2026-10-12. Mac: macOS 27.0.1 (26A434), Xcode 27, Swift 6.4. Bundle:
`/Applications/Agent Caller.app`, `dev.agentcaller.inbox`, signed with
"Apple Development: Toni Bergholm (RLYXB8JS76)". Plan:
`docs/superpowers/plans/2026-10-12-voice-step2.md`, Tasks 1A and 1B (probe
at 107d9e7).

Method note: grants were reset with `tccutil reset` for Microphone,
SpeechRecognition and ListenEvent, and the app launched with `open`. The
user ran the probe steps; the reports pasted back were taken after the
grants, so prompt order is from the user's recollection, not from reports.

## Signature

- Authority chain: Apple Development: Toni Bergholm (RLYXB8JS76) → Apple
  Worldwide Developer Relations Certification Authority → Apple Root CA.
  TeamIdentifier `XKZ37BLP9D`.
- `CodeDirectory v=20500 ... flags=0x10000(runtime)`: hardened runtime on.
- Entitlements: `com.apple.security.device.audio-input = true`.
- Info.plist: both usage strings present, `LSMinimumSystemVersion = 13.0`,
  `LSUIElement = true`.

| Build | CDHash | Designated requirement |
|---|---|---|
| Step 1 (0.1.0) | `6ccbdd9ff41f562be36957a84358abf5a050f5db` | `identifier "dev.agentcaller.inbox" and anchor apple generic and certificate leaf[subject.CN] = "Apple Development: Toni Bergholm (RLYXB8JS76)" and certificate 1[field.1.2.840.113635.100.6.2.1] /* exists */` |
| Step 4 (0.1.1) | `a82b30c9a30f16b6c8060dc1a2eb1d37864c8f1e` | identical to Step 1 |

## Prompts

| Permission | Prompt appeared on | Prompt text | State after |
|---|---|---|---|
| Microphone | "2. Ask for the microphone" (user) | not captured | granted |
| Speech Recognition | "5. Ask for Speech Recognition"; the user thinks none appeared during the earlier live run | not captured | granted |
| Input Monitoring | "6. Input Monitoring" | not captured | granted |

## SpeechTranscriber in the bundle

| Check | Result |
|---|---|
| `isAvailable` | true |
| Supported / installed English locales | 45 supported; English supported and installed: en_AU, en_CA, en_GB, en_IE, en_IN, en_NZ, en_SG, en_US, en_ZA |
| Resolved locale | en_US |
| Live run, Speech Recognition not determined | not captured as a report (see method note) |
| Speech Recognition prompt during that run | user: "I think not" |
| Clip run: elapsed ms, matched | 76 ms, matched true (20 chars) |
| Live runs, all granted | run A: matched false, 33 chars (extra words), partials 10, first partial 4086 ms, stop-to-final 189 ms. Run B: matched true, 17 chars, partials 4, first partial 4083 ms, stop-to-final 188 ms. After rebuild: matched true, partials 6, first partial 4089 ms, stop-to-final 100 ms |

## Event tap

| Check | Result |
|---|---|
| Tap created after the grant, without relaunch | yes (`tap_created: true`, no relaunch) |
| Right Option down / up counts (3 presses) | 3 / 3 (also after the rebuild). Two earlier runs counted 0 / 0; most likely Secure Keyboard Entry was still on or the key was not held in the 10 s window (not confirmed) |
| Under Secure Keyboard Entry | not cleanly measured; consistent with the 0 / 0 runs |

## Persistence across rebuild and reinstall

| Permission | Before | After rebuild | Prompted again |
|---|---|---|---|
| Microphone | granted | granted | no |
| Speech Recognition | granted | granted | no |
| Input Monitoring | granted | granted (tap created, 3/3) | no |

The CDHash changed and the designated requirement did not, so the result
counts.

## Outcomes for the plan

- S14 `SPEECH_PERMISSION_REQUIRED`: **true**. The run that would have
  shown SpeechTranscriber working without the grant was not captured, so
  the app asks for Speech Recognition (harmless if not strictly needed).
- S17 `TAP_NEEDS_RELAUNCH`: **false**. The tap was created right after
  the grant without a relaunch.
- S22 `Assets.partialsStream`: **false**. Partials arrive (4–10) but the
  first one only at about 4.08 s of a 4 s capture, i.e. at release, so
  the HUD shows "listening" without live text (spec §4.1 fallback).
  Latency from stop to final text: 100–190 ms.
- S22 stop conditions: grants survived the rebuild: **yes**; live
  transcription worked with every permission granted: **yes**. No stop
  condition hit.
- API names that differ from the plan: none reported by Task 1A.
- Foundation Models in the bundle (probe 7): **available**. The CLI check
  also found `available`.
- Probe serialization: a second click while a probe ran returned
  `"error": "busy"` (S20 holds).

## LiveTests (Task 5A)

Run by hand: `swift test --package-path app/src-tauri/swift --filter LiveTests`.
- `testTranscribesASpokenClip` (a `say` clip): passed in 1.53 s. The text contained "approve" and the audio and voiced durations matched. No early final was lost although `results()` subscribes after `analyzer.start`, so the subscribe ordering stays as is.
- `testCancelEndsAWaitingFinish`: passed in 0.09 s.
- `testCaptureStartsAndStops`: skipped, because the microphone is not granted to the test runner. No prompt was triggered.
- Speech Recognition permission did not block the clip test in this runner.

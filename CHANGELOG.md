# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Before 1.0, the
config and the `acd/1` protocol may change without a major version bump.

## [Unreleased]

### Added

- Voice, free-form commands on macOS: anything that is not a fixed phrase goes
  to Apple's on-device model, which proposes one step. The app validates it
  against the request, agent and options on screen, and every step the model
  produced is confirmed on the HUD before it is sent. Needs Apple
  Intelligence; when it is off or not ready the HUD and Settings say so and
  the fixed phrases still work. The model asks which agent only through the
  app's own name matching: it does not return candidate agents. A 40-phrase
  typed-text check of the shipped build scored 19 of 20 on each of two sets
  (see `docs/research/2026-10-13-voice-model-quality.md`).
- Voice, spoken output on macOS: replies ("read it", summaries) are spoken from
  templates, with a Stop button and Esc, and the voice and speed are chosen in
  Settings. Optional spoken alerts for new requests (off by default). Speech
  stops when you press the hold key, lock the screen, turn voice off or quit.

### Changed

- Esc in the inbox panel now also stops speech while the app is speaking. It
  still runs what it did before, so one Esc can stop speech and undo the
  newest inbox countdown.

## [0.9.0] - 2026-10-13

### Added

- External source: a local program can put questions in the inbox over
  `hook.sock` and act on the answers itself (`PUT`, `GET` and `DELETE` under
  `/external/v1/producers/{pid}`, never on TCP). acd only queues the answer;
  the producer reports `applied`, `rejected` or `failed`. See
  [PROTOCOL.md](crates/acd-protocol/PROTOCOL.md) section 8.
- Protocol additions in `acd/1`: `source` and `answer_path` `external`,
  `sources.external` on the machine, reason `rejected` and an optional
  `message` on `request.resolved`, and `confirm` on request options. A
  client shows the message on the resolved card. The Mac app, iOS and the
  Omarchy plugin ask for a second tap before sending a `confirm` option, name
  an external producer by its agent title and take typed answers for it. Voice never sends a `confirm` option on a countdown alone. `acd status` shows `external=true`.

### Changed

- A machine that refuses the connection, times out, or cannot be reached now
  says so in plain words ("connection refused. Is acd running on that
  machine?", "cannot reach that machine. Is Tailscale on?") instead of the
  raw system error. A stopped local daemon reads "acd is not running on this
  machine".
- Adding a machine from the clipboard clears the clipboard once the machine
  is saved, on the Mac app and on iOS. The pairing line carries the client
  token.
- Settings shows how to clear a stale grant when Input Monitoring reads
  Denied. A grant given to another build of the app can stay in the way
  even though System Settings lists the app as on.

### Fixed

- `acd status | head` no longer panics when the reader stops early.
- A clean stop no longer logs `TLS listener task ended` as an error, and a
  failure that stops `acd run` is printed before the shutdown wait.

## [0.8.0] - 2026-10-13

First public pre-release. The macOS binaries are ad-hoc signed and the Mac
app is not notarized. The Linux binaries are unsigned. See
[docs/release.md](docs/release.md) for what that means for users.

### Added

- `acd`, a daemon that collects permission requests and questions from
  Claude Code and Codex hooks, herdr, and T3 Code. It serves them over the
  `acd/1` WebSocket protocol, documented in
  `crates/acd-protocol/PROTOCOL.md`.
- The `acd` command line: `init`, `run`, `status`, `pair`, `token rotate`,
  `tls rotate`, `hooks install|uninstall|status` and
  `service install|uninstall|status`. The service uses launchd on macOS and
  a systemd user unit on Linux.
- Answers to permission prompts and to single-question, single-select
  questions. T3 questions that allow custom text accept typed answers.
  Multi-question and multi-select requests are read-only.
- "Yes, always" for Claude `Bash` permissions. The app shows the exact rule
  it saves.
- Free-text follow-ups to idle or finished agents, sent through herdr or T3.
- Remote machines over Tailscale. Each machine is pinned by its TLS
  certificate, and `acd pair` prints the line that adds it.
- Agent Caller, a macOS menu bar app built with Tauri. It has a panel with a
  2-second undo before each answer is sent, keyboard navigation, a global
  hotkey, notifications for new requests, and settings for idle agents,
  subagent threads and hook-only agents.
- Voice control on macOS 26 and later. Hold Right Option or press
  Control+Option+V, then say "approve", "deny", "yes always", "option N",
  "cancel", "next" or "previous". Speech is recognized on the Mac, in English
  only.
- `acd-inbox` for Linux on x86_64 and aarch64, built for the Omarchy inbox
  (Omarchy 4 on x86_64 is the tested platform), with a user service, an installer
  and a shell plugin that shows the same inbox in the shell bar.
- An iOS client, built from source (`apps/ios/`, see its README). It is a
  foreground inbox: it shows requests and sends answers while the app is open,
  over Tailscale, with a touch layout of the same panel. It has no push
  notifications and no relay. It is signed with a free personal team, so
  installs last 7 days. It is verified in the simulator only; Keychain access,
  touch and Tailscale on a real iPhone are not yet checked.

### Known limits

- Claude's HTTP hooks cannot authenticate the daemon. Another local user who
  binds the hook port while `acd` is stopped could answer a Claude
  permission. The design targets a personal, trusted machine (spec section
  9, "Hook endpoint trust").
- A key answer sent through herdr is best effort. A terminal answer can land
  between acd's screen check and its keystroke.
- Voice has no intent model yet. Other phrasing shows "Model unavailable".
  Spoken replies, voice on Linux and voice on Windows are not built.
- The release does not include a Windows client, an Android client or a
  Windows build. The iOS client is not in the release files.
- The Mac app and the macOS `acd` binary are ad-hoc signed and not notarized
  unless the maintainer configured a Developer ID certificate for the release.
  The Linux binaries are unsigned.
- Not every acceptance check has passed. The live end-to-end record and the
  follow-up list in `docs/superpowers/plans/2026-10-07-p1-followups.md`
  record the gaps.

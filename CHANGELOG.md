# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Before 1.0, the
config and the `acd/1` protocol may change without a major version bump.

## [Unreleased]

### Added

- Android, foreground inbox over Tailscale (`apps/android`, `docs/android.md`):
  pair by pasting the `acd pair` line, list the cards, answer with the same
  whole-or-read-only rules as the other clients. Machine secrets are kept in
  files wrapped by an Android Keystore AES-GCM key (`acd-app-core::wrapped`);
  the store fails closed; a permanently invalidated key is replaced and the
  user is told which machines to pair again (a pending-recovery record keeps
  the list until the user dismisses the notice on a resumed screen, so a crash
  or a refresh that finishes in the background cannot lose it). A debug APK builds with `just android-apk`. It has
  not been run on a device or an emulator, and there is no push yet.

- Fuzz targets (`fuzz/`, cargo-fuzz) for the parsers that read untrusted bytes:
  acd/1 frames, pairing strings, hook payloads, T3 frames, herdr screens, the
  Omarchy socket protocol and the shared text rules. Not part of CI; see
  `fuzz/README.md`.

- Voice, free-form commands on macOS: anything that is not a fixed phrase goes
  to Apple's on-device model, which proposes one step. The app validates it
  against the request, agent and options on screen. Anything it asks to send
  or to answer is confirmed on the HUD first; reading, summaries and selecting
  a card run at once. Needs Apple Intelligence; when it is off or not ready the
  HUD and Settings say so and the fixed phrases still work. The model does not
  suggest candidate agents: when a phrase is ambiguous the app asks which one
  from its own name matching, or the model's best guess is confirmed. A 40-phrase
  typed-text check of the shipped build scored 19 of 20 on each of two sets
  (see `docs/research/2026-10-13-voice-model-quality.md`).
- Voice, spoken output on macOS: replies ("read it", summaries) are spoken from
  templates, with a Stop button and Esc, and the voice and speed are chosen in
  Settings. Optional spoken alerts for new requests (off by default; none while
  you talk or the screen is locked). Speech stops when you press the hold key,
  lock the screen, turn voice off or quit.

### Security

Fixes from the 2026-11-20 security audit.

- Claude hooks run the `acd hook claude` shim over `hook.sock`, as Codex
  hooks do. No hook token is written to Claude settings files. Run
  `acd hooks install claude` to replace older HTTP hook entries; `acd status`
  and `acd hooks status` warn while any remain, and name a settings file that
  others can read.
- `acd token rotate hook --revoke` ends the old hook token at once.
- acd sends the T3 token to a T3 server on this machine only after it checks
  that the server end of the connection belongs to the same user. A T3 server
  run as another user on loopback is now refused.
- `request.answer` must carry the request revision (`bad_request` without
  it). `agent.send` must carry the agent's `incarnation` when the agent has
  one, and is refused while the agent's screen shows a dialog. Neither has a
  capability flag: update the apps, `acd-inbox` and the Omarchy plugin
  together with the daemon, or older clients cannot answer requests or reply
  to herdr agents.
- Every loopback address counts as one source for the pre-auth limits.
- Clients clean control and bidi characters from peer text, and treat an allow
  option whose label does not start with an approval word as unanswerable.
  Commands also show default-ignorable characters (such as U+3164) as
  escapes.
- Mac app: the HUD does not acknowledge a step whose text does not fit, and
  Enter key repeat does not confirm. Inbox commands check which window
  calls them. "Add from clipboard" shows the host and pin, and adds the
  machine only after you confirm.
- Push registrations are bound to the client token and are dropped after a
  rotation, also across a restart. Existing registrations must register
  once more. The relay checks per-IP limits before its global limit.
  While a client token rotation is in progress, `push.register` answers
  `source_unavailable` (retry) instead of acknowledging a registration that
  a restart could lose.
- On herdr 0.9.5 and later, follow-up text goes to the agent's herdr name
  when it has one, so herdr itself refuses it once the agent is replaced.
- Debug logs record hook and herdr errors by name, length and hash, not by
  their text.
- Releases carry GitHub build provenance attestations, and
  `scripts/install.sh` verifies them, pinned to the `release.yml` workflow
  and the release tag, when `gh` is installed and signed in
  (`ACD_INSTALL_SKIP_PROVENANCE=1` skips the check).
  Dependabot also covers the cargo and npm dependencies.

From the 2026-11-04 apps and voice review and the 2026-11-05 bug hunt:

- Voice: when no card is named or selected, a free-form command that sends
  text or selects a card asks which card instead of taking the model's pick.
  Text to send must be the user's own words. A read-back that was cut says
  that more follows. A bare "yes" for a request other than the one just read
  back on that card asks for confirmation instead of counting down.
- Claude and Codex hook correlation keys can no longer collide when a session
  or prompt id contains `:`.

### Changed

- Esc in the inbox panel now also stops speech while the app is speaking. It
  still runs what it did before, so one Esc can stop speech and undo the
  newest inbox countdown.

### Fixed

- A symlinked directory that acd refuses is reported as a symlink, not as
  "not a regular file".
- acd: a request whose text exceeds 40 KiB (for example a question at every
  field cap in 4-byte characters) is published read-only, and a working
  directory is cut to 8 KiB of JSON. Over 64 KiB an agent made every client
  drop the connection and resync, in a loop, for as long as it existed.
- acd: a question whose text, header, option label or description was cut or
  altered by cleaning (hooks and T3) is read-only: it can no longer be answered
  from a card that shows it cut.
- A sub-millisecond timestamp is cut to the millisecond on the way in, so a
  message decodes to what it re-encodes to.
- Log redaction masks a `Bearer` token even when the previous line ends in the
  word `bearer`.
- acd: one device that opens connections and never sends `hello` can no longer
  keep other clients out. Each source address may hold two unauthenticated
  connections at a time (a third is dropped), and the local socket has its own
  pre-auth slots.
- A daemon that closes every connection with 4408 (client too slow) is no longer
  redialled at once, about 150 times a second. The client now waits as it does
  after any other close: 1 s, doubling to 30 s, reset when a session is ready.
- After an `acd` restart, a new request that reuses an id the app already announced
  now notifies, and is spoken when alerts are on. `acd` numbers requests from `r1`
  on every start. A request first seen in a reconnect snapshot stays silent, as before.
- Omarchy: a request that left the view when its machine disconnected no longer
  notifies again on reconnect; the last 256 announced requests are remembered.
- Omarchy: J/K now scrolls the card list so the selected card stays in view.
- Omarchy: an approval card shows every row of the command and of a question,
  not only the first six (security finding F1). A card is answered from the
  panel (digit, Enter, click or typed answer) only while the whole card is
  inside the visible list; a card taller than the panel disables its options
  and says "Too long to show here. Answer it in the terminal." A card opened
  from a notification is scrolled into view. The QML rendering and wiring are
  UNVERIFIED: Quickshell could not run in the development sandbox. The manual
  Omarchy check is in the pull request description (PR #80).

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

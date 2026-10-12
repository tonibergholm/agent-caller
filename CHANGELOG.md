# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Before 1.0, the
config and the `acd/1` protocol may change without a major version bump.

## [Unreleased]

## [0.1.0] - 2026-10-12

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

### Known limits

- Claude's HTTP hooks cannot authenticate the daemon. Another local user who
  binds the hook port while `acd` is stopped could answer a Claude
  permission. The design targets a personal, trusted machine (spec section
  9, "Hook endpoint trust").
- A key answer sent through herdr is best effort. A terminal answer can land
  between acd's screen check and its keystroke.
- Voice has no intent model yet. Other phrasing shows "Model unavailable".
  Spoken replies, voice on Linux and voice on Windows are not built.
- The release does not include a Windows client or a Windows build.
- The Mac app and the macOS `acd` binary are ad-hoc signed and not notarized
  unless the maintainer configured a Developer ID certificate for the release.
  The Linux binaries are unsigned.
- Not every acceptance check has passed. The live end-to-end record and the
  follow-up list in `docs/superpowers/plans/2026-10-07-p1-followups.md`
  record the gaps.

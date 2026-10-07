# Research: open-source alternative to One (getone.one)

Date: 2026-10-07. Sources are web pages fetched that day, mostly through a
summarizing fetcher. Re-verify anything load-bearing against primary sources.

## Goal

Build primarily for my own multi-agent workflow (A). Ship a reusable core,
a daemon plus a documented protocol, with a reference Mac app on top (C).

## What One is

One is a closed-source Mac app with paid tiers. It does not write code; it
manages the coding agents you already run. Its features:

- An inbox and toolbar showing each agent's state: blue is working, amber
  needs you, green is done. You answer pending questions from the keyboard,
  and every answer gets a 2-second undo.
- A voice/text router. Double-tap Option, speak or type, and One sends the
  request to the right agent or starts a new one.
- An iPhone app (TestFlight) and a connector for Linux servers.

How it works, according to its docs:

- **Runtime.** Agents run inside herdr, an open-source Rust terminal
  multiplexer that persists sessions.
- **State detection.** One reads herdr's agent list, plus each agent's
  conversation from the Claude Code and Codex session files, falling back
  to the terminal screen. It polls every 2.5 s while a view is open and
  every 10 s otherwise.
- **Remote.** The server connector keeps an outbound connection to One's
  relay. Content is encrypted with AES-256-GCM using a key per machine. The
  relay sees metadata: agent names, states, timing. The pairing and
  key-exchange protocol is not documented.
- **Cloud AI.** Speech-to-text and the router LLM are cloud services
  (OpenAI, Groq, Anthropic, OpenRouter). Screenshots are sent "per turn".
  How the router picks an agent is not documented; the likely design is an
  LLM working over the request, a screenshot and agent context
  (inference).
- **Permissions.** Microphone, Accessibility, Screen Recording, Automation.

## herdr

- Repository: github.com/herdrdev/herdr. Rust, about 42.8k stars, last push
  2026-10-07. **Apache-2.0**, confirmed from the LICENSE file and the GitHub
  API on 2026-10-07. The AGPL claim in a third-party review is outdated or
  wrong.
- A server owns the PTYs and clients attach over a Unix socket. The socket
  API is NDJSON and covers panes, reading output, sending input, agent
  state and event subscriptions.
- State detection matches screen-bottom rules per agent (TOML manifests).
  When an integration reports state, that report takes precedence over the
  screen.

## Open-source landscape

| Project | License | Relevance |
|---|---|---|
| Paseo | Apache-2.0 | Closest overall: daemon, desktop, mobile, voice, and a relay it calls E2E (unverified in code). Candidate base. |
| Happy | MIT | Best-documented E2E relay: NaCl/AES-GCM, Ed25519 auth, self-hostable. Reuse the design. |
| T3 Code | MIT | Desktop and mobile, relay, Tailscale, SSH. No E2E claim, no voice. |
| cmux | GPL-3.0 + BSL | Mac-native. Status from OSC escape sequences and hooks. Reference only (license). |
| CCManager | MIT | Per-agent idle/busy/waiting detection. Worth porting. |
| agent-deck | MIT | tmux-based. Telegram/Slack control. |
| Codey | MIT | Menubar push-to-talk with WhisperKit. Tiny. |
| Claude Squad, opcode | AGPL | TUI / Claude-only GUI. |
| Superset | Elastic-2.0 | Source-available, not OSS. |
| vibe-kanban, Terragon, Omnara | — | Sunsetting, shut down, or pivoted. Avoid. |

**Gap.** No OSS project has a router that sends a spoken or typed request
to the right session, or the undo window. None combines a Mac-native inbox
with voice, mobile and E2E remote. These are our differentiators.

## Integration surfaces

The surfaces fall into three groups:

1. **Structured protocol, for sessions we own.** We get typed prompts,
   option IDs and reliable answers. The sessions must be launched or
   attached by us.
   - Claude Agent SDK: `canUseTool` covers permissions and
     `AskUserQuestion`.
   - Codex app-server: JSON-RPC, with `requestApproval` and
     `tool/requestUserInput`.
   - OpenCode `serve`: HTTP + SSE, with a permissions endpoint.
   - ACP: `session/request_permission`. 40+ agents, 1:1 stdio, client
     spawns the agent.
2. **Hooks, for sessions the user started.** Hooks see state and can
   answer prompts while they block, but cannot inject free text.
   - Claude Code: Notification (`permission_prompt`, `idle_prompt`, ...),
     PermissionRequest and Stop hooks. An `http` hook can POST to our
     daemon. A PreToolUse hook can answer `AskUserQuestion`. The default
     hook timeout is 600 s, which bounds how long a reply from the phone
     can take.
   - Codex hooks. Codex `notify` fires only on turn-complete.
3. **PTY/screen**, via herdr or tmux. Works with any agent and can inject
   free text. Brittle when the TUI changes.

Likely strategy: hooks and structured protocols as the primary source of
truth, with a PTY layer (herdr or tmux) for free-text injection and as a
fallback for agents without hooks.

## Voice, routing, remote, shell

- **Speech-to-text.** Apple SpeechAnalyzer on macOS 26 is local, needs no
  model download, and has latency comparable to whisper.cpp (blog-level
  benchmarks). FluidAudio Parakeet is optional; whisper.cpp is the
  cross-platform fallback.
- **Hotkey.** Detect a double-tap modifier by watching `flagsChanged`
  events. Needs the Input Monitoring permission.
- **Routing.** Cheap signals first (repo or cwd name, a pending question),
  then a small LLM classifier over per-session summaries, with confirmation
  when confidence is low. Apple Foundation Models runs it on-device on
  macOS 26; Haiku or a local Ollama model are the alternatives.
- **Remote.** Self-hostable relay that stores only ciphertext (libsodium,
  X25519 + XChaCha20-Poly1305), QR pairing, and wake-up pings with no
  content via ntfy or APNs. Do not depend on Claude Code Remote Control:
  it is Anthropic-hosted and has no third-party API.
- **Shell.** A native SwiftUI `MenuBarExtra` app gives the best macOS fit.
  If other platforms matter, use Tauri for the UI plus a Swift helper for
  the hotkey and speech.

## herdr socket API (detail)

All sources are first-party (herdr.dev docs, `docs/next/api/herdr-api.schema.json`,
CHANGELOG), read on 2026-10-07 through a summarizing fetcher. herdr's Rust
source was not read.

- **Transport.** NDJSON over a Unix socket at `~/.config/herdr/herdr.sock`
  (or the `HERDR_SOCKET_PATH` override). No TCP listener, no auth.
  Remote machines are reached over SSH.
- **Agents.** `agent.list` and `agent.get` return `pane_id`, `agent`,
  `agent_status`, `cwd`, `title`, `state_change_seq`, and more.
- **States.** `idle`, `working`, `blocked`, `done` (finished, not yet
  seen), `unknown`.
- **Events.** `events.subscribe` streams `pane_agent_status_changed`,
  `pane_agent_detected` and `pane_closed`. If a subscriber falls behind it
  gets `events_lost`, and should re-sync from `agent.list`.
- **Input.** `agent.prompt` sends free text. `pane.send_keys` sends keys.
  There is no "select option" method.
- **Gap: the question and its options are not exposed.** `blocked` comes
  only from matching the screen against detection manifests. Getting the
  question and options means reading the pane with `pane.read` and parsing
  it ourselves, or getting the structured payload from our own Claude Code
  and Codex hooks (PermissionRequest, Notification, AskUserQuestion).
- **Hooks.** `herdr integration install claude|codex` installs hooks, but
  they only report session identity, not state.
- **Plugins.** v1 plugins are one-shot commands, not long-running. The
  inbox has to be a separate daemon.
- **Stability.** herdr is pre-1.0 and releases every 1–3 weeks. It
  introduces a breaking API change roughly every 1–2 months. Pin a minimum
  version, ignore unknown fields, and call `ping` first.

## T3 Code integration

Sources: t3code at `main` on 2026-10-07, read through a summarizing fetcher.

- **Architecture.** The server owns providers, git and terminals. Clients
  are thin and use authenticated WebSocket RPC, with Effect schemas in
  `packages/contracts/src/` (`rpc.ts`, `orchestrationV2.ts`, `auth.ts`).
  There is no stability promise for third-party clients; the README calls
  the project "very early".
- **Headless auth.** Run `t3 auth session issue --scope orchestration:read
  --scope orchestration:operate` to get a bearer token. `t3 serve
  --tailscale-serve` covers Tailscale.
- **What we need is all in the contracts:**
  - Thread status: `idle|preparing|queued|starting|running|waiting|completed|interrupted|failed|cancelled|rolled_back`.
  - Push streams: `subscribeShell` (all threads) and `subscribeThread`.
  - Commands, sent through `dispatchCommand`: `message.dispatch` (send a
    message) and `runtime-request.respond` (takes `decision` for approvals,
    `answers` for questions).
  - Pending request kinds: approvals (`command_execution`, `file_change`,
    ...) and user-input questions with options.
- **Not suitable: T3's MCP server.** Its `t3_pending_request_*` tools skip
  approvals and cannot answer them, and it has no push.
- **Prior art.** Two small third-party clients, t3layer and t3code-mcp,
  use HTTP `/api/orchestration/*` with a bearer token and poll.
- **Unverified.** The WebSocket handshake (how the token is presented), the
  `ProviderApprovalDecision` literals, and the HTTP routes in T3's own
  source. Read `packages/client-runtime/src/{rpc,connection}` before
  building.
- **Risk.** Claude Code inside T3 runs through the Agent SDK. If T3 sessions
  also load the user's Claude hooks, the same request could show up twice:
  once from the hook and once from T3. The inbox has to deduplicate, or
  skip hooks for T3-owned sessions.

## Spike: answer path (2026-10-07, throwaway)

Setup: herdr 0.9.3 (isolated `spike` session), Claude Code 2.1.292, Codex
CLI 0.160.1. Hooks lived in project-local settings and posted to a Python
listener. All artifacts were removed afterwards except Claude's entry in
`~/.claude.json` trusting `/private/tmp/acspike/proj`.

**Joining hook events to herdr panes**

- Hook processes inherit `HERDR_PANE_ID` and `HERDR_SOCKET_PATH`, for both
  Claude and Codex.
- herdr's `agent_session.value` equals Claude's `session_id`.

**Claude Code**

- `PermissionRequest` fires immediately. Its payload has `tool_name`,
  `tool_input`, `permission_suggestions` (proposed always-allow rules) and
  `prompt_id`.
- `AskUserQuestion` fires both `PermissionRequest` and `PreToolUse`. The
  `tool_input.questions[]` carry `question`, `header`, `options[{label,
  description}]` and `multiSelect`.
- `Notification` (`permission_prompt`) arrives about 6 s later. It is
  redundant.
- **While a blocking `PermissionRequest` hook is pending, the terminal dialog
  is already shown** and herdr reports `blocked`.
  - An empty hook reply leaves the dialog up (fails open).
  - Replying `decision.behavior: "allow"` dismisses the dialog and logs
    "Allowed by PermissionRequest hook".
- **Race:** answering at the terminal first resolves the prompt and Claude
  closes the hook's HTTP connection (the listener saw a broken pipe). A late
  "allow" is ignored. The daemon can detect "answered elsewhere" when the
  connection closes.
- Numbered key presses through `herdr agent send-keys` answer both
  permission dialogs and `AskUserQuestion`.
- A `PreToolUse` reply with `updatedInput.answers` answers `AskUserQuestion`
  without showing the dialog. It accepts free text that is not one of the
  options. The cost is that the terminal user never sees the question while
  the hook blocks.
- Hooks fire in T3-hosted Claude sessions as well: T3 omits
  `settingSources`, yet user plugin hooks ran in this T3 session. Those
  processes carry `CLAUDE_CODE_ENTRYPOINT=sdk-ts`, while a terminal session
  started from the CLI has a different entrypoint. The hook shim should
  forward the entrypoint so the daemon can drop T3 sessions, which the T3
  adapter covers.

**Codex**

- Only `command` hooks are supported, so we use a curl shim. Project hooks
  require a one-time trust review in the TUI.
- **While a `PermissionRequest` hook is pending, Codex shows "Running hooks"
  and herdr reports `working`. The approval dialog appears only after the
  hook returns.**
  - A blocking hook therefore stalls the terminal.
  - Hook "allow" works.
  - Esc in the dialog denies.
- The payload has `tool_name`, `tool_input.command`,
  `tool_input.description` and `turn_id`. There is no Notification event.
- herdr reported Codex's folder-trust and hook-review screens as `idle`, not
  `blocked`: a detection gap.

**Implications for the design**

- Claude: hold `PermissionRequest` open while the inbox decides. The
  terminal stays usable, the first answer wins, and a connection close
  signals that the prompt was answered elsewhere.
- Codex: return from the hook immediately (it is notify-only) and answer
  with herdr key presses. Alternatively, hold briefly only when an inbox
  client is actively connected.
- A daemon that is down, or a hook timeout, must fail open: an empty reply
  and the normal dialog.

## Handy (voice building block)

- Repository: github.com/cjpais/handy. **MIT**, Rust + Tauri 2, about 33k
  stars, last push 2026-10-07 (GitHub API, checked that day). Offline
  speech-to-text on macOS, Windows and Linux. Press a shortcut, speak, and
  the text is pasted into the focused app.
- Models: Whisper (Small/Medium/Turbo/Large, GPU when available) and
  Parakeet V3 (CPU-optimized). Silence is filtered with Silero VAD.
- Reusable crates, from `src-tauri/Cargo.toml`:
  - `transcribe-rs` (ONNX) and `transcribe-cpp`: model inference.
  - `vad-rs`: voice activity detection.
  - `handy-keys` and `rdev`: global keys.
  - `cpal`: audio capture.
  - `tauri-nspanel`: a macOS floating panel.
- **Implication.** herdr and Handy are both Rust, and both are permissively
  licensed. A Rust core daemon plus a Tauri app could reuse Handy's voice
  stack directly and talk to herdr natively, and would leave a path to
  Linux and Windows. This is a strong alternative to the SwiftUI shell.
  Apple SpeechAnalyzer and Foundation Models would then become optional
  macOS backends rather than the default.

## Open questions

- herdr's relationship to One (its license is settled: Apache-2.0).
- Does Paseo's relay actually do E2E (read the code)? Is Paseo a better
  base than building on herdr or our own daemon?
- Robustness of long-blocking hooks against the 600 s timeout (needs a
  prototype).
- Unverified surfaces: Claude statusline/stream-json, Codex `exec --json`,
  Codex app-server thread-status notifications.

# Omarchy inbox: design

Status: approved in conversation 2026-10-12; written spec awaiting review.

## 1. Purpose

An agent-caller inbox for Omarchy (Arch Linux + Hyprland, Omarchy 4) that
is, and feels like, an Omarchy app. The inbox exists to act when agents
need the user (see the main spec and the "actionable only" rule): answer
permission requests and questions, send replies, and undo within the 2 s
window, for agents on every paired machine.

The user's decisions (2026-10-12):
- Two views over time: an Omarchy shell plugin first, a TUI later.
- A notification opens the panel on that card; it does not answer.
- One inbox service with thin views (approach 1 below).

## 2. Scope

### 2.1 This slice

- `acd-inbox`: a Rust systemd user service that runs the existing
  `acd-client` inbox and serves it over a local socket.
- The Omarchy plugin `tonibergholm.agentcaller`: a bar widget with a
  panel.
- Notifications through the shell's freedesktop notification server.
- Remote tokens in the Secret Service (gnome-keyring on Omarchy).
- Setup: `acd-inbox install`.

### 2.2 Later, separate plans

- The TUI (same socket), including the agent chat log view.
- Voice on Linux (voice spec backend B; research in
  `docs/research/2026-10-12-voice-backend-b.md`).
- Windows; a published plugin repo for `omarchy plugin add`; packaging
  (AUR).

The Mac app does not change.

## 3. Architecture

```
 acd daemons (local + remotes over Tailscale)
        │ acd/1 (existing)
        ▼
 acd-inbox  (systemd user service, Rust)
   • acd-client Inbox: views, guards, 2 s countdowns, undo, filtering
   • SecretServiceStore: remote tokens (D-Bus org.freedesktop.secrets)
   • notifier: org.freedesktop.Notifications
   • socket server: $XDG_RUNTIME_DIR/acd-inbox.sock
        │ JSON lines (§5)
        ▼
 Omarchy plugin (QML bar widget + panel)      later: TUI, same socket
```

Rejected:
- The plugin starting the helper itself over stdin/stdout: the helper dies
  with the shell, and a TUI would run a second inbox (two undo windows,
  duplicate notifications).
- Speaking acd/1 from QML: it would reimplement the inbox and its safety
  rules in JavaScript.

The service owns all state and every rule about what is safe to send.
Views send intents; the service checks them with the guards the Mac app
uses (`acd-client`).

## 4. `acd-inbox`

New crate `crates/acd-inbox` (binary `acd-inbox`). Linux-only parts
(Secret Service, D-Bus notifications, systemd install) are behind
`cfg(target_os = "linux")`; the protocol, command handling and socket
server build and test on every platform.

Modules:
- `protocol`: wire types (§5) and their JSON schema tests.
- `server`: the socket listener, per-connection reader and writer tasks,
  peer check, snapshot broadcast.
- `commands`: maps a command to an `Inbox` call and its result to a reply.
- `notify`: decides which requests notify (§7) and sends them; the D-Bus
  sender sits behind a trait so tests use a fake.
- `secrets`: `SecretServiceStore`, an `acd_client::SecretStore`, wrapped
  in `CachedSecrets` as the Mac app does.
- `install`: `acd-inbox install | uninstall | status` (§9).

Files:
- Settings: `$XDG_CONFIG_HOME/acd-inbox/settings.json` (the `acd-client`
  `Settings` format; no secrets).
- Socket: `$XDG_RUNTIME_DIR/acd-inbox.sock`, mode 0600, in a 0700 runtime
  dir. A stale socket is replaced only when it is a socket and no service
  answers on it (as `acd` does for its own sockets).
- Single instance: a lock file next to the socket.

Logging follows CLAUDE.md: agent and message text never reach logs; tags,
lengths and hashes only. Secret Service and D-Bus errors are logged by
class, not by message text.

## 5. Socket protocol

JSON, one object per line, UTF-8. Maximum line 4 MiB; a longer line
closes the connection.

Connection:
- Peer check: `SO_PEERCRED` uid must equal the service's uid, else the
  connection closes before any data.
- Service → view, first: `{"type":"hello","protocol":1,"service":"<version>"}`,
  then `{"type":"view","view":<InboxView>}`.
- A `view` message follows every change. Views are full snapshots; a
  view that falls behind gets only the newest (no queue of stale views).

View → service commands carry an `id` (string, ≤ 64 bytes):

| Command | Fields | Maps to |
|---|---|---|
| `pick` | `card`, `request`, `option`, `revision` | `Inbox::pick` |
| `answer_text` | `card`, `request`, `revision`, `text` | `Inbox::answer_text` |
| `reply` | `card`, `text` | `Inbox::reply` |
| `cancel` | `action` | `Inbox::cancel` |
| `escape` | — | `Inbox::escape` (newest countdown) |
| `mark_seen` | `card` | `Inbox::mark_seen` |
| `set_show` | `what` (`idle`, `subagents`, `hook_only`), `on` | the matching setter |
| `pair` | `pairing` | add a machine from a pairing string |
| `rename_machine` / `remove_machine` / `retry_machine` | `machine`, (`name`) | as in the Mac app |
| `set_notifications` | `on` | service setting |

Replies: `{"type":"ok","id":…,"result":…}` or
`{"type":"error","id":…,"code":"<code>"}`. Codes are fixed strings the
panel maps to text (for example `stale`, `unknown_card`, `not_answerable`,
`bad_text`, `bad_pairing`, `secret_store_locked`, `unsupported`); the
service never sends an error's message text. Action outcomes are pushed
as `{"type":"outcome",…}` in the form of the Mac app's outcome stream.

A `pair` result returns only the machine key and name. Tokens never
cross the socket. An unknown `type` gets `error` `unsupported`, so a newer
view can talk to an older service.

## 6. The plugin

`clients/omarchy/` in this repo: `manifest.json` (`kinds: ["bar-widget"]`,
`defaultSection: "right"`, category "AI"), `BarWidget.qml`, `Panel.qml`,
`Service.qml` (the socket client), and `model.js` (pure view logic:
sections, filtering, key handling, text for error codes).

It follows Omarchy's own panels (for example `omarchy.tailscale`):
- `Panel` from `qs.Ui`, colours from `Commons.Color` (qualified import),
  spacing, type and interactive states from `Style`. No colours or sizes
  of its own, so every theme applies.
- `ShellIpc` target `tonibergholm.agentcaller` with `open`, `close`,
  `toggle`, and `openCard <key>`.
- `Quickshell.Io` `Socket` to the service; reconnects with backoff.

Bar widget:
- A glyph and the number of agents that need the user.
- Dim when none; `urgent` colour when some; struck-through when the
  service is unreachable.
- Click toggles the panel.

Panel:
- Header `Inbox · N need you · M working`, then machine filter chips with
  per-machine status.
- Sections: Needs you, Working, Done (collapsed). Subagents, idle and
  hook-only agents hidden unless their toggle is on, as on the Mac.
- A card: agent kind, machine, project, title, herdr/T3 badge, and the
  request (command, or question with numbered options).
- Keys: J/K and arrows move, 1–9 pick, T typed answer, R reply, E mark
  seen, Esc undoes the newest countdown, else closes.
- A countdown shows as an accent bar on its card with "Esc to undo".
- The panel sends the card key, request id and revision it showed; the
  service rejects stale ones.
- Settings view (gear): paired machines (paste a pairing string; rename;
  remove; retry), the three show toggles, notifications on or off.

States the panel tells apart: service unreachable; a machine offline (per
chip); Secret Service locked (asks the user to unlock the keyring).

## 7. Notifications

- One notification per new pending request on a card that is visible
  under the current filters, sent by the service over
  `org.freedesktop.Notifications`. The Omarchy shell renders it.
- Text: "<Agent> on <machine> needs you: <request label>", built from the
  same templates as the Mac app; agent text cleaned and truncated.
- One action, "Open": the service runs
  `omarchy-shell tonibergholm.agentcaller openCard <key>` (argv, no shell).
- No notification for the user's own countdowns, or for a request already
  notified (keyed by card, request id and revision).
- The service closes a notification when its request resolves.
- Off switch in settings. Do-not-disturb is the shell's; the bar count
  still updates.

## 8. Secrets and pairing

- `SecretServiceStore` stores each remote token as a Secret Service item
  with attributes `service=agent-caller`, `machine=<key>`, in the default
  collection.
- A locked collection: the service asks the Secret Service to unlock
  (the user sees the keyring prompt) once per command that needs a
  secret; if it stays locked, the command returns `secret_store_locked`
  and connections to remotes wait.
- The local machine uses the local `acd` socket and token file, as the
  Mac app does; no stored secret.

## 9. Setup

`acd-inbox install`:
1. Writes `~/.config/systemd/user/acd-inbox.service` and enables it.
2. Copies the plugin to
   `~/.config/omarchy/plugins/tonibergholm.agentcaller/`, runs
   `omarchy-shell shell rescanPlugins`, then enables and places it with
   `omarchy plugin enable` (right section).
3. Prints the keybinding line (Super+Alt+A → `omarchy-shell
   tonibergholm.agentcaller toggle`) and offers to append it to the
   user's Hyprland bindings: asks first, backs up the file, and changes
   nothing without a yes.

`uninstall` reverses 1 and 2 and prints how to remove the binding.
`status` reports the service, socket, plugin and Secret Service state.

The Omarchy machine has no Rust toolchain: the binary is built in Docker
(`rust:1-bookworm`, `--platform linux/amd64`) on the Mac, as `acd` is.

## 10. Error handling

| Case | Behaviour |
|---|---|
| Service down | Bar struck-through; panel says the inbox service is not running and how to start it. |
| Service restarts | Plugin reconnects with backoff and gets a fresh snapshot. |
| Shell restarts | Service keeps running; plugin reconnects on load. |
| Stale pick | `error stale`; the panel keeps the new view. |
| Remote offline | Chip status, as on the Mac. |
| Keyring locked | `secret_store_locked`; panel asks to unlock. |
| Notification server absent | Notifications off for the session; logged by class. |
| Malformed or oversized line | Connection closed; logged by tag and length. |

## 11. Testing

Rust (CI on Ubuntu and macOS; Linux-only modules on Ubuntu):
- Protocol round trips and the JSON shape views depend on.
- Each command maps to the right `Inbox` call; stale revision and unknown
  card give their codes; tokens never appear in any message.
- Snapshot on connect and after every change; a slow view gets only the
  newest view.
- Peer check rejects another uid (unit test of the check); socket mode
  0600; stale-socket handling.
- Notifier: one per request, none for repeats or own countdowns, close on
  resolve; with a fake D-Bus sender.
- Secret store: against a fake; the Secret Service itself in a live
  check.
- Logs never contain agent text (log-capture test, as in the voice
  crates).

QML:
- `model.js` tested with node (sections, filters, keys, error texts), in
  the style of `tonibergholm.ruuvi`'s `model.test.cjs`.
- `omarchy-plugin-validate` and `qmllint` in a live check (they need the
  Omarchy machine).

Live checks on omarchy (recorded in `docs/research/`):
1. Answer a real Claude permission request from the panel; Esc undoes.
2. A notification opens the panel on the card.
3. Restart the shell; the panel reconnects. Stop the service; the bar
   shows it.
4. Pair a remote (minos) from the panel; restart; the token comes from
   the Secret Service.
5. A remote's request (minos) appears and is answerable.

## 12. Open questions

- The `Socket` type in the Omarchy Quickshell build: confirm it reads
  line-delimited data with a 4 MiB line, or frame the view in chunks.
- Omarchy's notification server: confirm action support and
  `CloseNotification`.
- Whether `omarchy plugin enable` accepts a section in this Omarchy
  version, or `omarchy bar put` is needed.

# Omarchy inbox: design

Status: approved by the user 2026-10-12. Amended the same day after
Sol's review (§13 lists the decisions).

## 1. Purpose

An agent-caller inbox for Omarchy (Arch Linux + Hyprland, Omarchy 4) that
is, and feels like, an Omarchy app. The inbox exists to act when agents
need the user (main spec; the "actionable only" rule): answer permission
requests and questions, send replies, and undo within the 2 s window, for
agents on every paired machine.

The user's decisions (2026-10-12):
- Two views over time: an Omarchy shell plugin first, a TUI later.
- A notification opens the panel on that card; it does not answer.
- One inbox service with thin views (§3).

## 2. Scope

### 2.1 This slice

- `acd-inbox`: a Rust systemd user service that runs the existing
  `acd-client` inbox and serves it over a local socket.
- The Omarchy plugin `tonibergholm.agentcaller`: a bar widget with a
  panel.
- Notifications through the shell's freedesktop notification server.
- Remote secrets in the Secret Service (gnome-keyring on Omarchy).
- Setup: `acd-inbox install`.
- The `acd-client` changes these need (§4.3).

### 2.2 Later, separate plans

- The TUI (same socket), including the agent chat log view.
- Voice on Linux (voice spec backend B; research in
  `docs/research/2026-10-12-voice-backend-b.md`).
- Windows; a published plugin repo for `omarchy plugin add`; packaging
  (AUR).

The Mac app keeps its behaviour. Shared `acd-client` changes must not
change it; its tests stay green.

## 3. Architecture

```
 acd daemons (local + remotes over Tailscale)
        │ acd/1 (existing)
        ▼
 acd-inbox  (systemd user service, Rust)
   • acd-client Inbox: views, guards, 2 s countdowns, undo, filtering
   • secret worker: Secret Service (D-Bus org.freedesktop.secrets)
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

### 4.1 Crate

New crate `crates/acd-inbox` (binary `acd-inbox`). Linux-only parts
(Secret Service, D-Bus notifications, systemd install) are behind
`cfg(target_os = "linux")`; the protocol, command handling and socket
server build and test on every platform (the peer check uses
`SO_PEERCRED` on Linux and `getpeereid` on macOS).

Modules:
- `protocol`: wire types (§5) and their JSON shape tests.
- `server`: socket startup (§4.4), per-connection reader and writer tasks,
  peer check, snapshot broadcast, connection registry.
- `commands`: maps a command to an `Inbox` call and its result to a reply.
- `owners`: which connection owns which action (§6).
- `notify`: reconciles views into notifications (§8); the D-Bus side sits
  behind a trait so tests use a fake.
- `secrets`: the secret worker and `SecretServiceStore` (§9).
- `install`: `acd-inbox install | uninstall | status` (§10).

Files:
- `$XDG_CONFIG_HOME/acd-inbox/settings.json`: the `acd-client` `Settings`
  (no secrets).
- `$XDG_CONFIG_HOME/acd-inbox/service.json`: service preferences
  (`notifications: bool`, default on).
- `$XDG_RUNTIME_DIR/acd-inbox.sock` and `acd-inbox.lock`.

Logging follows CLAUDE.md: agent and message text never reach logs; tags,
lengths and hashes only. Raw socket lines are never logged. Secret
Service and D-Bus errors are logged by class, not by message text.

### 4.2 Unit and lifetime

`~/.config/systemd/user/acd-inbox.service`:
- `ExecStart=` the absolute path of the installed binary
  (`~/.local/bin/acd-inbox serve`).
- `PartOf=graphical-session.target`, `After=graphical-session.target`,
  `WantedBy=graphical-session.target`: it runs with the Hyprland session,
  so the session D-Bus, the Secret Service and the notification server
  are there.
- `Restart=on-failure`, `RestartSec=2`.

`omarchy-shell` is found on `PATH` at startup, else `/usr/bin`; if
missing, the "Open" action is left out of notifications.

### 4.3 `acd-client` changes

- Action owners: `Inbox` actions carry an opaque owner tag. New
  `cancel_owned(owner)` cancels every unsent action of that owner and
  `escape_owned(owner)` cancels that owner's newest. The Mac app passes
  one fixed owner, so its behaviour is unchanged.
- Machine handle recreation: `reconnect_machine(key)` rebuilds a handle
  that was never created (for example a secret that could not be read at
  start), not only one that exists.
- Secret status per machine: `MachineEntryView` gains `secret`:
  `ok | locked | missing | unreadable`, so a locked keyring is not shown
  as "not paired". Connection status stays in `InboxView.machines`;
  views join the two by machine key.
- Secrets outside locks: `Inbox` never calls the `SecretStore` while
  holding its own lock. Pairing stores the secret first, then commits the
  machine under the lock. If the commit fails, a new machine's stored
  secret is removed and a re-paired machine's previous secret is
  restored (as `acd-client` does today).

### 4.4 Socket startup

- `$XDG_RUNTIME_DIR` must exist, be owned by the user, mode 0700, and not
  be a symlink; otherwise the service exits with an error.
- Take the lock file (`O_NOFOLLOW`, 0600, exclusive `flock`) before
  touching the socket path. A second instance exits.
- With the lock held: an existing path that is a socket is removed; any
  other file type, or a symlink, makes the service exit (never delete it).
- Bind, then set mode 0600.

## 5. Socket protocol

JSON, one object per line, UTF-8. A line over 4 MiB from a view closes
that connection. The service keeps its own lines under 4 MiB: agent text
fields in a view are truncated by the service (with a marker) until the
view fits, and the panel shows the marker.

### 5.1 Connection

- Peer check: the peer's uid must equal the service's uid, else the
  connection closes before any data.
- Service → view, in order: `hello` (`{"type":"hello","protocol":1,
  "service":"<version>"}`), `settings` (§5.4), `view`.
- A `view` (full `InboxView`) follows every change; a `settings` follows
  every settings change. Views and settings are coalesced: a slow view
  gets only the newest of each.
- `outcome` messages (§5.3) are not coalesced: each connection has a
  bounded queue of 256. On overflow the service sends
  `{"type":"outcomes_lost"}` once, drops the queue, and the view shows
  the results of its fired actions as uncertain ("Check the terminal").
  The service's own subscription to `acd-client`'s outcome stream can
  lag too: on `Lagged` it sends `outcomes_lost` to every connection.
- The service writes with backpressure: a view that does not read for
  30 s is disconnected.

### 5.2 Commands

View → service commands carry an `id` (string, ≤ 64 bytes, unique per
connection). A repeated id on the same connection gets `error
duplicate_id` and does nothing. A view never re-sends a command after a
reconnect; the user acts again.

| Command | Fields | Maps to |
|---|---|---|
| `pick` | `card`, `request`, `option`, `revision` | `Inbox::pick` (owned) |
| `answer_text` | `card`, `request`, `revision`, `text` | `Inbox::answer_text` (owned) |
| `reply` | `card`, `text` | `Inbox::reply` (owned) |
| `cancel` | `action` | `Inbox::cancel` (any action shown on a card) |
| `escape` | — | `escape_owned` (this connection's newest) |
| `mark_seen` | `card` | `Inbox::mark_seen` |
| `set_show` | `what` (`idle`, `subagents`, `hook_only`), `on` | the matching setter |
| `pair` | `pairing` | pair a machine (§9) |
| `rename_machine` / `remove_machine` | `machine`, (`name`) | as in the Mac app |
| `retry_machine` | `machine` | `reconnect_machine` |
| `unlock_secrets` | — | secret worker unlock (§9) |
| `set_notifications` | `on` | `service.json` |
| `panel` | `open` (bool) | notification suppression (§8) |

Replies: `{"type":"ok","id":…,"result":…}` or `{"type":"error","id":…,
"code":"<code>"}`. `pick`, `answer_text` and `reply` return the action id
in `result`. Codes are fixed strings the panel maps to text (`stale`,
`unknown_card`, `not_answerable`, `bad_text`, `bad_pairing`,
`secret_store_locked`, `secret_store_timeout`, `duplicate_id`,
`unsupported`, …); the service never sends an error's message text. An
unknown `type` gets `error unsupported`, so a newer view can talk to an
older service.

### 5.3 Outcomes

`{"type":"outcome","action":"<id>","card":"<key>","result":R}` where `R`
is one of `{"kind":"fired"}`, `{"kind":"delivered"}`, `{"kind":"rejected",
"code":"<acd error code>"}`, `{"kind":"uncertain"}`, `{"kind":"cancelled"}`,
`{"kind":"not_sent","reason":"<reason>"}`, mapped one to one from
`acd-client`'s action outcomes. `fired` is not terminal; every action
gets exactly one terminal result unless `outcomes_lost` intervenes.

Outcomes go to every connection (the bar and a TUI may show the same
card). A connection is subscribed when it is accepted, before `hello`,
so it sees results of its own commands and of other views' actions.

### 5.4 Settings snapshot

`{"type":"settings","machines":[MachineEntryView…],"notifications":bool,
"show_idle":bool,"show_subagents":bool,"show_hook_only":bool}`. Machine
entries include name, endpoint and `secret` status (§4.3); connection
status comes from `InboxView.machines`, joined by machine key. No tokens
or TLS pins.

## 6. Undo with several views

The main spec's rule holds: a client that disconnects during the undo
window sends nothing (`2026-10-07-agent-inbox-design.md` §12).

- Every action started over the socket is owned by its connection.
- When a connection closes (panel reload, shell restart, TUI quit), the
  service calls `cancel_owned` for it before anything else; its unsent
  countdowns end as `cancelled`.
- Esc in a view cancels that view's newest countdown only.
- A card's countdown bar has an explicit cancel control that cancels that
  action whichever view started it.

## 7. The plugin

`clients/omarchy/` in this repo: `manifest.json` (`kinds:
["bar-widget"]`, `defaultSection: "right"`, category "AI"),
`BarWidget.qml`, `Panel.qml`, `Service.qml` (the socket client), and
`model.js` (pure view logic: sections, filtering, keys, error texts).

It follows Omarchy's own panels (for example `omarchy.tailscale`):
- `Panel` from `qs.Ui`, colours from `Commons.Color` (qualified import),
  spacing, type and interactive states from `Style`. No colours or sizes
  of its own, so every theme applies.
- `ShellIpc` target `tonibergholm.agentcaller` with `open`, `close`,
  `toggle`, and `openCard <key>`.
- `Quickshell.Io` `Socket` with a `SplitParser` on newlines; every write
  is followed by `flush()`. Reconnects with backoff.
- The panel reports `panel open/closed` to the service.

Bar widget:
- A glyph and the number of agents that need the user.
- Dim when none; `urgent` colour when some; struck-through when the
  service is unreachable.
- Click toggles the panel.

Panel:
- Header `Inbox · N need you · M working`, then machine filter chips with
  per-machine status. The machine filter is the panel's own.
- Sections: Needs you, Working, Done (collapsed). What shows follows
  `InboxView` exactly: the service already hides subagents, idle agents
  and hook-only agents without a request unless their toggle is on;
  hook-only agents with a pending request stay visible.
- A card: agent kind, machine, project, title, herdr/T3 badge, and the
  request (command, or question with numbered options).
- Keys: J/K and arrows move, 1–9 pick, T typed answer, R reply, E mark
  seen, Esc undoes this panel's newest countdown, else closes. In a text
  field, letters and digits are text, and IME composition is never a
  shortcut.
- Actions follow the card's flags (`answerable`, `free_text_answerable`,
  `selectable_options`, `can_reply`, `can_mark_seen`) as the Mac panel
  does: read-only and unknown-effect options cannot be picked; an
  "always allow" option shows its rule text before it can be picked.
- A countdown shows as an accent bar on its card with "Esc to undo" and a
  cancel control.
- The panel sends the card key, request id and revision it showed; the
  service rejects stale ones.
- Settings view (gear): paired machines with status (paste a pairing
  string; rename; remove; retry; "Unlock keyring" when locked), the three
  show toggles, notifications on or off.

The pairing field is a secret: it is cleared after every attempt, never
logged, never written to the QML console, and an error never repeats it.

States the panel tells apart: service unreachable; a machine offline (per
chip); keyring locked (with "Unlock keyring").

## 8. Notifications

The service reconciles each new `InboxView` against the previous one:
- A notification for each `(card, request id, revision)` that newly
  appears with a pending request and is visible under the service's show
  toggles. Not for a request already notified, and not for the user's own
  countdowns.
- None while any connected view reports its panel open (the main spec
  notifies only while the panel is closed).
- When the request leaves the view (resolved or gone), the service closes
  its notification.
- Text: "<Agent> on <machine> needs you: <request label>"; agent text
  cleaned and truncated as the Mac app does (its exact wording differs).
- One action, "Open". The service accepts the action signal only for a
  notification id it created, from the current owner of
  `org.freedesktop.Notifications`, and then runs
  `omarchy-shell tonibergholm.agentcaller openCard <key>` as an argv
  (no shell), with a 5 s timeout and its output discarded.
- Off switch in settings. Do-not-disturb is the shell's; the bar count
  still updates.

## 9. Secrets and pairing

- Each remote's `MachineSecret` (token and TLS pin, the JSON the Mac app
  stores) is a Secret Service item with attributes
  `service=agent-caller`, `machine=<key>`, in the default collection.
- The secret worker: one thread owns the Secret Service connection; all
  reads, writes, deletes and unlocks go through it with a timeout (read,
  write, delete 5 s; unlock 120 s, as it waits for the user). `Inbox`
  sees `CachedSecrets` filled from the worker; no keyring call runs under
  an `Inbox` lock or blocks a timer or a cancel.
- Locked at start: the service does not prompt. Affected machines show
  `secret: locked`. "Unlock keyring" (`unlock_secrets`) asks the worker
  to unlock (the user sees the keyring prompt); on success the service
  re-reads the secrets and calls `reconnect_machine` for each affected
  machine. A dismissed or timed-out prompt returns
  `secret_store_locked` / `secret_store_timeout` and changes nothing.
- Pairing: the pairing string is parsed, its secret stored through the
  worker, then the machine committed; a failure after storing removes a
  new machine's secret or restores a re-paired machine's previous one. The reply carries the machine key and name only.
- The service never sends a token or pin to a view. The pairing string
  goes from view to service only; it is never logged or echoed.
- The local machine uses the local `acd` socket and token file, as the
  Mac app does; no stored secret.

## 10. Setup

`acd-inbox install`:
1. Copies the binary to `~/.local/bin/acd-inbox` (if run from elsewhere),
   writes the unit (§4.2), and runs `systemctl --user enable --now
   acd-inbox`.
2. Copies the plugin to
   `~/.config/omarchy/plugins/tonibergholm.agentcaller/`, runs
   `omarchy-shell shell rescanPlugins`, then `omarchy plugin enable
   tonibergholm.agentcaller` (the manifest's `defaultSection` places it).
3. Prints the keybinding line (Super+Alt+A → `omarchy-shell
   tonibergholm.agentcaller toggle`) and offers to append it to the
   user's Hyprland bindings: asks first, backs up the file, and changes
   nothing without a yes.

`uninstall` reverses 1 and 2 and prints how to remove the binding.
`status` reports the service, socket, plugin and keyring state.

The Omarchy machine has no Rust toolchain: the binary is built in Docker
(`rust:1-bookworm`, `--platform linux/amd64`) on the Mac, as `acd` is.

## 11. Error handling

| Case | Behaviour |
|---|---|
| Service down | Bar struck-through; panel says the inbox service is not running and how to start it. |
| Service restarts | Plugin reconnects with backoff and gets fresh `settings` and `view`. Unsent countdowns died with the service; nothing was sent. |
| View disconnects | Its unsent countdowns are cancelled (§6). |
| Shell restarts | Service keeps running; plugin reconnects on load. |
| Stale pick | `error stale`; the panel keeps the new view. |
| Remote offline | Chip status, as on the Mac. |
| Keyring locked | `secret: locked` per machine; "Unlock keyring". |
| Keyring prompt dismissed or slow | `secret_store_locked` / `secret_store_timeout`; nothing changes. |
| Notification server absent | Notifications off for the session; logged by class. |
| Malformed or oversized line | Connection closed; logged by tag and length. |
| Outcome queue overflow | `outcomes_lost`; fired actions shown as uncertain. |

## 12. Testing

Rust (CI on Ubuntu and macOS; Linux-only modules on Ubuntu):
- Protocol round trips and the JSON shapes views depend on (`hello`,
  `settings`, `view`, `outcome`, every reply code).
- Each command maps to the right `Inbox` call; stale revision, unknown
  card and duplicate id give their codes.
- No token, TLS pin or pairing string appears in any service message or
  log, including errors from secret-shaped bad input.
- Snapshot order on connect; a slow view gets only the newest view and
  settings; outcomes are not coalesced; overflow sends `outcomes_lost`.
- Undo with two views: Esc cancels only the sender's newest; the card
  cancel control cancels any; a disconnect cancels the disconnected
  view's countdowns; races of Esc, disconnect and expiry end with one
  terminal result each and nothing sent after a cancel; a lagged
  `acd-client` outcome stream sends `outcomes_lost` to every connection.
- Socket startup on a real listener: unsafe runtime dir, symlinked
  socket path, a regular file at the path, a second instance, mode 0600,
  peer check rejecting another uid (where the test can run as root, else
  the check function).
- Secret worker with a fake store: locked start → unlock → machines
  reconnect without restart or re-pairing; dismissed and timed-out
  unlock; no keyring call under an `Inbox` lock (a blocked fake store
  does not stop a cancel or a timer); pairing rollback deletes a new
  machine's secret and restores a re-paired machine's previous one.
- Notifier with a fake D-Bus: one per `(card, request, revision)`, none
  for repeats, own countdowns or while a panel is open; close on resolve;
  actions from an unknown id or sender are ignored.
- `acd-client`: owner-scoped cancel and escape; `reconnect_machine` for a
  missing handle; secret status in the view; the Mac app's tests
  unchanged.
- Logs never contain agent text (log-capture test, as in the voice
  crates).

QML:
- `model.js` tested with node: sections, filters, key handling (text
  fields, IME), flags (read-only, unknown effect, always-allow rule
  shown), typed-answer validation, error texts, split UTF-8 and partial
  lines in the line reader.
- `omarchy-plugin-validate` and `qmllint` in a live check (they need the
  Omarchy machine).

Live checks on omarchy (recorded in `docs/research/`):
1. Answer a real Claude permission request from the panel; Esc undoes.
2. A notification opens the panel on the card; none while it is open.
3. Restart the shell during a countdown: nothing is sent; the panel
   reconnects. Stop the service; the bar shows it.
4. Pair a remote (minos) from the panel; restart; the token comes from
   the keyring. Lock the keyring, restart, unlock from the panel; the
   remote reconnects.
5. A remote's request (minos) appears and is answerable.

## 13. Review decisions (2026-10-12)

Sol's review found five blocking gaps; the orchestrator decided:
1. Undo with several views: actions are owned by their connection; a
   disconnect cancels its countdowns; Esc is per view (§6). This keeps the
   main spec's disconnect rule rather than changing it.
2. Locked keyring recovery: typed secret status and handle recreation
   (§4.3, §9).
3. Keyring prompts never under `Inbox` locks: a secret worker with
   timeouts (§9).
4. Outcomes: a defined JSON form, per-connection queues, `outcomes_lost`
   on overflow, subscribe before execute, duplicate ids rejected (§5).
5. Settings: a `settings` snapshot with machine and secret status and the
   notification preference (§5.4).

The re-review added: pairing rollback restores a re-paired machine's
secret; upstream outcome lag also sends `outcomes_lost`; connections
subscribe to outcomes on accept; settings entries join view machines by
key.

Non-blocking points adopted: pairing-string handling, the socket startup
contract, snapshot-based notifications with open-panel suppression, the
unit's session lifetime, panel flag and input tests, and the hook-only
wording.

# Omarchy shell plugin API as installed (2026-10-12)

Checked on the user's omarchy machine (`ssh toni@omarchy`). Quickshell 0.3.2
(Omarchy build), Qt 6.12.0. The running shell is loaded from
`OMARCHY_PATH=/home/toni/omarchy` (git checkout, `be7c7bf3`), which matches
`/usr/share/omarchy/shell` except `plugins/lock/*` and `plugins/polkit/PolkitAgent.qml`.
Non-interactive `ssh` has no `OMARCHY_PATH`; run `omarchy` and `omarchy-shell`
through `bash -lc` (otherwise: `OMARCHY_PATH is not set`).

Reference plugin: `/usr/share/omarchy/shell/plugins/panels/tailscale/` (`omarchy.tailscale`:
`manifest.json`, `Panel.qml`, `Service.qml`, `Model.js`). A third-party bar widget with the
same shape: `~/.config/omarchy/plugins/tonibergholm.ruuvi/` (`BarWidget.qml`, `Model.js`).
Docs: `~/omarchy/docs/omarchy-shell.md`.

Probe method: a scratch plugin of kind `service` (so it did not touch the bar), enabled with
`omarchy plugin enable`, logging to `journalctl --user -b -o cat | grep probe`. A python
socket server on `$XDG_RUNTIME_DIR/acd-inbox-probe.sock` fed it. The probe is removed (see
the end).

## Summary of differences

| # | Plan assumption | Result |
|---|---|---|
| 1 | flat manifest keys, `barWidget: "BarWidget.qml"` | differs: entry point under `entryPoints`; widget keys nested |
| 2 | `Color.dim/surface/surfaceActive/text`, `Style.spacingSmall/radius` | differs: those names do not exist |
| 3 | raw `IpcHandler` | differs: use `Commons.ShellIpc` (same API) |
| 4 | `.mjs` import | holds |
| 5 | no `TextEncoder`/`TextDecoder` | holds (both `undefined`) |
| 6 | `Socket` API | holds |
| 7 | `SplitParser` whole lines | holds |
| 8 | key events | partly verified; live key order needs the user |
| 9 | `actions` capability | holds |
| 10 | lifecycle | holds, with two caveats (hot reload, `bash -lc`) |
| 11 | `bindings.conf` / `bindd` | differs: `bindings.lua` / `o.bind` |
| 12 | validator, qmllint | differs: `qmllint` is not on PATH; import path needs a `qs` link |

## 1. Manifest

`/usr/share/omarchy/shell/plugins/panels/tailscale/manifest.json`:

```json
  "id": "omarchy.tailscale",
  "kinds": ["bar-widget"],
  "entryPoints": { "barWidget": "Panel.qml" },
  "barWidget": {
    "displayName": "Tailscale",
    "category": "Network",
    "allowMultiple": false,
    "defaultSection": "right",
    "defaults": { "refreshIntervalSec": 30 },
    "schema": [ { "key": "refreshIntervalSec", "type": "integer", "label": "...", "min": 5, "max": 3600, "step": 5, "defaultValue": 30 } ]
  }
```

Top level also has `schemaVersion: 1` (must be the number 1), `name`, `version`, `author`,
`license`, `description`. `omarchy-plugin-validate` requires `schemaVersion id name version kinds
entryPoints`, rejects ids starting `omarchy.`, requires each kind's entry point
(`bar-widget` -> `entryPoints.barWidget`, `service` -> `entryPoints.service`), requires
`barWidget.defaultSection` in `left|center|right`, forbids symlinks.

A bar widget and its popup are one QML file: the entry point is a `Panel` (from `qs.Ui`) whose
children draw the bar button and a `KeyboardPanel` popup (see ruuvi's `BarWidget.qml`:
`Panel { moduleName: ...; manageIpc: false ... KeyboardPanel { anchorItem: button; owner: root;
bar: root.bar; open: root.opened; focusTarget: keyCatcher ... PanelKeyCatcher {...} } }`).
There is no separate panel declaration.

Plan says: `kinds: ["bar-widget"]`, `defaultSection`, `barWidget: "BarWidget.qml"`.
**Differs.** Use:

```json
{"schemaVersion": 1, "id": "tonibergholm.agentcaller", "name": "...", "version": "...",
 "author": "...", "license": "...", "description": "...",
 "kinds": ["bar-widget"], "entryPoints": {"barWidget": "BarWidget.qml"},
 "barWidget": {"displayName": "...", "category": "AI", "allowMultiple": false,
               "defaultSection": "right", "defaults": {}, "schema": []}}
```

`category` and `defaultSection` live inside `barWidget`. The id must not start with `omarchy.`.

## 2. Imports and names

`/usr/share/omarchy/shell/Commons/qmldir` and `Ui/qmldir`:
`singleton Color 1.0 Color.qml`, `singleton Style 1.0 Style.qml`, `ShellIpc 1.0 ShellIpc.qml`,
`Panel 1.0 Panel.qml`, `KeyboardPanel`, `PanelKeyCatcher`, `TextField`, `Button`.
Qualified imports `import qs.Ui as Ui` / `import qs.Commons as Commons` work (probe loaded
`Ui.Panel` and `Commons.ShellIpc`). Docs say `Color` must be qualified (Qt 6.12 has its own
`Color`): `~/omarchy/docs/omarchy-shell.md`, "Shell and plugin QML must qualify the palette
singleton". `Style`, `Util`, `Border` may stay unqualified (`import qs.Commons`).

Probe output (`Commons.Color`): `foreground #ece5d8, background #151e26, accent #f3b77b,
urgent #ef8d80, muted #88979f, popups.text #ece5d8, popups.background #151e26`.
`Commons.Color.dim`, `.surface`, `.surfaceActive`, `.text` are all `undefined`. Source
(`Commons/Color.qml:19-23`): `property color foreground`, `background`, `accent`, `urgent`, `muted`;
surface groups `popups`, `bar`, `menu`, `tooltip`, `notifications` (each `.background`, `.text`,
`.border`).

`Style` (probe): `Style.spacing.sm = 4`, `.md = 7`, `.lg = 9` (scaled by font scale),
`Style.cornerRadius` (12 here), `Style.space(10)` -> 11, `Style.font.body` 13, `.bodySmall` 12,
`.heading` 17, `Style.font.family`. `Style.spacingSmall`, `Style.spacing.spacingSmall` and
`Style.radius` are `undefined`.

`Panel` (`Ui/Panel.qml`): `open()`, `close()`, `toggle()`, `readonly property bool opened`,
`property QtObject bar`, `moduleName`, `setting(name, fallback)`, `switchPanel(direction)`,
`ipcTarget`, `manageIpc`. Probe: `typeof open/close/toggle = function`, `opened = false`.

Changes for Tasks 17-21:
- `Color.urgent`, `Color.accent`: hold. `Color.text` -> `Commons.Color.foreground` (or
  `bar ? bar.foreground`, as tailscale does). `Color.dim` -> `Commons.Color.muted` (or
  `Qt.darker(foreground, 1.55)`). `Color.surface` -> `Commons.Color.popups.background`.
  `Color.surfaceActive` has no equivalent: use
  `Qt.rgba(fg.r, fg.g, fg.b, 0.05)` for a card and `0.12` for the active row (ruuvi uses
  `.05`), or `Style.hoverFillFor(foreground, accent)` / `Style.selectedFillFor(...)`.
- `Style.spacing` -> `Style.spacing.sm` (or `Style.space(n)`); `Style.spacingSmall` -> `Style.spacing.xs`;
  `Style.radius` -> `Style.cornerRadius`.
- Popup content goes inside `KeyboardPanel` (not a bare `Panel`).

## 3. IPC

`Commons/ShellIpc.qml`: `IpcHandler { Component.onCompleted: IpcRegistry.register(handler) ... }`
("An IpcHandler the shell can also answer over its own socket ... so omarchy-shell reaches it
without a qs ipc client"). `Ui/Panel.qml` itself uses it (`ShellIpc { enabled: root.manageIpc &&
root.ipcTarget !== ""; target: root.ipcTarget; function toggle(): void ... }`).

Probe:

```qml
Commons.ShellIpc {
    target: "tonibergholm.agentcaller"
    function openCard(key: string): void { console.log("ipc openCard", key) }
    function noargs(): void { ... }
    function ret(a: string): string { return "ret:" + a }
}
```

Results (all via `bash -lc`): `omarchy-shell tonibergholm.agentcaller openCard abc` -> log
`ipc openCard abc`, rc 0. `... openCard "k 1/2"` -> arg arrives whole. `... noargs` rc 0.
`... ret x` prints `ret:x`. Wrong argument count: stderr
`Too few arguments provided (1 required but 0 were provided.)` plus the function definition, rc 1.
Unknown function: `Function not found.`, unknown target: `Target not found.`, rc 1.
Target names are free-form (`omarchy.tailscale`, `tonibergholm.agentcaller`); `omarchy-shell <target> <fn> <args...>`
maps to the handler function. `omarchy-shell` needs `OMARCHY_PATH` (set in the session; unset in plain ssh).
Panel's own `manageIpc: true` target gives `open close show hide toggle` (`omarchy-shell tonibergholm.probe toggle` rc 0).

Plan says: `IpcHandler { target: ...; function openCard(key: string): void }`.
**Differs (minor):** use `Commons.ShellIpc { ... }` with the same body. A plain `IpcHandler` is not in
`IpcRegistry`, so `omarchy-shell` takes the slower `qs ipc` fallback. The widget must be
loaded (enabled) for the target to exist; with `Panel` set `manageIpc: false` on the Panel if
the widget declares its own `ShellIpc` for another target (tailscale and ruuvi do).

## 4. `.mjs` import

Probe: `import "probe.mjs" as P` with `export function answer() { return 42; }` -> log
`probe: mjs import 42`. **Holds** (`import "model.mjs" as Model`). First-party code uses
`Model.js` with plain top-level functions and `import "Model.js" as Model`
(tailscale `Service.qml`, ruuvi `BarWidget.qml`); both forms work.

## 5. Host globals

Probe: `probe: TextEncoder undefined TextDecoder undefined`. **Holds** (plan does not rely on them).
`Quickshell.env("XDG_RUNTIME_DIR")` is non-empty.

## 6. `Socket`

`/usr/lib/qt6/qml/Quickshell/Io/quickshell-io.qmltypes`: `Socket` has `connected`, `path`,
signal `error(error)`, `connectionStateChanged`, methods `write(data: string)`, `flush()`; it
derives from `DataStream` (`parser`). Probe:
- `connected: true` at creation connects; log `connected true`.
- Peer close: `WARN quickshell.io.socket: ... PeerClosedError`, `onError` receives `1`
  (`ConnectionRefusedError` is `0`), then `connectedChanged` -> `false`.
- Reconnect: after the peer closed, `sock.connected = true` (from a Timer, 700 ms later)
  connected again to the next accept (`reconnect toggle`, `connected true`, second line received).
- `write("pingé😀\n"); flush()` arrived at the server as UTF-8:
  `b'ping\xc3\xa9\xf0\x9f\x98\x80\n'` (both connections).
- Connecting to a path with no listener: `error 0`, `connected` stays false.

**Holds** as named.

## 7. `SplitParser`

Server sent `{"t":"é😀"}\n` split at byte 9 (inside the 4-byte `😀`) with 0.5 s between chunks,
then `{"t":"two"}\n`. Probe log: `line "{\"t\":\"é😀\"}" 11` then `line "{\"t\":\"two\"}" 11`.
The split character arrived whole and decoded; one `read` per line, newline stripped; the JS
length 11 counts `😀` as two UTF-16 units. **Holds**; no byte handling needed (D20).

## 8. Key events

Static only. `Ui/PanelKeyCatcher.qml` sets `Keys.priority: Keys.BeforeItem`, handles
`Escape`, `Tab`, `j k h l`, arrows, `Return`, `Space`, `x`, and emits `textKey(text, modifiers)`;
it returns immediately when `blocked` is true: "When a panel has an inline editor ... the panel
must set `blocked: editor.activeFocus`". `Ui/TextField.qml` extends QQC `TextField`; probe:
`typeof tf.inputMethodComposing` = `boolean`, value `false`, so the property exists.
Qt's `Keys.onPressed` on an item runs before the item's own text handling by default. **Not
verified live: needs user** (see "Needs user"). Task 19 must set `PanelKeyCatcher.blocked` to
the field's `activeFocus`, or letters such as `j`, `k`, `x` are taken by the catcher.

## 9. Notification server

`busctl --user call org.freedesktop.Notifications ... GetCapabilities` ->
`as 6 "persistence" "body" "body-markup" "body-hyperlinks" "actions" "icon-static"`.
`GetServerInformation` -> `"quickshell" "quickshell" "" "1.2"`. `busctl status`: `Comm=quickshell`,
`Exe=/usr/bin/quickshell`. **Holds** (lists `actions`). `~/omarchy/docs/notifications.md`: left-click
invokes the default action; DND silences unless `app_name` is `omarchy-action`.
No test notification was sent.

## 10. Lifecycle

- `omarchy-shell shell rescanPlugins` -> picks up a new plugin dir; `listPlugins` JSON shows
  `{"id":"tonibergholm.probe","kinds":["service"],"enabled":false,...}` before enable.
- `omarchy plugin enable <id>` printed `Enabled <id>`; for a service it appended
  `{ "id": "<id>" }` to `plugins[]` in `~/.config/omarchy/shell.json`; `disable` removed it
  (file identical to the pre-probe copy). Bar widgets are placed per docs in `bar.layout.<section>`
  at `barWidget.defaultSection` (docs, not probed: the probe was a service, to keep the bar untouched).
- Files in an installed plugin are watched (`Local plugin changed, reloading: <id>`).
  **Caveat found:** editing an already loaded entry file did not take effect (the old
  code kept running after save, rescan, and disable/enable), and renaming the entry file failed
  with `service plugin load failed ...: File name case mismatch`. A fresh plugin directory loaded
  the new code at once. So the dev loop for Tasks 17-21 is: restart the shell
  (`omarchy-restart-shell`, user's call) after changing `.qml` files of a loaded plugin, or install
  under a new directory name. `.mjs`/`.js` edits were not separately tested.
- Load errors do surface: `journalctl --user -b -o cat | grep "load failed"` (long lines are
  shown whole there).
- `bash -lc` is needed for `omarchy` / `omarchy-shell` from ssh.

**Holds**, with the caveats above.

## 11. Hyprland bindings

`~/.config/hypr/` is Lua: `bindings.lua`, `hyprland.lua`, `autostart.lua`, ... (no `bindings.conf`).
`bindings.lua` header: "Keep only your personal keybinding overrides here". Syntax
(`~/omarchy/default/hypr/helpers.lua:138`): `function o.bind(keys, description, dispatcher, options)`,
a string dispatcher runs as `hl.dsp.exec_cmd`. Example in the file:
`-- o.bind("SUPER + PERIOD", nil, "omarchy-shell shell toggle omarchy.emojis")`.
`hyprctl binds -j` shows `SUPER+ALT+A` (modmask 72) free; taken: `A` with masks 64, 65, 68, 69, 73
(Select all, ChatGPT, Audio, Agent, Grok).

Plan says: `~/.config/hypr/bindings.conf`, `bindd = SUPER ALT, A, Agent inbox, exec, ...`. **Differs.**
Use, in `~/.config/hypr/bindings.lua`:

```lua
o.bind("SUPER + ALT + A", "Agent inbox", "omarchy-shell tonibergholm.agentcaller toggle")
```

(function name from Task 17-19's `ShellIpc`; the plugin must expose a no-argument function for
the binding). The README should show this form. Nothing was written to `bindings.lua`.

## 12. Validator and lint

- `omarchy plugin validate <dir>` (`/usr/bin/omarchy-plugin-validate`, a bash/jq script) returned 0 for
  the probe. It checks the manifest only.
- `qmllint` is not on PATH; it is `/usr/lib/qt6/bin/qmllint` (6.12.0, package `qt6-declarative`).
  `qmllint6` does not exist.
- `qs.Commons` and `qs.Ui` are not found with `-I $OMARCHY_PATH/shell`
  (`Failed to import qs.Commons`). They resolve with a link whose name is `qs`:
  `mkdir /tmp/qsroot && ln -s $OMARCHY_PATH/shell /tmp/qsroot/qs`, then
  `/usr/lib/qt6/bin/qmllint -I /tmp/qsroot -I /usr/lib/qt6/qml <file>.qml`. On ruuvi's
  `BarWidget.qml` that gives 0 import failures and style warnings only (exit 0). Without the
  `qs` link: 2 import failures.
- Even with the link, qmllint reports `Member "family" not found` style warnings on the singletons;
  treat warnings as non-fatal and `Failed to import` as fatal.
- Quickshell: `qs --version` -> `Quickshell 0.3.2 (revision , distributed by Omarchy)`.

## Needs user

1. Live key order (item 8): open the widget's popup, focus the filter/answer `TextField`, type
   `j`, `k`, `x`, and a composed character; the field must receive them while
   `PanelKeyCatcher.blocked` is true. Checkable only on the screen, after Task 19.
2. Applying the keybinding (item 11) means editing `~/.config/hypr/bindings.lua`; this task did not.
3. Shell restart after QML edits (item 10), when a loaded plugin's entry file changed.

## Cleanup

Probe plugin directories removed, `omarchy plugin disable` run, `rescanPlugins` run, and
`listPlugins` shows no `probe`; `~/.config/omarchy/shell.json` identical to the copy taken before
enabling; scratch files and the probe socket removed from `/tmp` and `$XDG_RUNTIME_DIR`.
The probe used plugin ids `tonibergholm.probe` and `tonibergholm.probe2` (a second directory,
because of the reload caveat in item 10).

# Claude Code 2.1.295 live verification: spec §11 gates 2, 5 and 7

Method: isolated herdr session `p3c` in detached tmux, Claude run as `claude --model haiku --permission-mode default`
(Haiku 5.5) in scratch dirs `/tmp/acp3/claude/s1..s4` with project-local `.claude/settings.local.json` only. Stand-in
for acd: `python3 -I` asyncio listener on 127.0.0.1:47931 that logged every request (token values hashed), could hold,
stall, allow, deny or reject by path, and checked `X-Acd-Token` against a token file. A second `PermissionRequest`
hook (`?h=fast`) was configured next to the holding one (`?h=main`) for items 2c/2d. A background sampler polled
`~/.claude/sessions` every 50 ms (read-only, own sessions only). Fixtures: `tests/fixtures/hooks/claude/`
(`settings-http-hooks-env-token.json`, `settings-local-after-addRules.json`, `payload-Stop-after-deny.json`,
`session-registry-entry.json`, `session-registry-status-sequence.jsonl`, `ui-hook-error-401.txt`).
The user's `~/.claude/settings.json` also has hooks; they ran alongside ("Ran 7 stop hooks"). That did not affect results.

## Summary

| Check | Verdict |
|---|---|
| 2a header env interpolation | PASS (listener got the real token; unlisted var became an empty string). Config is read at session start / `/clear`, not on file edit |
| 2b held connection closes on terminal answer | PARTIAL. Closes in about 15 ms on terminal **deny** and **Esc**. Does **not** close on terminal **allow**: stays open until the hook `timeout` |
| 2c held hook while another decides | PASS. Claude does not wait for the held hook. The held connection stays open until timeout (or until acd replies) |
| 2d late reply ignored, outcome evidence | PASS. Late allow after deny and late deny after allow had no effect. Evidence table below |
| 2e `allow_always` persists | PASS in a new session, with a directory caveat |
| 2f stalled endpoint | PASS. Lifecycle hooks proceed after exactly 1 s with a visible notice. PermissionRequest honours its configured timeout and the dialog is usable throughout |
| Unexpected: SessionStart over http | FAIL. No http request is ever sent for SessionStart |
| Gate 5 session registry | PASS, with findings on `status`, `waitingFor`, stale entries, `procStart` timezone |
| Gate 7 token rotation | Design proposed. Running sessions keep the launch-time token. A rejected token is visible, non-blocking, and fails open |

## Gate 2

### a. Header interpolation and when config is read

- Settings: `X-Acd-Token: $ACP3_TOKEN`, `X-Herdr-Pane: $HERDR_PANE_ID`, `X-Not-Allowed: $ACP3_NOTALLOWED`;
  `allowedEnvVars: ["ACP3_TOKEN","HERDR_PANE_ID"]`; the process was launched with `ACP3_TOKEN=tok-A-...` and
  `ACP3_NOTALLOWED=secret-notallowed`.
- Observed at the listener: `x-acd-token` hashed equal to the token (auth `ok`), `x-herdr-pane: w1:p1`,
  `x-not-allowed: ""` (header sent, empty value; the secret was not leaked). Matches the docs: "References to unlisted
  variables are replaced with empty strings".
- Request shape: POST, `content-type: application/json`, `user-agent: axios/1.15.2`, body = the hook JSON.
- Mid-session edit of `settings.local.json` (added header `X-Edit`, removed `HERDR_PANE_ID` from `allowedEnvVars`):
  the next hook request still had no `X-Edit` and still interpolated `HERDR_PANE_ID`. **Hook config is a snapshot taken
  at session start; file edits are not re-read.**
- `/clear` (same process, new `session_id`; SessionEnd fires for the old one) **does** re-read the hook config: the
  edited `X-Edit: after-edit` header appeared on the next UserPromptSubmit. Env vars cannot change in a running
  process, so an env-based token is fixed for the process lifetime. A literal header is refreshed by `/clear`,
  resume or restart only.
- SessionStart: **no http request was sent**, in two sessions. The docs state "Only `type: "command"` and
  `type: "mcp_tool"` hooks are supported" for SessionStart (and `command` only for Setup). The §6.2 table lists
  SessionStart as `http`; that cannot work. Session discovery (registry scan, `UserPromptSubmit`) must cover it.

### b. Held connection and a terminal answer

Held the `PermissionRequest` connection, answered in the terminal, watched the listener's socket (EOF detection) and
`netstat`:

| Terminal action | Held connection |
|---|---|
| `4` (No) | closed 14 ms after the keystroke (held 6.344 s) |
| `Esc` | closed 15 ms after the keystroke (held 5.945 s) |
| `1` (Yes) | **stayed ESTABLISHED**; closed only at the hook timeout (30.019 s with `timeout: 30`; 120.015 s with `timeout: 120`). PostToolUse and Stop arrived normally meanwhile |

The dialog is shown while the hook is held (screen showed it about 1 s after the request), so §4 invariant 1 holds.
**This contradicts the spec's "when the user answers in the terminal, the held connection closes" and P0 item 8's
claim for allow.** Close-on-deny/Esc looks like an abort of in-flight hooks when the permission is rejected; allow
does not abort them.

### c. Another hook decides while acd holds

- `fast` replied allow immediately: tool ran, `PostToolUse` arrived 46 ms after the request. Claude did not wait for
  the held hook.
- Held connection after that: still open (no EOF) until acd replied (3.9 s case) or the timeout (30.0 s case).
  Writing a reply to it caused no visible effect and Claude then closed the socket.
- `fast` replied deny: Claude showed "Denied from fake acd" to the model; the tool did not run.

### d. Late reply ignored, and evidence for `terminal`

- fast deny, then main allow written 12.6 s later: file not created, no PostToolUse, no new dialog, no further hook
  traffic. Late allow had **no effect**.
- fast allow, then main deny written 3.9 s later: tool ran, PostToolUse had arrived before the late reply. Late deny
  had no effect.
- Evidence after a denial: `UserPromptSubmit`, no `PostToolUse`, `Stop` 0.96 s after the request.
  `Stop.last_assistant_message` quotes the denial text (fixture `payload-Stop-after-deny.json`).
- Evidence after an allow: `PostToolUse` (same `tool_name`/`tool_input`) about 50 ms to 1.4 s after the request, then `Stop`.
- Hooks not configured in this run (`PostToolUseFailure`, `PermissionDenied`) were not checked.

So the §6.2 correlation works as written: acd allow + no PostToolUse by Stop means `terminal` (someone denied);
acd deny + PostToolUse means `terminal`.

### e. `allow_always` persistence

- Reply `addRules` / `ruleContent: "touch /tmp/acp3/claude/*"` / `destination: localSettings` (same shape as
  `response-allow-always-localSettings.json`). Claude wrote `{"permissions":{"allow":["Bash(touch /tmp/acp3/claude/*)"]}}`
  into `.claude/settings.local.json` (fixture `settings-local-after-addRules.json`) and `/permissions` lists it.
- Claude **reformatted the whole file** (indent 1 to indent 2), replaced it atomically (new inode), and **kept mode 0600**
  when the file was 0600.
- New session, same dir: `touch /tmp/acp3/claude/s3/y-1.txt` and `y-2.txt` ran with **no PermissionRequest hook and no
  dialog**.
- Caveat: in the new session `touch /tmp/acp3/claude/x-12.txt` (cwd `.../s3`, path outside the cwd) **still prompted**
  and the hook fired, although the rule matched textually. So an allow rule does not cover paths outside the working
  directory. acd should not promise "no more prompts" for such commands.
- `permission_suggestions` for these Bash requests contained only `addDirectories` (session) and `setMode`
  (session), no `addRules`. Per §6.2 the `allow_always` option is offered only with an `addRules` suggestion, so it
  was not offered for any request seen here.

### f. Stalled endpoint

- Lifecycle hook, `timeout: 1`, listener never replies: Claude cancelled at 1.001 s (UserPromptSubmit) and 1.000 s (Stop),
  continued, and printed `UserPromptSubmit hook [url] timed out after 1s - output discarded`. Registry showed
  busy at +3 ms and idle 1.0 s after the Stop request: a stalled Stop delays the turn ending by 1 s, a stalled
  UserPromptSubmit delays the model by 1 s. SessionEnd/PostToolUse were not stalled separately.
- PermissionRequest, `timeout: 30`, listener stalls: dialog visible within the first seconds and usable;
  connection cut at exactly 30.0 s; the dialog stayed open and answerable afterwards; no message was shown.

## Gate 5: session registry `~/.claude/sessions`

(Read-only. Own sessions only; other sessions in aggregate.)

- Fields, all entries: `pid, sessionId, cwd, startedAt (ms), procStart, version, peerProtocol, peerFeatures[], kind,
  entrypoint, pidDomain, messagingSocketPath, name, nameSource, nameSince, updatedAt, statusUpdatedAt, status`, plus
  `waitingFor` while waiting. Each entry has a sibling `<pid>.<64-hex>.key` that appears and disappears with it
  (never opened). Directory mode 0700.
- `status` values observed: `idle`, `busy`, `waiting`. `waiting` carries `waitingFor`: `"permission prompt"` for
  the permission dialog and `"dialog open"` for the `/permissions` UI. Sequence per turn:
  `idle -> busy -> waiting(permission prompt) -> busy -> idle`; deny/Esc goes `waiting -> idle` directly.
  `/clear` keeps the pid and file, changes `sessionId`, and goes busy -> idle. A stalled Stop hook kept `busy` for 1 s.
- Latency: file appears about 1.2 s after launch with no `status`, then `status: idle` within 120 ms. A change is
  visible in the file at most 120 ms after `statusUpdatedAt`. On `/exit` the file vanished about 0.1 s after the
  command, before the SessionEnd hook arrived.
- `kill -9`: the entry **stays** (`status: idle`, process gone). Left in place, as required:
  `14517.json` and its `.key` (from my s4 test). Validation by pid + `procStart` is therefore necessary.
- `procStart` is `ps -o lstart=` rendered in **UTC** (`Fri Oct  9 07:29:36 2026` for a start at 10:29:36 EEST, equal
  to `startedAt`). Compare with `TZ=UTC ps -p <pid> -o lstart=` or via `startedAt`.
- `entrypoint` is `sdk-ts` for my sessions (inherited from the launching Claude shell), which confirms why
  entrypoint alone cannot prove T3 ownership.
- Unreadable entries (copies in `/tmp/acp3/claude/regcopy`): half-truncated file and empty file give a JSON parse
  error; mode-0 file gives `PermissionError`; unknown `status` (`frobnicating`) parses fine; missing `procStart` parses
  fine. A parser must treat parse and permission errors as skip + log once, unknown status as `unknown`, and a missing
  `procStart` as unverifiable (skip). Writes seem atomic enough that no truncated file was seen in the sampler's
  live reads (no `unreadable` event over about 11 min, about 90 changes).
- Other sessions: 2 live entries by the user besides mine; same field set; same file layout.

## Gate 7: token rotation

Observed:

- A running session sends the token from its environment at launch (env-based header) or from the settings
  snapshot (literal header). Neither changes by rewriting the file (2a). Only `/clear`, resume or restart re-read
  settings; env tokens never change.
- Listener rejecting the token with 401 or 403 (`rotation` simulated by accepting only the new token):
  - UserPromptSubmit and Stop: non-blocking, **visible** in the transcript: `UserPromptSubmit hook error` /
    `HTTP 401 from <url>`, `Stop hook error: HTTP 401 from <url>`, footer `Stop hook error occurred - ctrl+o to see`.
    Same with 403. The turn proceeds with no added delay (reply is immediate).
  - PermissionRequest: no message; the normal dialog appears at once (fail open).
- Accepting both old and new tokens: all requests authenticated; nothing visible.

Design consequence: the spec's 10-minute grace (§6.1) is far too short with literal headers, because every Claude
session started before the rotation keeps the old token for its whole life (days), and after expiry each turn shows
two error lines to the user and acd silently loses permission requests.

## Spec changes (proposed)

1. **§6.1 / §6.2: SessionStart is not an http hook.** Remove it from the installed-hooks table; drop "a hooks-only agent
   can exist before its first hook" reliance on it. Discovery relies on the registry scan plus `UserPromptSubmit`. If a
   SessionStart signal is wanted, it needs a `command` hook (not planned for v1).
2. **§6.1 "When the user answers in the terminal, the held connection closes."** Replace with: the held connection
   closes within about 15 ms when the user **denies or presses Esc**; it **does not close** when the user allows in
   the terminal, and remains open until `permission_hold + 10 s`. §6.2 step 4 therefore only fires for deny, Esc,
   interruption or process death. For a terminal allow, acd learns the outcome from `PostToolUse` / the registry
   (`waiting -> busy`) / the fingerprint leaving the screen. Close the held connection with an empty reply once that
   evidence arrives.
3. **§6.1 coexisting hooks:** add "Claude does not wait for acd's held hook when another hook decides; a reply written
   afterwards is ignored; if acd replies empty it is also harmless." Keep the reconciliation rule.
4. **§6.2 attribution:** keep as written; add that a denial leaves `UserPromptSubmit`/`Stop` with no `PostToolUse` and
   that `Stop.last_assistant_message` often quotes the denial text.
5. **§6.2 `allow_always`:** state that the rule is built from the command (`ruleContent`) because Bash requests carry no
   `addRules` suggestion, so the option exists only when acd constructs it. Persisted rules do not cover paths outside
   the session's working directory, so the client text must not promise the prompt will never return. Claude rewrites
   the whole settings file (indent 2) atomically and preserves its mode.
6. **§6.2 timing:** keep the 1 s lifecycle timeout; note the visible message `... timed out after 1s - output discarded`
   and that a stalled Stop delays turn end (idle) by 1 s.
7. **§6.2 discovery:** add `status` values `idle|busy|waiting` and the `waitingFor` field (`permission prompt`, `dialog open`
   observed). `waiting` with `waitingFor == "permission prompt"` is a cheap cross-check that a dialog is open; and a
   `waiting -> busy` transition after an unanswered hold is evidence of a terminal allow. Compare `procStart`
   in UTC (`TZ=UTC ps -o lstart=`) or against `startedAt`; a killed process leaves its entry behind.
8. **§6.1 token rotation (replace the 10-minute grace):**
   - Claude hook headers should not carry the long-lived token as a literal that sessions snapshot. Prefer one of:
     (a) a stable per-install **hook credential** that is not rotated while any registered Claude session is alive
     (rotation = add a new credential, keep the old one valid until no live registry entry with
     `startedAt` older than the rotation remains, with an upper bound such as 30 days); or
     (b) the `$ACD_HOOK_TOKEN` env approach, which has the same lifetime problem and needs the same retention.
   - Rule for acd: after `acd token rotate hook`, accept the previous token **until every Claude session in the
     registry that started before the rotation has exited** (registry scan, pid + `procStart` validated), not for a
     fixed 10 minutes. Show `acd status` "hook token rotation pending: N sessions on the old token" meanwhile.
   - Sessions that reach the old token after it is retired receive 401; the user sees `hook error / HTTP 401` on
     each lifecycle hook and permission requests silently fall back to the terminal. acd should log every rejected
     token per `session_id`/`X-Herdr-Pane` (rate-limited) and surface "N Claude sessions use a retired hook token;
     `/clear` or restart them" in `acd status` and the client.
   - `/clear` re-reads settings, so rewriting literal headers helps sessions after their next `/clear`/restart, but
     never a session that simply keeps running.
9. **§11 items 2, 5, 7:** mark as checked 2026-10-09 with this document; add the open point "terminal allow does not
   close the held hook" to §6.2 step 4, and "PostToolUseFailure / PermissionDenied evidence not checked".

## Not verified

- `PostToolUseFailure` and `PermissionDenied` hook payloads after a denial.
- Behaviour when several Claude sessions share one listener under token rejection (one session at a time tested).
- `allow_always` for a request with a genuine `addRules` suggestion (none produced by Bash requests here).
- Resume (`claude --resume`) re-reading the header token (inferred from `/clear`, not run).

## Cleanup

Listener and sampler stopped; herdr session `p3c` stopped and `~/.config/herdr/sessions/p3c` deleted; tmux session gone;
throwaway tokens file and reply files deleted. Session files written by Claude (not deleted):
`~/.claude/projects/-private-tmp-acp3-claude-s1`, `-s2`, `-s3` (s4 left no project dir). Stale registry file left by the
`kill -9` test: `~/.claude/sessions/14517.json` (+ its `.key`).

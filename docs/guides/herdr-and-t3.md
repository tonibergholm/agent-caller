# herdr and T3 Code

What each source can answer, what it can only show, and how to set it up.
Hooks (Claude Code, Codex) are in
[hooks-claude-code-and-codex.md](hooks-claude-code-and-codex.md).

## What can be answered

| Source | You can answer | Shown read-only |
|---|---|---|
| Claude Code hooks | Permission prompts (reply to the held hook). Single-question, single-select questions, when the agent runs in a herdr pane acd can identify. | Multi-question and multi-select questions. A question whose dialog on screen does not match. |
| Codex hooks | Permission prompts and single-select questions, by one key through herdr. | The same shapes as above. No "Yes, always" for Codex. |
| herdr | Claude Code and Codex dialogs (one option key, after acd re-reads the screen). | Every other agent herdr detects: you see its state and, when blocked, a screen preview. |
| T3 Code | Approvals, single-select questions, and typed answers where the question allows custom text. Follow-ups to idle threads. | Multi-select and multi-question requests. |

Cards are whole or read-only. acd never offers a part of a request:

- A permission whose text is over the display limit (a detail cut at 3800
  characters, or a request over 40 KiB) has no options. The card says to
  answer in the terminal.
- "Yes, always" appears only when the whole rule it will save can be shown.
- A read-only card names where to answer: **Answer in T3** for T3 agents,
  **Answer in terminal** for the rest, **Answer in <producer>** for an
  external producer.
- If acd cannot prove which agent and screen an answer belongs to, it does
  not deliver it. The card says "Not ready yet. Enlarge the pane to answer
  here." or "The terminal shows a different dialog. Nothing was sent."

Answers through herdr are best effort: a terminal answer can land between
acd's screen check and its keystroke. acd never retries. The card then says
"Check the terminal" or "A stray keystroke may be in the input box".
Follow-ups go through herdr or T3. An agent acd sees only through hooks cannot
receive one.

## herdr

Needs herdr 0.9.3 or newer (below 0.10). No setup. acd looks for
`$HERDR_SOCKET_PATH`, or `~/.config/herdr/herdr.sock`, plus the session
sockets under `~/.config/herdr/sessions/`, every 30 seconds. Add other
sockets in `config.toml`:

```toml
[herdr]
enabled = true
extra_sockets = ["/path/to/herdr.sock"]
```

A socket that is not owned by you or can be changed by others is refused
(a warning in the daemon log). Source status, in `acd status`: `unconfigured`
with "no herdr socket found" when none exists; `degraded` with
`<socket>: <reason>` when one is down or incompatible.

## T3 Code

1. Issue a token and keep it private:

   ```sh
   (umask 077; t3 auth session issue \
     --scope orchestration:read --scope orchestration:operate \
     > ~/.config/acd/t3.token)
   ```

2. Set the server address in `~/.config/acd/config.toml`:

   ```toml
   [t3]
   url = "http://127.0.0.1:3773"
   ```

   Use the port your T3 server listens on. The URL is scheme, host and port.
   The host must be `localhost`, a loopback IP or a Tailscale IP, because the
   token travels over plain `ws://`. MagicDNS names are refused.

3. Run `acd service install` again (or restart `acd run`). The daemon reads
   its config at start.

Source status messages and what to do:

| Message | Fix |
|---|---|
| `set t3.url in config.toml` | Add `[t3] url`. |
| `T3 token missing: save t3 auth session issue output to <path> with mode 0600` | Create the token file. |
| `t3.token must be mode 0600` | `chmod 600 ~/.config/acd/t3.token`. |
| `t3.token cannot be read`, `t3.token is not a valid token` | Re-issue the token. |
| `T3 rejected the token` | Issue a new token with both scopes. |
| `T3 does not speak orchestration protocol 2` | Update T3 or acd. |
| `cannot reach T3` | Start T3, check the URL and port. |
| `N T3 items could not be read (see the debug log)` | Some items are skipped. Update acd. |

T3 message text never reaches the acd logs, only tags, lengths and hashes.

Verified: the table of what is answerable and the status messages are read
from `PROTOCOL.md` (section 6), the design spec (section 6.2) and the
adapter source (`t3/reconciler.rs`, `t3/conn.rs`, `herdr/discovery.rs`); the
card labels from `app/src/components/Card.svelte` and
`crates/acd-client/src/view.rs`. Not verified: any live herdr or T3 session;
the recorded live checks are in `docs/research/` (herdr gate 6, T3 gate 3).

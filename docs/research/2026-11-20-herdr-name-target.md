# herdr agent names as prompt targets (herdr 0.9.5)

Live check, 2026-11-20, herdr 0.9.5 (protocol 22), scratch session from
`tests/e2e/up.sh`, two Claude Code 2.1.329 agents (`w1:p1`, `w2:p1`).

Why: acd sends follow-up text with `agent.prompt`. Targeting the pane id
leaves a window in which another agent can take the pane after acd's last
identity check. `herdr --skill` says a target may be a unique live agent
name, that names match `[a-z][a-z0-9_-]{0,31}`, and that a name is cleared
when its agent is replaced. acd now prompts by name when the agent has one
and herdr is 0.9.5 or later (`crates/acd-adapters/src/herdr/mod.rs`).

| Step | Result |
|---|---|
| `agent rename w2:p1 probe-a` | ok, `name: "probe-a"` |
| `agent get probe-a` | resolves to `w2:p1` |
| `agent rename w1:p1 probe-a` (name in use) | `agent_name_taken` |
| `agent rename w1:p1 w1:p9` (colon) | `invalid_agent_name` |
| `agent prompt probe-a "/help"` | `agent_prompted`, on `w2:p1` |
| `/exit` in `w2:p1`, start a new Claude there | new agent, `name: null`, same `terminal_id` |
| `agent get probe-a` | `agent_not_found` |
| `agent prompt probe-a "/help"` | `agent_not_found`, nothing typed |

Findings:

- Names are unique among live agents and must match herdr's syntax, so a
  name cannot be read as a pane id (`w1:p1`).
- Replacing the agent clears its name, so a prompt by the old name is
  refused instead of reaching the new agent.
- The `terminal_id` stays the same when a new agent starts in the same pane:
  a terminal id alone would not tell the two agents apart.
- Not checked: herdr 0.9.3 and 0.9.4. acd keeps the pane id there.
- Residual: if another live agent is renamed to the same name between acd's
  check and herdr applying the prompt, the text reaches that agent, whose
  screen acd did not read for a dialog. That needs a rename within
  milliseconds by the user.

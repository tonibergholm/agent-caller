# Red team: dynamic attack on a running `acd`

Date: 2026-11-04. Base: `main` at `3404287` (includes #87 sec-local-adapters,
#89 hello-gate test hardening, #90 sec-core-log-notify). This is a dynamic
test: a real `acd` built with `cargo build --release -p acd`, run as a
subprocess, attacked from separate OS processes. It is a sequel to the two
static reviews (`2026-11-03-security-review-core.md`,
`2026-11-03-security-review-local.md`), which are at `108034c`; the delta
between that commit and this one is in "What the static reviews' findings
look like today" below.

Status: **in progress** — this file is being filled in as the session
proceeds. See `tests/redteam/README.md` for how to run the reproducers.

## Method

(to be completed)

## What the static reviews' findings look like today

| # | Finding | Status at 3404287 |
|---|---|---|
| core #1 | Pre-auth slot exhaustion locks out TLS and local clients | (checking) |
| core #2 | No notification for request ids in an earlier epoch | (checking) |
| core #3 | Hostile peer exhausts client memory | (checking) |
| core #4 | No connection/auth log | (checking) |
| core #5 | Notification flood / unbounded dedupe set | (checking) |
| core #6 | Close code 4408 reconnect storm | (checking) |
| core #7 | Pairing host not validated | (checking) |
| core #8 | `agent.send` weaker character rule | (checking) |
| local F1 | Omarchy panel hides tail of card | (checking) |
| local F2 | T3 bearer token to loopback squatter | (checking) |
| local F3 | Hook listeners starved by idle connections | (checking) |
| local F4 | herdr discovery trusts any socket | (checking) |
| local F5 | Service files built from unescaped paths | (checking) |

## Attacks tried

(table to be completed)

## Findings

(to be completed)

## Not tried / not run

(to be completed)

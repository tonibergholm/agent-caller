# Red-team reproducers: dynamic attack on a running `acd`

This directory holds minimal, runnable reproducers for the dynamic
penetration test recorded in `docs/research/2026-11-04-red-team-acd.md`.
That document is the primary deliverable: method, a table of every attack
tried (including the ones that failed), and findings with severity and a
fix suggestion. Read it first.

## Scope and safety

Every script here:

- targets only `127.0.0.0/8` or a Unix socket under a throwaway config
  directory it creates itself (never a real `~/.config/acd`);
- reads tokens only from that throwaway directory, and never prints them;
- is meant to run against a local, disposable `acd` built from this
  checkout — never against a production daemon.

Do not point `ACD_HOST`/`--host` at anything else.

## Requirements

- A release build: `cargo build --release -p acd` from the repo root.
- Python 3 (stdlib only; no pip installs). Scripts use `-I` where noted.
- `useradd`/`runuser`/`setpriv` for the cross-user tests (root in this
  sandbox can create a throwaway second user; see each script's header).

## Running

Each script is self-contained and documents its own command at the top of
the file. Most take `--acd-bin` (default: `target/release/acd` relative to
the repo root) and `--workdir` (default: a fresh temp directory).

See `docs/research/2026-11-04-red-team-acd.md` for the full list, results,
and how to interpret them.

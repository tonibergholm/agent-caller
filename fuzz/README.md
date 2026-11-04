# Fuzz targets

Coverage-guided fuzzing (cargo-fuzz, libFuzzer) for the parsers that read
untrusted bytes. `fuzz/` is its own cargo workspace, like `app/src-tauri`:
the root workspace excludes it, so `cargo test --workspace` never builds it,
and CI does not run it.

| Target | Reads | Invariants asserted |
| --- | --- | --- |
| `protocol_frames` | acd/1 frames, both directions (`decode_server`, `ClientMessage`) | `parse(serialise(x)) == x`; the serialised form passes the committed JSON schema; `Debug` of a client message never shows its token or typed text |
| `pairing` | `acd://host:port?pin=..#token` and arbitrary hosts in a valid line | an accepted line has a valid host (IPv4, IPv6 or hostname), a non-zero port, canonical pin and token, and round-trips; a rejected host never round-trips |
| `hook_mapping` | Claude Code and Codex hook payloads (one or two, split at a NUL byte) | whole-or-read-only: an answerable permission shows its command whole (the escaped detail decodes back to the exact command), within the cap, with no raw control, bidi or invisible character; two requests that differ in what would run never render identically; "yes, always" only for a rule shown whole and saved as shown; an answerable question is one single-select question with distinct options matching the keys sent |
| `t3_map` | T3 websocket frames, shell items, thread projections and events | an answerable command shows the command (and description) whole; a read-only request offers nothing; every offered option is accepted by `respond_payload` with the value shown, a typed answer only where free text is offered |
| `herdr_screen` | a herdr screen (optionally NUL, then a command line) and herdr agent JSON | a dialog is found only when its footer is the last non-empty line; a line added below it loses the match; fingerprint is cut from the digest; the keys for Yes and No name real options |
| `inbox_protocol` | views built from arbitrary cards, encoded under an arbitrary limit; raw bytes through `read_line` | a line is one JSON object under the limit; Needs-you cards are never left out; a card shown answerable shows exactly the request it was given; omitted counts add up; `read_line` never returns an over-long line or invents bytes |
| `text_rules` | any text | `has_forbidden_chars` is exactly "any forbidden character"; the prompt check agrees with it; cleaning and caps are bounded and idempotent; log redaction masks a secret after a known indicator |

Helpers that act as independent oracles (the inverse of the detail escaping,
the hostname rule, the hidden-character set) are in `src/lib.rs`.

## Setup

```sh
rustup toolchain install nightly --profile minimal
cargo install cargo-fuzz --locked
```

## Run

From `fuzz/`:

```sh
cargo +nightly fuzz list
cargo +nightly fuzz run hook_mapping                     # until it finds something
cargo +nightly fuzz run hook_mapping -- -max_total_time=1200
```

`cargo fuzz run <target>` writes new inputs into `fuzz/corpus/<target>`. To
keep the committed corpus small, run against a scratch directory and put the
committed one second (libFuzzer reads every directory, writes to the first):

```sh
cargo +nightly fuzz run hook_mapping /tmp/work/hook_mapping corpus/hook_mapping -- -max_total_time=1200
```

From the repository root, `just fuzz-smoke` runs every target for 30 seconds
this way. A manual GitHub workflow (`Fuzz`, `workflow_dispatch`) runs them
for a time you choose and keeps crashing inputs as an artifact.

A crash is written to `fuzz/artifacts/<target>/`. Reproduce it with
`cargo +nightly fuzz run <target> fuzz/artifacts/<target>/crash-...`.
To fix one: add a named regression test next to the production code with
the crashing input as a fixture, check that it fails without the fix, then
make the fix.

## Corpus

`fuzz/corpus/<target>` holds the seeds: the recorded herdr screens and
API replies, the Claude hook payloads, the acd/1 examples and hand-written
cases for the rest, merged down with `cargo fuzz cmin`. Keep it under
2 MiB in total. To refresh a corpus from a long run:

```sh
cargo +nightly fuzz cmin hook_mapping /tmp/work/hook_mapping   # into the work dir
rm -rf corpus/hook_mapping && cargo +nightly fuzz cmin hook_mapping /tmp/work/hook_mapping corpus/hook_mapping
```

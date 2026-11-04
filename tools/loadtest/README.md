# acd-loadtest

Load and soak harness for `acd`. Results and method:
`docs/research/2026-11-04-soak-load.md`.

It is a standalone crate with its own empty `[workspace]` table, outside the
root workspace: `cargo test --workspace` never builds it, and the root
`Cargo.lock` is untouched. Its `Cargo.lock` was seeded from the root one.

```sh
cargo build --release -p acd                                # the real daemon (repository root)
cd tools/loadtest && cargo build --release
```

Use a short `--work` path (Unix socket paths are limited to about 100 bytes).
No command prints a token.

## `soak`: drive a daemon, sample it, write a CSV

```sh
./target/release/acd-loadtest soak --acd-bin ../../target/release/acd --work /tmp/lt \
  --duration 7200 --agents 128 --rate 8 --clients 4 \
  --churn-every 60 --restart-every 1800 --burst-every 300 \
  --csv soak.csv --summary soak.txt
```

`--target acd` (default) starts the real `acd`, drives it with external
producers on `hook.sock` (the producer API, at most 8 producers of 32 agents
= 256 concurrent agents) and answers through `acd-client` clients on
`acd.sock`. RSS, fds, threads and CPU come from `/proc/<pid>` every
`--sample` seconds.

`--target host` runs this binary's own `host` mode instead: the daemon's
engine, registry, idempotency cache, event ring and acd/1 server with
built-in synthetic agents (any number, bursts past the 1000-event ring) and
`Core::sizes()` in the CSV (`agents,requests,keys,seen,idem,ring,receivers,tasks`).

| flag | meaning (default) |
|---|---|
| `--agents` | agent slots (128) |
| `--rate` | request opens per second, all agents (8) |
| `--clients` | subscribed `acd-client` clients (4) |
| `--answer-min/--answer-max` | answer delay range, s (0.05/1.5) |
| `--p-answer`, `--p-dup` | designated client answers (0.9); another also answers (0.02) |
| `--withdraw-after` | an unanswered request is withdrawn after s (15) |
| `--churn-every/--churn-frac` | drop this fraction of connections every ~N s (60/0.25) |
| `--restart-every` | kill and restart the process every N s; SIGTERM and SIGKILL alternate (off) |
| `--burst-every` | every free agent opens a request at once, every N s (off) |
| `--turnover` | chance per open that the agent is replaced by a new one (0.02) |

CSV columns: `t_s,pid,rss_kb,hwm_kb,threads,fds,cpu_pct,log_bytes`, rates
(`opens_s` at the source, `seen_s` = opens seen summed over clients,
`answers_ok_s`, `resolved_s`), per-window latency (`vis_*`: request open at the source
to `request.opened` seen by a client; `res_*`: answer submitted to
`request.resolved` seen; `cmd_*`: answer submitted to command result), then
`clients_online,errors,lagged,conn_ends,drops,restarts,welcome_same,welcome_new`
and the sizes columns.

## `limits`: documented limits and what happens beyond them

```sh
./target/release/acd-loadtest limits --acd-bin ../../target/release/acd   # --only daemon|agents
```

## `host`

Internal: started by `soak --target host` and `limits`, which mark the run directory first. `host` refuses any `--dir` that is not a harness-marked directory (real directory, owned by you, not the acd config directory). Without `--dir` it creates a new unique `acd-loadtest-host-<pid>-<nanos>` directory in the temp dir.

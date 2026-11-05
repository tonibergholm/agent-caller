# Relay red-team rig

Reproducers for `docs/research/2026-11-04-red-team-relay.md`. Not run in CI.
The `#[ignore]`d Rust repros live in `crates/acd-relay/tests/redteam.rs`.

**Run this only on a throwaway Linux machine, VM or container.** `up.sh`:

- needs root and GNU `sed`;
- appends `127.0.0.1 fcm.googleapis.com # redteam-relay` to `/etc/hosts`, so
  the machine cannot reach the real FCM until `down.sh` runs;
- binds port 443 for the fake FCM and OAuth endpoint, and raises the open-file
  limit for the relay it starts.

`up.sh` does not add the line if `fcm.googleapis.com` is already in
`/etc/hosts`, and `down.sh` removes only lines carrying the `# redteam-relay`
marker (set `KEEP_HOSTS=1` to keep it). If a run dies, run `down.sh` with the
same work directory, then check `/etc/hosts`.

```sh
cargo build --release -p acd-relay
sudo tests/redteam-relay/up.sh /tmp/rr     # relay + fake FCM
python3 tests/redteam-relay/a_global_drain.py   # one script per attack
sudo tests/redteam-relay/down.sh /tmp/rr
```

The keys, certificates and service-account key it generates are throwaway
test material and stay in the work directory.

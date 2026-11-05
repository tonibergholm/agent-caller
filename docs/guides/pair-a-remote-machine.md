# Pair a remote machine over Tailscale

You end up with a second machine's agents in your inbox. Both machines must
be on the same tailnet. Run `acd` on the remote machine; the app or inbox
runs on the machine you sit at.

## On the remote machine

```sh
cargo install --path crates/acd --locked   # or install a release
acd init
acd service install
acd pair --host <machine>.<tailnet>.ts.net
```

`acd pair` prints one line:

```text
acd://<host>:47800?pin=sha256:<43 characters>#<client token>
```

- The host is what you passed to `--host`. Without `--host`, `acd` asks the
  Tailscale CLI for this machine's name (`tailscale status --json`) and uses
  its DNS name, or its first Tailscale address. If the CLI is missing it
  fails with `Tailscale CLI not found` and you pass `--host`.
- The port is `port` in `config.toml` (default 47800).
- The **pin** is the SHA-256 of this daemon's TLS certificate. The client
  connects over TLS 1.3 and refuses any other certificate.
- The part after `#` is the **client token**. The line is a secret: anyone
  with it can answer your agents. `acd pair` says so on stderr.

`acd` listens only on loopback and its own Tailscale addresses
(`listen = ["loopback", "tailscale"]`). On a remote Mac, allow incoming
connections for `acd` if the firewall asks.

## On your machine

- **Mac app:** copy the line, open settings, click **Add from clipboard**.
- **Omarchy:** paste the line into the panel's settings and press Add.
- **iOS and Android:** see [ios-and-android.md](ios-and-android.md).

The client accepts only a Tailscale or loopback address. Anything else fails
with "That address is not a Tailscale or loopback address. Use the machine's
Tailscale name or address." A malformed line fails with "That is not an acd
pairing line. Run `acd pair` on the machine and copy its output."

The token is stored in the Keychain (Mac, iOS), the Secret Service keyring
(Omarchy) or a Keystore-wrapped file (Android). On iOS the app clears the clipboard once the machine is saved.

## Rotate the token

Do this when you think the line leaked, or on a schedule.

```sh
acd token rotate client     # prints the new token (not a pairing line)
acd pair --host <machine>.<tailnet>.ts.net
```

The daemon re-reads `client.token` every 2 seconds. It closes every client
that used the old token (close code 4401, "token rotated"). Those clients
show **auth failed: The token was rejected. Re-pair this machine.** Remove the
machine in each client and add the new line. Those clients stay disconnected until you re-pair.

## Rotate the certificate (the pin)

```sh
acd tls rotate
```

This replaces the TLS key and certificate. The old pin no longer matches, so
every paired client shows **identity changed: Server identity changed.
Re-pair this machine.** and retries every 30 seconds without connecting.
Run `acd pair` again and re-add the machine. If you rotated because the key
may be compromised, also run `acd token rotate client`.

A changed pin on a machine you did not rotate is a warning sign: do not
re-pair until you know why. See [security-for-users.md](security-for-users.md).

## Rotate the hook token

`acd token rotate hook` is separate. It protects the local Claude hooks, not
paired clients: see [hooks-claude-code-and-codex.md](hooks-claude-code-and-codex.md).

Problems: [troubleshooting.md](troubleshooting.md).

Verified: `acd init`; `acd pair --host mini.tail1234.ts.net` (the line has
the form shown); `acd pair` with no Tailscale CLI (error text above);
`acd token rotate client`, `acd tls rotate` (output text above). Client
messages are copied from `crates/acd-client` source. Not verified: a real
pairing over Tailscale, the 30-second identity retry and the 4401 close
against a running daemon (the sandbox cannot bind IPv6, so `acd run` exits;
the behaviour is from `conn.rs`, `daemon.rs` and `server.rs`, which have
their own tests), `acd pair` host discovery with a real Tailscale CLI.

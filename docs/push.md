# Wake pushes (acd side)

Spec: [relay spec](superpowers/specs/2026-11-03-relay-push-design.md), section 5.
Wire: [PROTOCOL.md §5.1](../crates/acd-protocol/PROTOCOL.md).

Off unless `config.toml` has a `[push]` section that resolves. Off means no
`push` capability, no `push.json`, no outbound connection.

```toml
[push]
relay = "https://relay.example.net"            # https only
ca_file = "/etc/ssl/certs/ca-certificates.crt"  # optional
```

- `relay`: base URL of an `acd-relay`. No userinfo, query or fragment.
- `ca_file`: PEM. The relay's certificate must chain to a certificate in this
  file; nothing else is trusted. Use a CA bundle, or the relay's own
  certificate for a relay without a public one. Without it acd tries
  `/etc/ssl/certs/ca-certificates.crt`, then `/etc/ssl/cert.pem`.
- Any other key, a non-https URL, an unreadable or empty CA file, or
  `relay_pin` (not implemented) turns push off. The daemon still starts.
  `acd status` prints `push: off (reason)`.

Restart acd after changing the section.

## What happens

1. A phone registers over acd/1 (`push.register`) with a handle from the
   relay. acd stores the handle, never a push address, in `push.json` in the
   config directory (mode 0600, written atomically). At most 8 devices.
2. When a request opens, acd sends each registered device one wake:
   `POST <relay>/v1/wake {"v":1,"handle":"..."}`. Nothing about the request,
   agent or machine is in it, and request ids are not used.
3. Per device: at most one wake per 30 s, with one trailing wake for requests
   that open inside the window; at most 4 sends at once; each send has a 5 s
   deadline; redirects are not followed; at most 1 KiB of a response is read.
4. A 5xx, timeout, connection or TLS error, redirect or unexpected answer is
   transient: the device stays enabled, wakes are skipped for
   `10 s * 2^(n-1)` (cap 900 s), the next wake after that goes out. Only a
   `gone` or `invalid_handle` answer disables a device (`expired` or
   `invalid`) until it registers again. A 429 drops that one wake.

## Commands

- `acd push list`: devices and their state, from `push.json`. Never prints
  handles.
- `acd status` shows a `push:` line.
- `acd push --help` repeats the configuration.

A damaged `push.json` (loose mode, bad content) is reported as
`push: degraded (reason)`: acd runs with no registrations and leaves the file
alone until a device registers again.

## Not here

Removing a device from the command line (`acd push remove`) and sending a test
wake (`acd push test`) are not implemented; a device removes itself with
`push.unregister`. The provisional owner decisions (generic notification text,
no per-handle revocation, stolen push address) live only in
`crates/acd-relay/src/provisional.rs`; acd does not touch them.

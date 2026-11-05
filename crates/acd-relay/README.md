# acd-relay

Turns a content-free wake from `acd` into a push (FCM, Android first). Spec:
`docs/superpowers/specs/2026-11-03-relay-push-design.md`. The wake request is
`{"v":1,"handle":"..."}` and nothing else; no request content can reach it.

## Run

```sh
acd-relay --config /etc/acd-relay/relay.toml
```

```toml
listen = "127.0.0.1:8787"       # non-loopback needs allow_non_loopback = true
allowed_apps = ["com.example.agentcaller"]
sealing_keys_file = "/etc/acd-relay/sealing-keys.json"   # mode 0600
# trusted_proxy = "127.0.0.1"   # honour X-Forwarded-For only from this peer

[fcm]
project_id = "my-firebase-project"
credentials_file = "/etc/acd-relay/fcm-service-account.json"   # mode 0600
# ca_file = "/etc/ssl/certs/ca-certificates.crt"
```

Make a sealing key (32 random bytes, base64url) and keep the file at 0600:

```sh
umask 077
printf '{"keys":[{"kid":1,"key":"%s"}]}\n' \
  "$(head -c 32 /dev/urandom | base64 | tr '+/' '-_' | tr -d '=\n')" \
  > /etc/acd-relay/sealing-keys.json
```

To rotate, add a key with a higher `kid`, keep the old one for 30 days, then
delete it (deleting a key revokes its handles). The relay refuses to start on
any config problem (unknown key, loose file mode, empty `allowed_apps`, ...).

## Deployment notes

- The relay speaks **plain HTTP**. Put a TLS-terminating reverse proxy in
  front, or listen on a tailnet address only. Behind a proxy set
  `trusted_proxy` to the proxy's address, or every request shares the
  proxy's rate-limit bucket.
- The proxy should also enforce its own client header/body timeouts and
  connection limits and reach the relay over loopback. As a backstop the
  relay serves at most 1024 connections and closes any connection that does
  not send a complete request head within 10 s (also between requests on a
  keep-alive connection). A push send has one 10 s deadline.
- Rate limits and state are in memory and bounded, and a sweep every 30 s
  drops expired counters and challenges; a restart forgets them and
  keeps every issued handle valid.
- Logs carry route, status, platform and a short tag; never handles,
  addresses, codes, IPs or credentials. Keep the journal retention short.
- Provisional owner decisions (`src/provisional.rs`): generic notification
  text, no per-handle revocation, stolen push address is a documented limit.
- The FCM client is tested only against in-process fakes of the documented
  request and response shapes. Run `validateOnly` against a real project
  before relying on it.

#!/bin/sh
# Start a real release acd-relay against the fake FCM/OAuth endpoint.
# usage: up.sh <workdir> [relay-binary]      env: TRUSTED_PROXY, LISTEN, NOFILE
# Needs root (port 443, /etc/hosts). Local test relay only.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd); W=$1; BIN=${2:-$HERE/../../target/release/acd-relay}
mkdir -p "$W"; cd "$W"
[ -f certs/ca.pem ] || "$HERE/mkcerts.sh" certs
grep -q 'fcm.googleapis.com' /etc/hosts || {
  cp /etc/hosts "$W/hosts.orig"; echo '127.0.0.1 fcm.googleapis.com # redteam-relay' >> /etc/hosts; }
umask 077
printf '{"keys":[{"kid":1,"key":"%s"}]}\n' "$(head -c 32 /dev/urandom | base64 | tr '+/' '-_' | tr -d '=\n')" > keys.json
python3 - "$HERE/../../crates/acd-relay/tests/fixtures/test_rsa_pkcs8.pem" <<'PY' > cred.json
import json,sys
print(json.dumps({"type":"service_account","project_id":"p-1","client_email":"relay@p-1.iam.example",
 "private_key":open(sys.argv[1]).read(),"token_uri":"https://fcm.googleapis.com/token"}))
PY
LISTEN=${LISTEN:-127.0.0.1:8787}
{
 echo "listen = \"$LISTEN\""
 [ "$LISTEN" = "${LISTEN#127.}" ] && echo 'allow_non_loopback = true'
 echo 'allowed_apps = ["com.example.agentcaller"]'
 echo "sealing_keys_file = \"$W/keys.json\""
 [ -n "${TRUSTED_PROXY:-}" ] && echo "trusted_proxy = \"$TRUSTED_PROXY\""
 printf '[fcm]\nproject_id = "p-1"\ncredentials_file = "%s/cred.json"\nca_file = "%s/certs/ca.pem"\n' "$W" "$W"
} > relay.toml
python3 "$HERE/fakefcm.py" --certs "$W/certs" > fcm.log 2>&1 &
echo $! > fcm.pid
sleep 1
( ulimit -n "${NOFILE:-20000}"; RUST_LOG=${RUST_LOG:-info} exec "$BIN" --config relay.toml ) > relay.log 2>&1 &
echo $! > relay.pid
sleep 1
echo "relay pid $(cat relay.pid) fcm pid $(cat fcm.pid) listen $LISTEN"

#!/bin/sh
# Stop what up.sh started and restore /etc/hosts.
W=$1
for p in relay fcm; do [ -f "$W/$p.pid" ] && kill "$(cat "$W/$p.pid")" 2>/dev/null; rm -f "$W/$p.pid"; done
sed -i '/# redteam-relay/d' /etc/hosts

#!/bin/sh
# Throwaway CA + server certificate for the fake FCM/OAuth endpoint.
# usage: mkcerts.sh <outdir>   (writes ca.pem, srv.pem, srv.key)
set -eu
d=$1; mkdir -p "$d"; cd "$d"
openssl req -x509 -newkey rsa:2048 -nodes -keyout ca.key -out ca.pem -days 30 \
  -subj "/CN=redteam-relay-ca" >/dev/null 2>&1
openssl req -newkey rsa:2048 -nodes -keyout srv.key -out srv.csr \
  -subj "/CN=fcm.googleapis.com" >/dev/null 2>&1
printf 'subjectAltName=DNS:fcm.googleapis.com,IP:127.0.0.1\n' > ext.cnf
openssl x509 -req -in srv.csr -CA ca.pem -CAkey ca.key -CAcreateserial -out srv.pem \
  -days 30 -extfile ext.cnf >/dev/null 2>&1

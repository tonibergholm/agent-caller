#!/bin/sh
# Imports a Developer ID Application certificate into a temporary keychain for one
# CI job. release.yml runs it only when the APPLE_CERTIFICATE secret is set.
#
# Reads: APPLE_CERTIFICATE (base64 .p12), APPLE_CERTIFICATE_PASSWORD,
#        APPLE_SIGNING_IDENTITY (optional; defaults to the first Developer ID
#        Application identity in the certificate).
# Writes to $GITHUB_OUTPUT: identity and keychain (read them as steps.<id>.outputs).
# Never prints the certificate or its password.
set -eu

if [ "$(uname -s)" != "Darwin" ]; then
	echo "apple-keychain.sh runs on macOS only" >&2
	exit 1
fi
: "${APPLE_CERTIFICATE:?APPLE_CERTIFICATE is not set}"
: "${APPLE_CERTIFICATE_PASSWORD:?APPLE_CERTIFICATE_PASSWORD is not set}"

tmp=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/acd-signing.XXXXXX")
keychain="$tmp/signing.keychain-db"
keychain_password=$(openssl rand -hex 24)

printf '%s' "$APPLE_CERTIFICATE" | base64 --decode >"$tmp/certificate.p12"
security create-keychain -p "$keychain_password" "$keychain"
security set-keychain-settings -lut 21600 "$keychain"
security unlock-keychain -p "$keychain_password" "$keychain"
security import "$tmp/certificate.p12" -k "$keychain" \
	-P "$APPLE_CERTIFICATE_PASSWORD" -T /usr/bin/codesign -T /usr/bin/security >/dev/null
rm -f "$tmp/certificate.p12"
security set-key-partition-list -S apple-tool:,apple: -s \
	-k "$keychain_password" "$keychain" >/dev/null

# Search the temporary keychain first and keep the existing user keychains after it.
# shellcheck disable=SC2046
security list-keychains -d user -s "$keychain" $(security list-keychains -d user | tr -d ' "')

identity="${APPLE_SIGNING_IDENTITY:-}"
if [ -z "$identity" ]; then
	identity=$(security find-identity -v -p codesigning "$keychain" |
		sed -n 's/.*"\(Developer ID Application:[^"]*\)".*/\1/p' | head -n 1)
fi
if [ -z "$identity" ]; then
	echo "The certificate has no Developer ID Application identity." >&2
	exit 1
fi

# Step outputs reach only the steps that name them (steps.<id>.outputs.*). Writing
# to GITHUB_ENV would expose the values to every later step, including uploads.
echo "::add-mask::$identity"
if [ -n "${GITHUB_OUTPUT:-}" ]; then
	printf 'identity=%s\nkeychain=%s\n' "$identity" "$keychain" >>"$GITHUB_OUTPUT"
else
	echo "apple-keychain.sh: GITHUB_OUTPUT is not set; run it inside a workflow step" >&2
	exit 1
fi
echo "Imported Developer ID identity into a temporary keychain."

# Live end-to-end harness (spec §13).  See tests/e2e/README.md.
# Everything runs under /tmp/ace2e with its own acd, herdr session and T3 server.

e2e_dir := "tests/e2e"

# Build, up, every scenario, down (trap-protected; non-zero on any FAIL, interrupt or failed teardown).  See tests/e2e/run-all.sh.
e2e: e2e-build
    {{e2e_dir}}/run-all.sh

e2e-build:
    cargo build --release -p acd --locked

e2e-up:
    {{e2e_dir}}/up.sh

e2e-down:
    {{e2e_dir}}/down.sh

# One scenario: just e2e-one 30-codex-live  (needs e2e-up; Codex scenarios need `codex-trust.sh add`)
e2e-one name:
    {{e2e_dir}}/scenarios/{{name}}.sh

# Inspect the live inbox of the scratch acd
e2e-inbox:
    python3 {{e2e_dir}}/inbox.py -d /tmp/ace2e/acd list

# Build the Mac app and install it to /Applications. Signs with
# $APPLE_SIGNING_IDENTITY when set (a stable identity keeps Keychain and
# privacy grants across rebuilds); otherwise the build is ad-hoc signed.
app-install:
    cd app && npm ci --silent && npx tauri build --bundles app --ci
    -pkill -f "Agent Caller.app/Contents/MacOS/agent-caller-app"
    sleep 1
    rm -rf "/Applications/Agent Caller.app"
    ditto "app/src-tauri/target/release/bundle/macos/Agent Caller.app" "/Applications/Agent Caller.app"
    codesign -dvv "/Applications/Agent Caller.app" 2>&1 | grep -E "^(Authority|Signature)=" | head -1
    open "/Applications/Agent Caller.app"

# Build acd-inbox for Omarchy (x86_64 Linux) in Docker, as acd is built
# (docs/research/2026-10-11-e2e-run.md). Output:
# target/linux-amd64/release/acd-inbox. The Linux target dir is separate
# from the Mac's.
inbox-linux:
    docker run --rm --platform linux/amd64 -v "$PWD":/src -w /src -v acd-inbox-cargo:/usr/local/cargo/registry -e CARGO_TARGET_DIR=/src/target/linux-amd64 rust:1-bookworm cargo build --release --locked -p acd-inbox

# Copy the Linux build to a host and run its installer there. The installer
# asks before it edits the bindings file only when it has a terminal; use
# `ssh -t` by hand for that.
inbox-install host="toni@omarchy": inbox-linux
    scp target/linux-amd64/release/acd-inbox {{host}}:/tmp/acd-inbox
    ssh {{host}} /tmp/acd-inbox install

# Check that every version field, the crates and CHANGELOG agree (docs/release.md).
check-version:
    scripts/check-version.sh

# Runs the release script tests (scripts/tests; also run in CI on ubuntu).
test-scripts:
    scripts/tests/run.sh

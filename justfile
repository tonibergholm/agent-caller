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

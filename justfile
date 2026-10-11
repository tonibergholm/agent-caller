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

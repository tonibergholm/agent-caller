# Live end-to-end harness (spec §13).  See tests/e2e/README.md.
# Everything runs under /tmp/ace2e with its own acd, herdr session and T3 server.

e2e_dir := "tests/e2e"

# Build, bring the scratch environment up, run every scenario, tear down.
e2e: e2e-build e2e-up
    -{{e2e_dir}}/scenarios/10-hooks-roundtrip.sh
    -{{e2e_dir}}/scenarios/20-synthetic-daemon.sh
    {{e2e_dir}}/codex-trust.sh add
    -{{e2e_dir}}/scenarios/30-codex-live.sh
    -{{e2e_dir}}/scenarios/31-codex-resilience.sh
    -{{e2e_dir}}/scenarios/40-claude-live.sh
    -{{e2e_dir}}/scenarios/41-claude-resilience.sh
    -{{e2e_dir}}/scenarios/50-t3.sh
    @just e2e-down
    @grep -hE '^(PASS|FAIL)' /tmp/ace2e/results/*.txt; true

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

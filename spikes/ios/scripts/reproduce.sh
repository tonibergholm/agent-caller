#!/usr/bin/env bash
# THROWAWAY SPIKE (2026-10-12). Records the commands behind
# docs/research/2026-10-12-ios-spike.md. Not a unit: the steps are ordered,
# some need manual input, and the simulator must be booted first.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/../../.." && pwd)"
export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
TAURI="$REPO/app/node_modules/.bin/tauri"
WORK=/tmp/ios-spike
mkdir -p "$WORK"

# 0. Toolchain (once):
#    brew install rustup; rustup default stable --profile minimal
#    rustup target add aarch64-apple-ios aarch64-apple-ios-sim
#    xcodebuild -downloadPlatform iOS ; brew install xcodegen

# 1. acd-client on iOS.
(cd "$REPO" && cargo check -p acd-client --target aarch64-apple-ios --locked)
(cd "$REPO" && cargo check -p acd-client -p acd-securefs -p acd-protocol --target aarch64-apple-ios-sim --locked)

# 2. Scratch daemon on loopback, separate config dir. Replace config.toml
#    with loopback-only ports 47810/47811, herdr and T3 off (see the doc).
export ACD_CONFIG_DIR="$WORK/acd-config"
mkdir -p "$ACD_CONFIG_DIR" && chmod 700 "$ACD_CONFIG_DIR"
(cd "$REPO" && cargo build -p acd --locked)
"$REPO/target/debug/acd" init
# Loopback only, scratch ports, herdr and T3 off. Overwrites the init config.
printf '%s\n' 'machine_name = "ios-spike-scratch"' 'port = 47810' 'hook_port = 47811' \
  'listen = ["loopback"]' 'allowed_origins = []' 'ignored_entrypoints = ["sdk-ts"]' '' \
  '[herdr]' 'enabled = false' 'extra_sockets = []' '' '[t3]' 'enabled = false' '' \
  '[claude]' 'permission_hold = 3600' > "$ACD_CONFIG_DIR/config.toml"
chmod 600 "$ACD_CONFIG_DIR/config.toml"
"$REPO/target/debug/acd" run > "$WORK/acd-run.log" 2>&1 &
"$REPO/target/debug/acd" pair --host 127.0.0.1 > "$WORK/pairing.line"
chmod 600 "$WORK/pairing.line"

# 3. Probe on the simulator. The pairing file is passed by env. The line is
#    never printed. Add SIMCTL_CHILD_ACD_PROBE_BAD_PIN=1 for the refusal case.
(cd "$REPO/spikes/ios/probe" && cargo build --target aarch64-apple-ios-sim)
SIMCTL_CHILD_ACD_PAIRING_FILE="$WORK/pairing.line" \
  xcrun simctl spawn booted "$REPO/spikes/ios/probe/target/aarch64-apple-ios-sim/debug/acd-ios-probe"

# 4. Tauri iOS shell. After `ios init`, link gen/apple/tauri to the CLI
#    (see the doc, the generated Xcode script runs `node tauri`).
(cd "$REPO/spikes/ios/tauri-shell" && "$TAURI" ios init --ci)
(cd "$REPO/spikes/ios/tauri-shell" && "$TAURI" ios build --debug --target aarch64-sim --ci)
xcrun simctl install booted "$REPO/spikes/ios/tauri-shell/src-tauri/gen/apple/build/arm64-sim/Agent Caller Spike.app"
SIMCTL_CHILD_ACD_SPIKE_PAIRING_FILE="$WORK/pairing.line" \
  xcrun simctl launch booted dev.agentcaller.inbox.iosspike
xcrun simctl io booted screenshot "$WORK/shot.png"

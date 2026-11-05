#!/bin/sh
# Builds an installable DEBUG APK of the Android inbox from the pieces in this
# directory, without Gradle or the Tauri CLI (neither could be installed in the
# sandbox this was written in; see docs/android.md).
#
#   cargo (Rust, 2 ABIs) + javac + dx|d8 + aapt + zipalign + apksigner
#
# Output: build/agent-caller-debug.apk, signed with a throwaway debug key made
# in build/ (gitignored). No release signing here, and no key is ever stored in
# the repository.
#
# Native toolchain, first match wins:
#   ANDROID_NDK_HOME   a real NDK (r26 or later). UNTESTED here: the sandbox has
#                      no NDK. Uses the NDK's clang as compiler and linker.
#   ACD_SANDBOX_NDK    the directory tools/sandbox-ndk.sh wrote. Stand-in link
#                      stubs for a sandbox without an NDK. The result links but
#                      has never been loaded on a device or emulator.
# Other inputs (environment): ANDROID_JAR (a platform android.jar, API 23 or
# later; default: the Debian package android-sdk-platform-23 or
# $ANDROID_HOME/platforms/android-*), ABIS (default "arm64-v8a x86_64").
set -eu
here=$(cd "$(dirname "$0")" && pwd)
cd "$here"
api=23
abis=${ABIS:-"arm64-v8a x86_64"}
build=$here/build
stage=$build/stage

need() { command -v "$1" >/dev/null 2>&1 || { echo "missing tool: $1 ($2)" >&2; exit 1; }; }
need cargo "rustup"; need javac "a JDK"; need aapt "apt install aapt"
need zipalign "apt install zipalign"; need apksigner "apt install apksigner"; need keytool "a JDK"

# ---- platform jar ----
jar=${ANDROID_JAR:-}
if [ -z "$jar" ]; then
  for c in /usr/lib/android-sdk/platforms/android-23/android.jar \
           "${ANDROID_HOME:-/nonexistent}"/platforms/android-*/android.jar; do
    [ -f "$c" ] && jar=$c && break
  done
fi
[ -f "$jar" ] || { echo "no android.jar: set ANDROID_JAR" >&2; exit 1; }

# ---- native toolchain ----
rust_target() { case $1 in arm64-v8a) echo aarch64-linux-android ;; x86_64) echo x86_64-linux-android ;; *) echo "unknown ABI $1" >&2; exit 1 ;; esac; }
if [ -n "${ANDROID_NDK_HOME:-}" ]; then
  case $(uname -s) in Darwin) tag=darwin-x86_64 ;; *) tag=linux-x86_64 ;; esac
  bin=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/$tag/bin
  [ -x "$bin/aarch64-linux-android$api-clang" ] || { echo "not an NDK: $ANDROID_NDK_HOME" >&2; exit 1; }
  for abi in $abis; do
    t=$(rust_target "$abi"); u=$(echo "$t" | tr 'a-z-' 'A-Z_'); l=$(echo "$t" | tr - _)
    export "CC_$l=$bin/${t%%-*}-linux-android$api-clang"
    export "AR_$l=$bin/llvm-ar"
    export "CARGO_TARGET_${u}_LINKER=$bin/${t%%-*}-linux-android$api-clang"
  done
  native="NDK $ANDROID_NDK_HOME"
elif [ -n "${ACD_SANDBOX_NDK:-}" ]; then
  . "$ACD_SANDBOX_NDK/env.sh"
  native="sandbox stand-in sysroot (not an NDK)"
else
  echo "set ANDROID_NDK_HOME (a real NDK) or ACD_SANDBOX_NDK (tools/sandbox-ndk.sh output)" >&2
  exit 1
fi

rm -rf "$stage" "$build/classes"
mkdir -p "$stage" "$build/classes"

# ---- native libraries ----
for abi in $abis; do
  t=$(rust_target "$abi")
  cargo build --release --locked --lib --target "$t"
  mkdir -p "$stage/lib/$abi"
  cp "target/$t/release/libagent_caller_android.so" "$stage/lib/$abi/"
done

# ---- Java -> dex ----
javac -source 8 -target 8 -Xlint:-options -bootclasspath "$jar" -d "$build/classes" \
  $(find java -name '*.java')
if command -v d8 >/dev/null 2>&1; then
  d8 --min-api $api --lib "$jar" --output "$stage" $(find "$build/classes" -name '*.class')
else
  need dalvik-exchange "apt install dalvik-exchange, or put d8 on PATH"
  dalvik-exchange --dex --min-sdk-version=$api --output="$stage/classes.dex" "$build/classes"
fi

# ---- package, align, sign ----
rm -f "$build/unsigned.apk" "$build/aligned.apk" "$build/agent-caller-debug.apk"
# The version name is the crate version (kept in step with the workspace by
# scripts/check-version.sh). The code is major*10000 + minor*100 + patch.
ver=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
code=$(echo "$ver" | awk -F'[.-]' '{ print $1 * 10000 + $2 * 100 + $3 }')
aapt package -f -M AndroidManifest.xml --version-name "$ver" --version-code "$code" \
  -I "$jar" -F "$build/unsigned.apk"
(cd "$stage" && aapt add "$build/unsigned.apk" classes.dex $(find lib -name '*.so') >/dev/null)
zipalign -f 4 "$build/unsigned.apk" "$build/aligned.apk"
ks=$build/debug.keystore
if [ ! -f "$ks" ]; then
  keytool -genkeypair -keystore "$ks" -storepass android -keypass android \
    -alias androiddebugkey -keyalg RSA -keysize 2048 -validity 10000 \
    -dname "CN=Android Debug,O=Android,C=US" >/dev/null 2>&1
fi
apksigner sign --ks "$ks" --ks-pass pass:android --key-pass pass:android \
  --ks-key-alias androiddebugkey --min-sdk-version $api \
  --out "$build/agent-caller-debug.apk" "$build/aligned.apk"
rm -f "$build/agent-caller-debug.apk.idsig"
echo "built $build/agent-caller-debug.apk"
echo "native toolchain: $native"

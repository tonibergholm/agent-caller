#!/usr/bin/env bash
# Type-check the portable client crates for Windows and Android (CLAUDE.md,
# "Platform limits"). `cargo check` never links, so no NDK, MSVC or Windows SDK
# is needed. The one thing a check still runs is ring's build script, which
# compiles C and asm. The stub below stands in for that C compiler and archiver
# and just writes empty output files, so nothing is compiled for the target:
# only Rust type checking is covered. CI runs the same script.
set -euo pipefail
cd "$(dirname "$0")/.."

# Crates that type-check for both targets (docs/research/2026-11-03-windows-android-feasibility.md,
# PR #79). Not listed: acd-adapters, acd-inbox and acd use unix sockets and
# unix file modes and do not build for either target.
crates=(acd-protocol acd-securefs acd-core acd-client acd-app-core acd-voice acd-voice-client)
targets=(x86_64-pc-windows-msvc aarch64-linux-android)

stub=$(mktemp -d)
trap 'rm -rf "$stub"' EXIT
cat >"$stub/cc" <<'STUB'
#!/usr/bin/env bash
# Writes every output path the cc crate asks for: `-o X`, `-FoX`, `-out:X`, `*.a`, `*.lib`.
prev=
for a in "$@"; do
  out=
  case "$prev" in -o) out=$a ;; esac
  case "$a" in
    -Fo?*|/Fo?*) out=${a:3} ;;
    -out:?*|-OUT:?*|/out:?*|/OUT:?*) out=${a#*:} ;;
    *.a|*.lib) out=$a ;;
  esac
  if [ -n "$out" ]; then mkdir -p "$(dirname "$out")" && : >"$out"; fi
  prev=$a
done
exit 0
STUB
chmod +x "$stub/cc"

export CC_x86_64_pc_windows_msvc="$stub/cc" AR_x86_64_pc_windows_msvc="$stub/cc"
export CC_aarch64_linux_android="$stub/cc" AR_aarch64_linux_android="$stub/cc"

pkgs=()
for c in "${crates[@]}"; do pkgs+=(-p "$c"); done
for t in "${targets[@]}"; do
  rustup target add "$t" >/dev/null 2>&1 || true
  echo "== cargo check $t"
  cargo check --locked "${pkgs[@]}" --target "$t"
done

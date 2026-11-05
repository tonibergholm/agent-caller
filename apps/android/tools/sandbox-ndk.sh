#!/bin/sh
# Builds a stand-in for the NDK link environment in a sandbox that cannot
# download the NDK (dl.google.com is blocked there). It is NOT an NDK:
#
#   - libc.so, libm.so, libdl.so: empty-bodied stubs that export the symbols
#     listed in bionic-symbols.txt, so lld records them as NEEDED and leaves
#     the symbols undefined. Android's dynamic linker resolves them against the
#     real bionic at load time. Nothing is resolved against glibc.
#   - libunwind.a: LLVM libunwind from the rust-src component, built with the
#     host clang for the Android target. Rust's Android std links `-lunwind`.
#   - crt_so.o: defines the hidden __dso_handle that std refers to, as the
#     NDK's crtbegin_so.o does.
#   - C headers for ring's C code come from the host's glibc (arm64 cross
#     headers for aarch64). Only declarations are used; the one glibc-only
#     symbol libunwind would import (__errno_location) is renamed to bionic's
#     __errno on the command line.
#
# Usage: sandbox-ndk.sh OUTDIR   then   . OUTDIR/env.sh
# Needs: clang, clang++, lld, llvm-ar, readelf, the rustup components
# rust-src and rust-std for the two Android targets, and for aarch64 the
# package libc6-dev-arm64-cross.
set -eu
out=${1:?usage: sandbox-ndk.sh OUTDIR}
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$out"
out=$(cd "$out" && pwd)
api=23
unw=$(rustc --print sysroot)/lib/rustlib/src/rust/src/llvm-project/libunwind
[ -d "$unw/src" ] || { echo "rust-src is missing: rustup component add rust-src" >&2; exit 1; }

: > "$out/env.sh"
for arch in aarch64 x86_64; do
  triple=$arch-linux-android$api
  d=$out/$arch
  mkdir -p "$d/obj"
  case $arch in
    aarch64) inc="-isystem /usr/aarch64-linux-gnu/include" ;;
    x86_64) inc="-isystem /usr/include/x86_64-linux-gnu -isystem /usr/include" ;;
  esac
  common="--target=$triple $inc -O2 -fPIC -Wno-unused-command-line-argument"
  # libunwind
  ucommon="$common -I$unw/include -fvisibility=hidden -funwind-tables -D_LIBUNWIND_IS_NATIVE_ONLY -DNDEBUG -D__errno_location=__errno"
  clang++ $ucommon -std=c++11 -fno-exceptions -fno-rtti -nostdinc++ -c "$unw/src/libunwind.cpp" -o "$d/obj/libunwind.o"
  for f in UnwindLevel1.c UnwindLevel1-gcc-ext.c Unwind-sjlj.c; do
    clang $ucommon -std=c11 -c "$unw/src/$f" -o "$d/obj/${f%.c}.o"
  done
  for f in UnwindRegistersRestore.S UnwindRegistersSave.S; do
    clang $ucommon -c "$unw/src/$f" -o "$d/obj/${f%.S}.o"
  done
  rm -f "$d/libunwind.a"
  llvm-ar rcs "$d/libunwind.a" "$d"/obj/*.o
  # __dso_handle
  printf '__attribute__((visibility("hidden"))) void *__dso_handle = &__dso_handle;\n' > "$d/crt_so.c"
  clang --target=$triple -c -O2 -fPIC "$d/crt_so.c" -o "$d/crt_so.o"
  # stub libraries
  for lib in libc libdl libm liblog; do
    : > "$d/$lib.c"
    grep "^$lib " "$here/bionic-symbols.txt" | while read -r _ a b; do
      if [ "$a" = data ]; then
        echo "void *$b;" >> "$d/$lib.c"
      else
        echo "void $a(void) {}" >> "$d/$lib.c"
      fi
    done
    clang --target=$triple -fuse-ld=lld -nostdlib -shared -fPIC -fno-builtin -w -Wl,-soname,$lib.so \
      "$d/$lib.c" -o "$d/$lib.so"
  done
  # linker wrapper: no NDK sysroot, no start files, explicit stubs
  cat > "$d/cc.sh" <<EOF
#!/bin/sh
exec clang --target=$triple -fuse-ld=lld -nostartfiles -nostdlib -L$d "\$@" $d/crt_so.o -Wl,-z,max-page-size=16384
EOF
  chmod +x "$d/cc.sh"
  var=$(echo "$arch" | tr a-z A-Z)
  cat >> "$out/env.sh" <<EOF
export CC_${arch}_linux_android="clang --target=$triple $inc"
export AR_${arch}_linux_android=llvm-ar
export CARGO_TARGET_${var}_LINUX_ANDROID_LINKER=$d/cc.sh
EOF
done
echo "stand-in NDK environment written to $out/env.sh"

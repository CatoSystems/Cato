#!/usr/bin/env bash
# Build a static LLVM 23.1.x + lld toolchain into vendor/llvm-prefix.
#
# Usage:   tools/build_toolchain.sh
# Env:     LLVM_SRC  path to an llvm-project source tree (default: ~/clones/llvm-project)
# Output:  vendor/llvm-prefix/ containing bin/llvm-config (reports 23.1.x),
#          bin/lld, include/, lib/libLLVM*.a and
#          lib/lld/liblld{ELF,COFF,MachO,MinGW,Common}.a (layout may be
#          lib/liblld*.a instead of lib/lld/ — both are checked later).
# Re-run:  exits 0 immediately once the prefix is already built and valid.
#
# llvm-sys 231 (strict-versioning) requires exactly this major.minor; the
# script refuses any other source version.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PREFIX="$ROOT/vendor/llvm-prefix"
BUILD="$ROOT/vendor/llvm-prefix-build"
SRC="${LLVM_SRC:-$HOME/clones/llvm-project}"

if [[ ! -f "$SRC/llvm/CMakeLists.txt" ]]; then
  echo "build_toolchain: LLVM source not found at $SRC (set LLVM_SRC to an llvm-project checkout)" >&2
  exit 1
fi

VER_MAJOR="$(sed -n 's/^ *set(LLVM_VERSION_MAJOR \([0-9]*\)).*/\1/p' "$SRC/cmake/Modules/LLVMVersion.cmake" | head -1)"
VER_MINOR="$(sed -n 's/^ *set(LLVM_VERSION_MINOR \([0-9]*\)).*/\1/p' "$SRC/cmake/Modules/LLVMVersion.cmake" | head -1)"
if [[ "$VER_MAJOR" != "23" || "$VER_MINOR" != "1" ]]; then
  echo "build_toolchain: $SRC is LLVM $VER_MAJOR.$VER_MINOR; llvm-sys 231 needs a 23.1.x source tree (check out release/23.x)" >&2
  exit 1
fi

if [[ -x "$PREFIX/bin/llvm-config" ]] \
   && [[ "$("$PREFIX/bin/llvm-config" --version)" == 23.1.* ]] \
   && [[ -x "$PREFIX/bin/lld" ]]; then
  echo "build_toolchain: $PREFIX already provides LLVM $($PREFIX/bin/llvm-config --version) — nothing to do"
  exit 0
fi

JOBS="$(sysctl -n hw.ncpu 2>/dev/null || nproc)"

cmake -S "$SRC/llvm" -B "$BUILD" \
  -DCMAKE_INSTALL_PREFIX="$PREFIX" \
  -DCMAKE_BUILD_TYPE=Release \
  -DLLVM_ENABLE_PROJECTS=lld \
  -DLLVM_ENABLE_TERMINFO=OFF \
  -DLLVM_ENABLE_ZLIB=OFF \
  -DLLVM_ENABLE_ZSTD=OFF \
  -DLLVM_TARGETS_TO_BUILD="X86;AArch64" \
  -DBUILD_SHARED_LIBS=OFF

cmake --build "$BUILD" --parallel "$JOBS"
cmake --install "$BUILD"

VERSION="$("$PREFIX/bin/llvm-config" --version)"
if [[ "$VERSION" != 23.1.* ]]; then
  echo "build_toolchain: installed llvm-config reports $VERSION, expected 23.1.*" >&2
  exit 1
fi
[[ -x "$PREFIX/bin/lld" ]] || { echo "build_toolchain: $PREFIX/bin/lld missing after install" >&2; exit 1; }
LLD_LIB=""
for p in "$PREFIX/lib/lld/liblldELF.a" "$PREFIX/lib/liblldELF.a"; do
  [[ -f "$p" ]] && LLD_LIB="$p" && break
done
[[ -n "$LLD_LIB" ]] || { echo "build_toolchain: liblldELF.a missing from $PREFIX/lib{,/lld}" >&2; exit 1; }

echo "build_toolchain: OK"
echo "  llvm-config: $VERSION"
echo "  lld:         $PREFIX/bin/lld ($("$PREFIX/bin/ld.lld" --version))"
echo "  lld libs:    $(dirname "$LLD_LIB")"

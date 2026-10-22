#!/usr/bin/env bash
# Build a static LLVM 23.1.x + lld toolchain into vendor/llvm-prefix.
#
# Usage:   tools/build_toolchain.sh
# Env:     LLVM_SRC  path to an llvm-project source tree (default: ~/clones/llvm-project)
# Output:  vendor/llvm-prefix/ containing bin/llvm-config (reports 23.1.x),
#          bin/lld, include/, and lib/ with LLVM's component libraries
#          (libLLVM*.a on Unix, LLVM*.lib on MSVC) next to lld's own
#          (liblld*.a on Unix, lld*.lib on MSVC).
# Re-run:  exits 0 immediately once the prefix is already built and valid.
#
# The toolchain's configuration lives in tools/toolchain-config.cmake, so
# that CI can key its toolchain cache on the configuration rather than on
# this script.
#
# llvm-sys 231 (strict-versioning) requires exactly this major.minor; the
# script refuses any other source version.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PREFIX="$ROOT/vendor/llvm-prefix"
BUILD="$ROOT/vendor/llvm-prefix-build"
SRC="${LLVM_SRC:-$HOME/clones/llvm-project}"

# The toolchain is always built for the host. Windows uses the MSVC
# toolchain and keeps the .exe suffix; Unix keeps the extensionless names.
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*|Windows_NT) IS_WINDOWS=1 ;;
  *) IS_WINDOWS=0 ;;
esac
EXE=""
if [[ $IS_WINDOWS -eq 1 ]]; then
  EXE=".exe"
fi

die() { echo "build_toolchain: $*" >&2; exit 1; }

# Echo the first path that exists.
first_existing() {
  local p
  for p in "$@"; do
    if [[ -f "$p" ]]; then
      printf '%s' "$p"
      return 0
    fi
  done
  return 1
}

# A native Windows program needs native paths. Git Bash can translate them.
native_path() {
  if [[ $IS_WINDOWS -eq 1 ]] && command -v cygpath >/dev/null 2>&1; then
    cygpath -w "$1"
  else
    printf '%s' "$1"
  fi
}

LLVM_CONFIG="$PREFIX/bin/llvm-config$EXE"
LLD_LIB="$(first_existing "$PREFIX/lib/liblldELF.a" "$PREFIX/lib/lld/liblldELF.a" \
                        "$PREFIX/lib/lldELF.lib" "$PREFIX/lib/lld/lldELF.lib" || true)"
LLVM_LIB="$(first_existing "$PREFIX/lib/libLLVMCore.a" "$PREFIX/lib/LLVMCore.lib" || true)"
if [[ -x "$LLVM_CONFIG" ]] \
   && [[ "$("$LLVM_CONFIG" --version)" == 23.1.* ]] \
   && [[ -x "$PREFIX/bin/lld$EXE" ]] \
   && [[ -n "$LLD_LIB" && -n "$LLVM_LIB" ]]; then
  echo "build_toolchain: $PREFIX already provides LLVM $("$LLVM_CONFIG" --version) — nothing to do"
  exit 0
fi

# The source is only needed to build the toolchain, so the check for a
# usable prefix comes first: a cached prefix makes the source unnecessary.
if [[ ! -f "$SRC/llvm/CMakeLists.txt" ]]; then
  die "LLVM source not found at $SRC (set LLVM_SRC to an llvm-project checkout)"
fi

VER_MAJOR="$(sed -n 's/^ *set(LLVM_VERSION_MAJOR \([0-9]*\)).*/\1/p' "$SRC/cmake/Modules/LLVMVersion.cmake" | head -1)"
VER_MINOR="$(sed -n 's/^ *set(LLVM_VERSION_MINOR \([0-9]*\)).*/\1/p' "$SRC/cmake/Modules/LLVMVersion.cmake" | head -1)"
if [[ "$VER_MAJOR" != "23" || "$VER_MINOR" != "1" ]]; then
  die "$SRC is LLVM $VER_MAJOR.$VER_MINOR; llvm-sys 231 needs a 23.1.x source tree (check out release/23.x)"
fi

JOBS="$(sysctl -n hw.ncpu 2>/dev/null || nproc)"

# Ninja builds LLVM noticeably faster than the Visual Studio generator, but
# it is not installed on every machine: use it when it is there.
GENERATOR=""
if [[ $IS_WINDOWS -eq 1 ]] && command -v ninja >/dev/null 2>&1; then
  GENERATOR="-G Ninja"
fi

cmake -S "$(native_path "$SRC/llvm")" -B "$(native_path "$BUILD")" \
  $GENERATOR \
  -C "$(native_path "$ROOT/tools/toolchain-config.cmake")" \
  -DCMAKE_INSTALL_PREFIX="$(native_path "$PREFIX")" \
  -DCMAKE_BUILD_TYPE=Release

# The configuration is spelled out on both sides: the Visual Studio
# generator builds every configuration by default and installs the one
# named by --config, which mismatches when the two disagree.
cmake --build "$BUILD" --config Release --parallel "$JOBS"
cmake --install "$BUILD" --config Release

VERSION="$("$LLVM_CONFIG" --version)"
if [[ "$VERSION" != 23.1.* ]]; then
  die "installed llvm-config reports $VERSION, expected 23.1.*"
fi
[[ -x "$PREFIX/bin/lld$EXE" ]] || die "$PREFIX/bin/lld$EXE missing after install"
LLD_LIB="$(first_existing "$PREFIX/lib/liblldELF.a" "$PREFIX/lib/lld/liblldELF.a" \
                        "$PREFIX/lib/lldELF.lib" "$PREFIX/lib/lld/lldELF.lib" || true)"
[[ -n "$LLD_LIB" ]] || die "lld's ELF driver library missing from $PREFIX/lib"
LLVM_LIB="$(first_existing "$PREFIX/lib/libLLVMCore.a" "$PREFIX/lib/LLVMCore.lib" || true)"
[[ -n "$LLVM_LIB" ]] || die "LLVM's component libraries missing from $PREFIX/lib"

echo "build_toolchain: OK"
echo "  llvm-config: $VERSION"
echo "  lld:         $("$PREFIX/bin/ld.lld$EXE" --version)"
echo "  libs:        $(dirname "$LLD_LIB")"

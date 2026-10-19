#!/usr/bin/env bash
# Generate and validate cato's embedded per-target link assets.
#
# Usage:
#   tools/gen_assets.sh                # generate all targets runnable on this
#                                      # host, then validate
#   tools/gen_assets.sh --validate     # validate existing asset dirs only
#                                      # (required-file checks always; full lld
#                                      # validation link per target when its
#                                      # compiler — zig / xcrun — is available)
#   tools/gen_assets.sh --refresh-lld  # copy vendor/llvm-prefix/bin/lld into
#                                      # assets/lld/<host>/ (no zig needed)
#
# Requirements (full generation): zig 0.17+ on PATH (linux/windows targets),
# xcrun + macOS SDK (macos targets, macOS hosts only),
# vendor/llvm-prefix built by tools/build_toolchain.sh (validation links + lld copy).
#
# Generated, gitignored: assets/x86_64-macos/, assets/aarch64-macos/,
# assets/lld/<host>/. Committed: assets/x86_64-linux/, assets/aarch64-linux/,
# assets/x86_64-windows/.
#
# ---------------------------------------------------------------------------
# lld argv deviations discovered against plan shapes (Task 6 hermetic_link_argv
# unit tests must pin these same shapes):
#   - linux aarch64: NO -m flag. lld 23.1 has no elf_aarch64/elf_aarch64le
#     emulation name; the ELF driver auto-detects the machine from the input
#     objects. -m elf_x86_64 is kept for x86_64 (still registered).
#   - linux both: extra -lzigc required between -lc and -lcompiler_rt. zig
#     0.17 puts strchr/malloc/strcmp-style routines in libzigc.a, not musl's
#     libc.a. crti.o/crtn.o are EMPTY placeholder objects (zig's musl CRT
#     needs none; lld accepts empty .init/.fini framing objects).
#   - windows: the mingw driver adds NO default CRT or libraries (defaults
#     come from GCC's specs, not lld). Every input must be an explicit staged
#     path. zig 0.17 is UCRT-based: no libmingwex.a / libmsvcrt.a exist;
#     libzigc.a + 15x api-ms-win-crt-*.lib + system import libs replace them.
#     Renames at staging: crt2.obj→crt2.o, libmingw32.lib→libmingw32.a,
#     zigc.lib→libzigc.a, compiler_rt.lib→libcompiler_rt.a.
#     ubsan_rt runtimes are never staged (no sanitized compilations).
#   - macos: plan shape works as-is; ld64.lld warns benignly about object
#     version 26 > -platform_version minimum 11.0.
# ---------------------------------------------------------------------------
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ASSETS="$ROOT/crates/cato-assets/assets"
LLD="$ROOT/vendor/llvm-prefix/bin/ld.lld"
LLD64="$ROOT/vendor/llvm-prefix/bin/ld64.lld"
LLD_BIN="$ROOT/vendor/llvm-prefix/bin/lld"

MODE=generate
case "${1:-}" in
  "") ;;
  --validate) MODE=validate ;;
  --refresh-lld) MODE=refresh-lld ;;
  *) echo "gen_assets: unknown option $1 (see header usage)" >&2; exit 2 ;;
esac

die() { echo "gen_assets: $*" >&2; exit 1; }

host_target_name() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os" in
    Darwin)
      case "$arch" in
        arm64|aarch64) echo aarch64-macos ;;
        x86_64) echo x86_64-macos ;;
        *) die "unsupported macOS arch $arch" ;;
      esac ;;
    Linux)
      case "$arch" in
        x86_64) echo x86_64-linux ;;
        aarch64|arm64) echo aarch64-linux ;;
        *) die "unsupported Linux arch $arch" ;;
      esac ;;
    MINGW*|MSYS*|CYGWIN*|Windows_NT)
      case "$arch" in
        x86_64|AMD64) echo x86_64-windows ;;
        *) die "unsupported Windows arch $arch" ;;
      esac ;;
    *) die "unsupported host OS $os" ;;
  esac
}

copy_bundled_lld() {
  local host src dst
  host="$(host_target_name)"
  src="$LLD_BIN"
  [[ "$host" == *windows ]] && src="$LLD_BIN.exe"
  [[ -f "$src" ]] || die "bundled lld missing: $src (run tools/build_toolchain.sh)"
  dst="$ASSETS/lld/$host/lld"
  [[ "$host" == *windows ]] && dst="$dst.exe"
  mkdir -p "$(dirname "$dst")"
  cp "$src" "$dst"
  echo "gen_assets: bundled lld -> $dst"
}

# Print every zig-cache .o/.a/.obj/.lib path on the linker command line of a
# zig cc link (the authoritative list of CRT/libc inputs for the target).
discover_zig_link_inputs() {
  local triple="$1"; shift
  local tmp cmd
  tmp="$(mktemp -d)"
  printf 'int main(void){return 0;}\n' > "$tmp/t.c"
  cmd="$(zig cc -target "$triple" "$@" -v "$tmp/t.c" -o "$tmp/t.out" 2>&1 \
    | grep -E '^(ld\.lld|lld-link) ' | tail -1 || true)"
  [[ -n "$cmd" ]] || die "could not capture zig link line for $triple"
  # shellcheck disable=SC2086
  printf '%s\n' $cmd | grep -E '^/.*\.(o|a|obj|lib)$' | grep -v '/tmp/' | grep -vE '/t\.o$|/t\.obj$' || true
  rm -rf "$tmp"
}

require_any() {
  local dir="$1"; shift
  local f missing=0
  for f in "$@"; do
    [[ -s "$dir/$f" ]] || { echo "gen_assets: missing required file $dir/$f" >&2; missing=1; }
  done
  [[ $missing -eq 0 ]] || die "asset set incomplete: $dir"
}

gen_linux_target() {
  local name="$1" triple="$2"
  local dir="$ASSETS/$name" f
  command -v zig >/dev/null || die "zig not found on PATH (required for $name)"
  mkdir -p "$dir"
  zig cc -target "$triple" -O2 -c "$ROOT/runtime/runtime.c" -o "$dir/runtime.o"
  # empty CRT framing placeholders (zig's musl link uses none; lld accepts them)
  zig cc -target "$triple" -c -x c /dev/null -o "$dir/crti.o"
  zig cc -target "$triple" -c -x c /dev/null -o "$dir/crtn.o"
  while IFS= read -r f; do
    [[ -n "$f" ]] || continue
    case "$(basename "$f")" in libubsan_rt.a|ubsan_rt.lib) continue ;; esac
    cp "$f" "$dir/$(basename "$f")"
  done < <(discover_zig_link_inputs "$triple" -static)
  require_any "$dir" runtime.o crt1.o crti.o crtn.o libc.a libzigc.a libcompiler_rt.a
  echo "gen_assets: $name generated"
}

gen_windows_target() {
  local name="x86_64-windows" triple="x86_64-windows-gnu"
  local dir="$ASSETS/$name" f base
  command -v zig >/dev/null || die "zig not found on PATH (required for $name)"
  mkdir -p "$dir"
  zig cc -target "$triple" -O2 -c "$ROOT/runtime/runtime.c" -o "$dir/runtime.obj"
  while IFS= read -r f; do
    [[ -n "$f" ]] || continue
    base="$(basename "$f")"
    case "$base" in
      crt2.obj) base=crt2.o ;;
      libmingw32.lib) base=libmingw32.a ;;
      zigc.lib) base=libzigc.a ;;
      ubsan_rt.lib) continue ;;
      compiler_rt.lib) base=libcompiler_rt.a ;;
    esac
    cp "$f" "$dir/$base"
  done < <(discover_zig_link_inputs "$triple")
  mv "$dir/runtime.obj" "$dir/runtime.o"
  require_any "$dir" runtime.o crt2.o libmingw32.a libzigc.a libcompiler_rt.a \
    kernel32.lib api-ms-win-crt-stdio-l1-1-0.lib
  echo "gen_assets: $name generated"
}

gen_macos_target() {
  local name="$1" triple="$2" arch="$3"
  local dir="$ASSETS/$name" sdk
  [[ "$(uname -s)" == Darwin ]] || return 0
  command -v xcrun >/dev/null || die "xcrun not found (required for $name)"
  mkdir -p "$dir"
  xcrun --sdk macosx clang -c -target "$triple" -O2 "$ROOT/runtime/runtime.c" -o "$dir/runtime.o"
  sdk="$(xcrun --show-sdk-path)"
  cp "$sdk/usr/lib/libSystem.tbd" "$dir/libSystem.tbd"
  require_any "$dir" runtime.o libSystem.tbd
  echo "gen_assets: $name generated (arch $arch)"
}

compile_prog() { # $1=triple-or-macos-arch-kind $2=output
  local kind="$1" out="$2"
  local tmp; tmp="$(mktemp -d)"
  printf 'int main(void){return 0;}\n' > "$tmp/t.c"
  case "$kind" in
    linux-*) zig cc -target "${kind#linux-}" -c "$tmp/t.c" -o "$out" ;;
    windows) zig cc -target x86_64-windows-gnu -c "$tmp/t.c" -o "$out" ;;
    macos-x86_64) xcrun --sdk macosx clang -c -target x86_64-apple-darwin "$tmp/t.c" -o "$out" ;;
    macos-arm64) xcrun --sdk macosx clang -c -target aarch64-apple-darwin "$tmp/t.c" -o "$out" ;;
    *) die "unknown prog kind $kind" ;;
  esac
  rm -rf "$tmp"
}

magic_ok() { # $1=file $2=family
  local f="$1" fam="$2" head
  head="$(head -c 4 "$f" | od -An -tx1 | tr -d ' \n')"
  case "$fam" in
    elf) [[ "$head" == 7f454c46* ]] || return 1 ;;
    pe)  [[ "$(head -c 2 "$f")" == MZ ]] || return 1 ;;
    macho) [[ "$head" == cffaedfe* || "$head" == feedfacf* ]] || return 1 ;;
    *) return 1 ;;
  esac
}

validate_dir() { # $1=target-name
  local name="$1" dir="$ASSETS/$1"
  local staged
  [[ -d "$dir" ]] || { echo "gen_assets: skip $name (dir absent)"; return 0; }

  # required-file checks (always)
  case "$name" in
    x86_64-linux|aarch64-linux)
      require_any "$dir" runtime.o crt1.o crti.o crtn.o libc.a libzigc.a libcompiler_rt.a ;;
    x86_64-windows)
      require_any "$dir" runtime.o crt2.o libmingw32.a libzigc.a libcompiler_rt.a \
        advapi32.lib kernel32.lib ntdll.lib shell32.lib user32.lib \
        api-ms-win-crt-conio-l1-1-0.lib api-ms-win-crt-convert-l1-1-0.lib \
        api-ms-win-crt-environment-l1-1-0.lib api-ms-win-crt-filesystem-l1-1-0.lib \
        api-ms-win-crt-heap-l1-1-0.lib api-ms-win-crt-locale-l1-1-0.lib \
        api-ms-win-crt-math-l1-1-0.lib api-ms-win-crt-multibyte-l1-1-0.lib \
        api-ms-win-crt-private-l1-1-0.lib api-ms-win-crt-process-l1-1-0.lib \
        api-ms-win-crt-runtime-l1-1-0.lib api-ms-win-crt-stdio-l1-1-0.lib \
        api-ms-win-crt-string-l1-1-0.lib api-ms-win-crt-time-l1-1-0.lib \
        api-ms-win-crt-utility-l1-1-0.lib ;;
    x86_64-macos|aarch64-macos)
      require_any "$dir" runtime.o libSystem.tbd ;;
    *) die "unknown target dir $name" ;;
  esac

  # full validation link (when the target's compiler + lld are available)
  staged="$(mktemp -d)"
  local prog="$staged/prog.o" out="$staged/out"
  case "$name" in
    x86_64-linux|aarch64-linux)
      if command -v zig >/dev/null && [[ -x "$LLD" ]]; then
        [[ "$name" == x86_64-linux ]] && triple=x86_64-linux-musl || triple=aarch64-linux-musl
        compile_prog "linux-$triple" "$prog"
        cp "$dir"/* "$staged/"
        local m=()
        [[ "$name" == x86_64-linux ]] && m=(-m elf_x86_64)
        "$LLD" -static ${m[@]+"${m[@]}"} -o "$out" "$staged/crt1.o" "$staged/crti.o" "$prog" \
          "$staged/runtime.o" -L "$staged" -lc -lzigc -lcompiler_rt "$staged/crtn.o"
        magic_ok "$out" elf || die "$name: validation link output is not ELF"
        echo "gen_assets: $name validation link OK (ELF)"
      else
        echo "gen_assets: $name filename checks OK (zig or lld unavailable; link not run)"
      fi ;;
    x86_64-windows)
      if command -v zig >/dev/null && [[ -x "$LLD" ]]; then
        compile_prog windows "$prog"
        cp "$dir"/* "$staged/"
        local rest=()
        while IFS= read -r f; do
          base="$(basename "$f")"
          case "$base" in crt2.o|prog.o|runtime.o) ;; *) rest+=("$f") ;; esac
        done < <(ls "$staged"/* | sort)
        "$LLD" -m i386pep --subsystem console -o "$out" \
          "$staged/crt2.o" "$prog" "$staged/runtime.o" "${rest[@]}"
        magic_ok "$out" pe || die "$name: validation link output is not PE"
        echo "gen_assets: $name validation link OK (PE)"
      else
        echo "gen_assets: $name filename checks OK (zig or lld unavailable; link not run)"
      fi ;;
    x86_64-macos|aarch64-macos)
      if command -v xcrun >/dev/null && [[ -x "$LLD64" ]]; then
        local larch=arm64
        [[ "$name" == x86_64-macos ]] && larch=x86_64
        compile_prog "macos-$larch" "$prog"
        cp "$dir"/* "$staged/"
        "$LLD64" -arch "$larch" -platform_version macos 11.0 11.0 -o "$out" \
          "$prog" "$staged/runtime.o" -L "$staged" -lSystem
        magic_ok "$out" macho || die "$name: validation link output is not Mach-O"
        echo "gen_assets: $name validation link OK (Mach-O $larch)"
      else
        echo "gen_assets: $name filename checks OK (xcrun or lld unavailable; link not run)"
      fi ;;
  esac
  rm -rf "$staged"
}

if [[ "$MODE" == refresh-lld ]]; then
  copy_bundled_lld
  exit 0
fi

if [[ "$MODE" == generate ]]; then
  gen_linux_target x86_64-linux x86_64-linux-musl
  gen_linux_target aarch64-linux aarch64-linux-musl
  gen_windows_target
  gen_macos_target x86_64-macos x86_64-apple-darwin x86_64
  gen_macos_target aarch64-macos aarch64-apple-darwin arm64
  [[ -f "$LLD" ]] || die "vendor/llvm-prefix missing — run tools/build_toolchain.sh"
  copy_bundled_lld
fi

for t in x86_64-linux aarch64-linux x86_64-windows x86_64-macos aarch64-macos; do
  validate_dir "$t"
done

echo "gen_assets: done"

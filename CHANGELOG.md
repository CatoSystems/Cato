# Changelog

Hand-maintained release notes for Cato. Each released version gets a
`## [x.y.z]` section matching its git tag; the release workflow publishes
that section as the GitHub Release notes.

## [Unreleased]

### Changed

- The workspace builds with Rust edition 2024.

### Fixed

- Module loading on Windows: a `require` that reached a file through `..`
  across `\` separators normalized to a second spelling of the same path,
  so the module loaded twice and its `ns` was reported as a duplicate.
- CI, against the current stable toolchain: the newer clippy lints, the
  formatter golden comparison on a checkout that rewrites line endings,
  and an `ETXTBSY` race when the CLI passthrough tests share a fake `cc`.

## [0.1.0] - 2026-10-21

### Added

- First distributed release of the seed CLI: the `cato` compiler
  front end with the interpreter and LLVM backends, the standard
  library, the `cato fmt` formatter, and the bundled examples.
- Packaged binaries for macOS (arm64, x86_64), Linux (x86_64,
  aarch64 — fully static via musl) and Windows (x86_64, static CRT),
  published as tarballs with SHA-256 checksums on each tagged release.

### Notes

- Native compiles shell out to a system `clang`; hermetic builds with
  no toolchain installed (`static-llvm` / `in-process-lld`) are not
  part of this release yet.

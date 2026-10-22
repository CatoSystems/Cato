# The configuration of the LLVM + lld toolchain that cato links against.
#
# This file is read by tools/build_toolchain.sh as CMake's initial cache
# (`cmake -C`) and hashed by CI to key the toolchain cache: changing the
# toolchain's content invalidates the cache, editing the build script does not.
#
# Every entry must be a cache entry. `-C` only seeds the cache, and a plain
# `set(VAR value)` there is discarded the moment LLVM declares the variable
# (`set(VAR "" CACHE ...)` / `option(VAR ...)`), which would silently drop the
# setting — an empty LLVM_ENABLE_PROJECTS builds no lld at all.
#
# The toolchain is deliberately minimal. cato only needs LLVM's code
# generation for two targets and lld's linkers; everything else is build
# time that nobody uses.
set(LLVM_ENABLE_PROJECTS "lld" CACHE STRING
  "Semicolon-separated list of projects to build alongside LLVM.")
set(LLVM_TARGETS_TO_BUILD "X86;AArch64" CACHE STRING
  "Semicolon-separated list of target backends to build.")

# Compression is only used by tools cato never invokes; OFF keeps the
# toolchain free of zlib and zstd.
set(LLVM_ENABLE_ZLIB "OFF" CACHE STRING
  "Use zlib for compression/decompression if available. Can be ON, OFF, or FORCE_ON.")
set(LLVM_ENABLE_ZSTD "OFF" CACHE STRING
  "Use zstd for compression/decompression if available. Can be ON, OFF, or FORCE_ON.")

# lld and LLVM are linked statically into the compiler process.
set(BUILD_SHARED_LIBS OFF CACHE BOOL "Build shared libraries instead of static ones.")

# The unit tests, the examples and the benchmarks are not built: they are a
# large part of the build time and nothing in cato uses them.
set(LLVM_INCLUDE_TESTS OFF CACHE BOOL "Generate build targets for the LLVM unit tests.")
set(LLVM_BUILD_TESTS OFF CACHE BOOL "Build LLVM unit tests.")
set(LLVM_INCLUDE_EXAMPLES OFF CACHE BOOL "Generate build targets for the LLVM examples.")
set(LLVM_INCLUDE_BENCHMARKS OFF CACHE BOOL "Generate benchmark targets.")

# lld picks its driver from argv[0]; "ld.lld" must mean the ELF driver, not
# the MinGW one, because cato links ELF targets from every host.
set(LLD_DEFAULT_LD_LLD_IS_MINGW OFF CACHE BOOL
  "Use MinGW as the default backend for ld.lld. If OFF, ELF will be used.")

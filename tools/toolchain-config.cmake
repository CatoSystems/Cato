# The configuration of the LLVM + lld toolchain that cato links against.
#
# This file is read by tools/build_toolchain.sh as CMake's initial cache and
# hashed by CI to key the toolchain cache: changing the toolchain's content
# invalidates the cache, editing the build script does not.
#
# The toolchain is deliberately minimal. cato only needs LLVM's code
# generation for two targets and lld's linkers; everything else is build
# time that nobody uses.
set(LLVM_ENABLE_PROJECTS lld)
set(LLVM_ENABLE_TERMINFO OFF)
set(LLVM_ENABLE_ZLIB OFF)
set(LLVM_ENABLE_ZSTD OFF)
set(LLVM_TARGETS_TO_BUILD "X86;AArch64")
set(BUILD_SHARED_LIBS OFF)

# The unit tests, the examples and the benchmarks are not built: they are a
# large part of the build time and nothing in cato uses them.
set(LLVM_INCLUDE_TESTS OFF)
set(LLVM_BUILD_TESTS OFF)
set(LLVM_INCLUDE_EXAMPLES OFF)
set(LLVM_INCLUDE_BENCHMARKS OFF)

# lld picks its driver from argv[0]; "ld.lld" must mean the ELF driver, not
# the MinGW one, because cato links ELF targets from every host.
set(LLD_DEFAULT_LD_LLD_IS_MINGW OFF)

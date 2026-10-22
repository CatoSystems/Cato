#include "lld/Common/Driver.h"
#include <cstdlib>
#include <cstring>
#include <string>
#include <vector>
#include <llvm/ADT/ArrayRef.h>
#include <llvm/Support/raw_ostream.h>

namespace lld {
namespace elf {
bool link(llvm::ArrayRef<const char*> args, llvm::raw_ostream& stdoutOS,
          llvm::raw_ostream& stderrOS, bool exitEarly, bool disableOutput);
}
namespace mingw {
bool link(llvm::ArrayRef<const char*> args, llvm::raw_ostream& stdoutOS,
          llvm::raw_ostream& stderrOS, bool exitEarly, bool disableOutput);
}
namespace macho {
bool link(llvm::ArrayRef<const char*> args, llvm::raw_ostream& stdoutOS,
          llvm::raw_ostream& stderrOS, bool exitEarly, bool disableOutput);
}
}

// Copy a message into a buffer the caller frees with cato_lld_free_err.
// malloc is used instead of strdup because strdup is a deprecated POSIX
// name on Windows, where the C runtime only offers it behind a warning.
static char* dup_message(const char* text) {
    std::size_t size = std::strlen(text) + 1;
    char* copy = (char*)std::malloc(size);
    if (copy) {
        std::memcpy(copy, text, size);
    }
    return copy;
}

extern "C" int cato_lld_link(int flavor,
                            const char* const* argv,
                            int argc,
                            char** out_err) {
    if (argc <= 0) {
        *out_err = dup_message("lld: no input files");
        return 1;
    }
    std::vector<const char*> args(argv, argv + argc);
    std::string diag;
    llvm::raw_string_ostream os(diag);
    bool ok = false;
    switch (flavor) {
    case 0:
        ok = lld::elf::link(llvm::ArrayRef<const char*>(args), os, os,
                            /*exitEarly=*/false, /*disableOutput=*/false);
        break;
    case 1:
        ok = lld::mingw::link(llvm::ArrayRef<const char*>(args), os, os,
                              /*exitEarly=*/false, /*disableOutput=*/false);
        break;
    case 2:
        ok = lld::macho::link(llvm::ArrayRef<const char*>(args), os, os,
                               /*exitEarly=*/false, /*disableOutput=*/false);
        break;
    default:
        *out_err = dup_message("cato-lld-sys: unknown flavor");
        return 1;
    }
    os.flush();
    if (ok) {
        *out_err = nullptr;
        return 0;
    }
    if (diag.empty()) {
        *out_err = dup_message("lld failed");
    } else {
        *out_err = dup_message(diag.c_str());
    }
    return 1;
}

extern "C" void cato_lld_free_err(char* p) {
    if (p) {
        free(p);
    }
}

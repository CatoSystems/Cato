# AGENTS.md

Guidance for agents (and humans) working in this repository.

## Project
Cato is a statically-typed, compiled, Lisp-flavored language (Rust workspace,
LLVM backend, interpreter). See `README.md` and `docs/ARCHITECTURE.md`.

## Working style
- Prefer small, logical commits and reviewable increments over one large patch.
- Keep files under ~400 lines; split when they grow beyond that.
- Follow existing folder structure, naming, and conventions; do not invent parallel layouts.
- Reuse existing infrastructure rather than duplicating it.
- Keep the git working tree clean: ignore tool/agent state, generated results, and binaries; purge unwanted files from history, not just the working tree.
- Before rewriting git history, stash/preserve uncommitted changes first.

## Code quality
- Modular, elegant code; avoid tangled spaghetti.
- Coherent, well-designed APIs; every public API needs a clear reason to exist. Backward compatibility is not required.
- Maximize usefulness, coherence, and quality over API count.
- Represent errors explicitly (e.g. `Result`); never silently discard them.
- Pointers and low-level primitives stay available for systems programming; no mandatory GC.
- Use established standards/mature libraries for Unicode, crypto, etc. instead of hand-rolled approximations.
- Keep implemented vs. planned features clearly distinguished in docs; do not present proposed syntax as implemented.

## Comments
- Code must be commented properly: explain the abstraction in place, why it is useful, and other relevant remarks.
- Do not reference phase names (S1, S4, S8, ...) or spec documents in comments.
- Comments describe the abstraction, not the change history.

## Performance
- Performance is a major concern. Avoid unnecessary allocations, clones, and O(n²) passes; prefer in-place updates on provable last use.
- Validate with tests, benchmarks, and compiling examples — not API proposals alone.

## Portability / honesty
- Make explicit, honest claims about safety, portability, and completeness; do not claim guarantees that are not enforced or mark work complete prematurely.
- This codebase is used by others: keep it robust, documented, and production-ready.

## Output
- Prefer succinct documentation and short, single-line rows.

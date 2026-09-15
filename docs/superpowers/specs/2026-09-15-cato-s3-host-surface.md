# S3 — Host surface: numbers↔strings, files, argv, exit, spawn

Date: 2026-09-15
Goal: the compiler is a program that reads files, writes files, converts
numbers and strings, and starts other programs. This slice gives Cato that
surface with both backends in parity. Reference: `examples/host.cato`.

## Builtins

- `(str n)` / number→string: `str` accepts numeric (and bool) operands and
  produces their decimal text — the emitter cannot emit IR without this.
  Formats exactly like the printed form (`%g`-style for floats is NOT
  required; integers are exact, floats use the shortest round-trip the C
  runtime's `%.17g`-vs-print parity allows — specified as: floats format
  identically to `print-f64`'s runtime formatting but **without** the
  trailing newline).
- `(parse-i64 s)` → `(Option i64)`, `(parse-f64 s)` → `(Option f64)` —
  S1a's `Option` pays off. Leading/trailing whitespace rejected; the whole
  string must parse; overflow → `None`.
- `(print-str s)` — raw print without a trailing newline (all existing
  printers keep their `\n`).
- `(slurp path)` → `str` — read a whole file; `(spit path content)` →
  writes (creates/truncates). Runtime helpers in `runtime.c`, shared by
  both backends.
- `(args)` → `(Vector str)` — process arguments (argv[1..]; argv[0] is the
  program name and is not exposed). The emitted `main` gains argc/argv
  plumbing; runtime helper `cato_args` copies them into a heap vector.
- `(exit n)` — terminate with a status; `noreturn` in codegen.
- Spawning the C compiler stays a direct `(extern ...)` of libc
  (`system`); the interpreter models the externs a compiler needs
  (`system` at minimum) so `cato run` works on kernel code.

## Parity contract

Every new builtin is modeled natively by the interpreter (no
"unsupported extern" for anything this slice adds), and the parity suite
covers numeric formatting, parse round-trips, file write→read, and argv
content. `examples/host.cato` exercises the full surface end-to-end.

## Non-goals

Buffered/line IO, directories, environment variables, piped subprocess
IO (`popen`), string formatting mini-languages.

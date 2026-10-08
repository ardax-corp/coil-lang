# Dissect

`coil dissect` re-execs the sibling `coil-dissect` helper (git-style). That helper
compiles a `.hy` entry (and its module graph) **in memory** — it never writes
`out.hyc` — and prints a filtered view of the result for DX (`compiler` feature
`dissect`).

```bash
cargo build   # coil + coil-dissect (+ other helpers)
coil dissect examples/fib.hy --fn fib
coil dissect examples/fib.hy --fn fib --il
coil dissect examples/fib.hy --ast
coil dissect gated.hy --allow-attach --fn main
# or:
coil-dissect examples/fib.hy --fn fib --il
```

| Flag | Effect |
|------|--------|
| (none) | Symbol index + full final fused bytecode (function headers interleaved), with the source line (`;; file:line │ text`) shown wherever it changes. Dense register ops are decoded (`r3 = r1`, `r5 = array(r15..r18)`, `r2 = len(r5)`) |
| `--no-source` | Bytecode without the interleaved source lines |
| `--fn <pat>` | Case-insensitive FQN match (exact, substring, trailing segment, `name#N`) |
| `--il` | Also print **pre-opt** stack IL (snapshot after finalize splices, before lower) |
| `--il-post` | Also print the **optimized** IL (after IL passes and MIR substitution, before fuse / lowering) |
| `--tests` | Compile `test("…") { … }` cases too (`__zs_test_N`), so PCs match a `coil test` run (for example a gc-stress report) |
| `--mir` | Also print the MIR of each numeric body that reached emission, marked `dense` / `lir`; a dense body says whether it was kept or lost the cost gate to fuse-IL |
| `--effects` | Also print each function's effects with the first reason for each (`step: uses {write, suspend}: calls `write_all` (write, suspend)`, `apply: pure apart from its parameters: calls parameter `f``), then, when auto-par is on, each counted loop or fork site left sequential only because a callee is impure (`;; loop over `i` in `main` not parallelized: `step` reads static `SCALE` (read)`). Needs HIR lowering (the default); see [auto-par.md](auto-par.md) |
| `-O LEVEL` | Optimization level, as for `coil compile` (`--opt-stats` / `--opt-stats-json` print the IL counters) |
| `--ast` | Also pretty-print the entry-file AST (as `coil fmt` writes it) |
| `--root DIR` | Extra `use`/`mod` search directory (repeatable; default `src`) |
| `--entry FILE` | Entry `.hy` instead of the positional file |
| `--allow-attach` | Allow `Stream.attach` at typecheck (default deny) |
| `--allow-exit` | Allow `env::exit` at typecheck (default deny) |
| `--allow-exec` | Allow `env::exec` at typecheck (default deny) |
| `--allow-ffi-exec` | Allow FFI process-exec symbols (`system`, `execve`, …) |
| `--allow-dload STEM` | Allow `dload` of STEM (repeatable; `dload("c")` still denied) |
| `--ffi-search-path DIR` | Extra FFI lookup directory (repeatable; not a dload grant) |

Host grants match `coil` compile/run (`HostGrantFlags` → `Pipeline` `HostGrants`). They are CLI-only — `coil.toml` does not grant them. Dissect compiles in memory; gated calls without a flag fail typecheck (`E0406`–`E0411`) the same as `coil compile`. `--ffi-search-path` is lookup only.

A `.hyc` archive as the entry (`coil dissect out.hyc`) dumps its bytecode
without compiling; `--il` / `--il-post` / `--hir` / `--effects` / `--mir` / `--ast` need a source.
Archives may not record function symbols, in which case the listing is one
`<program>` section.

Name filtering uses live compiler FQNs (`fib`, `mod::fib`, `Foo::bar`, `Show__int__show`, …). 

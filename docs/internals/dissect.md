# Dissect

`coil dissect` re-execs the sibling `coil-dissect` helper (git-style). That helper
compiles a `.coil` entry (and its module graph) **in memory** — it never writes
`out.ccoil` — and prints a filtered view of the result for DX (`compiler` feature
`dissect`).

```bash
cargo build   # coil + coil-dissect (+ other helpers)
coil dissect examples/fib.coil --fn fib
coil dissect examples/fib.coil --fn fib --hir
coil dissect examples/fib.coil --ast
coil dissect gated.coil --allow-attach --fn main
# or:
coil-dissect examples/fib.coil --fn fib
```

| Flag | Effect |
|------|--------|
| (none) | A symbol index, then `=== mir ===`: each body's MIR under `;; fn NAME` |
| `--fn <pat>` | Case-insensitive FQN match (exact, substring, trailing segment, `name#N`) |
| `--hir` | Also print each body's HIR (typed, desugared tree), under `=== hir ===` |
| `--expand` | Print the entry file after macro expansion (no MIR); generated and `comptime` items are marked with where they came from |
| `--tests` | Compile `test("…") { … }` cases too (`__zs_test_N`), like `coil test` |
| `--mir` | Print the MIR the engines run (the default) |
| `--effects` | Also print each function's effects with the first reason for each (`step: uses {write, suspend}: calls `write_all` (write, suspend)`, `apply: pure apart from its parameters: calls parameter `f``) and the parameters it mutates (`Point::bump: uses {mutate}: writes field `x` of a shared object (mutate); mutates self`, the summary a call with a `readonly` argument is checked against), then, when auto-par is on, each counted loop or fork site left sequential only because a callee is impure (`;; loop over `i` in `main` not parallelized: `step` reads static `SCALE` (read)`). See [auto-par.md](auto-par.md) |
| `--boxes` | Also print every place lowering boxes a value: the function, what was boxed (`pair`, `variant`, `niche`, `host result`), its type and the lowering line that asked for it (`stored: pair `Option<int>` (from_hir.rs:5779)`). See [layout.md](layout.md) |
| `-O LEVEL` | Optimization level, as for `coil compile` (`--opt-stats` / `--opt-stats-json` print HIR counters and compile-time work) |
| `--ast` | Also pretty-print the entry-file AST (as `coil fmt` writes it) |
| `--root DIR` | Extra `use`/`mod` search directory (repeatable; default `src`) |
| `--entry FILE` | Entry `.coil` instead of the positional file |
| `--allow-read` / `--allow-write` | Allow reachable file reads / writes (`io::open`, `io::fs::*`; default deny) |
| `--allow-net` | Allow reachable sockets (`io::net::tcp::*`, `udp_*`; default deny) |
| `--allow-env` | Allow reachable environment / working-directory calls (`env::var`, …; default deny) |
| `--allow-attach` | Allow `Stream.attach` (default deny) |
| `--allow-exit` | Allow reachable `env::exit` (default deny) |
| `-A`, `--allow-all` | Every capability above plus `--allow-ffi-exec` (not `dload`) |
| `--allow-ffi-exec` | Allow FFI process-exec symbols (`system`, `execve`, …) |
| `--allow-dload STEM` | Allow `dload` of STEM (repeatable; `dload("c")` still denied) |
| `--ffi-search-path DIR` | Extra FFI lookup directory (repeatable; not a dload grant) |
| `--dload-pin STEM=SHA256` / `--dload-trusted STEM` | `dload` integrity: expected library hash, or no hash check |

Host grants match `coil` compile/run (`HostGrantFlags` → `Pipeline` `HostGrants`). They are CLI-only. Dissect compiles in memory; gated calls without a flag fail the compile (`E0406`–`E0411`, `E0414`) the same as `coil compile` ([pipeline.md](pipeline.md#capabilities)). `--ffi-search-path` is lookup only.

A `.ccoil` archive as the entry (`coil dissect out.ccoil`) prints its MIR without
compiling; `--hir`, `--effects`, `--boxes`, `--ast` and `--expand` need a source.

Name filtering uses live compiler FQNs (`fib`, `mod::fib`, `Foo::bar`, `Show__int__show`, …). 

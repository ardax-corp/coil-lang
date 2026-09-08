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
| (none) | Symbol index + full final fused bytecode (function headers interleaved) |
| `--fn <pat>` | Case-insensitive FQN match (exact, substring, trailing segment, `name#N`) |
| `--il` | Also print **pre-opt** stack IL (snapshot after finalize splices, before lower) |
| `--ast` | Also pretty-print the entry-file AST |
| `--root DIR` | Extra `use`/`mod` search directory (repeatable; default `src`) |
| `--entry FILE` | Entry `.hy` instead of the positional file |
| `--allow-attach` | Allow `Stream.attach` at typecheck (default deny) |
| `--allow-exit` | Allow `env::exit` at typecheck (default deny) |
| `--allow-exec` | Allow `env::exec` at typecheck (default deny) |
| `--allow-ffi-exec` | Allow FFI process-exec symbols (`system`, `execve`, …) |
| `--allow-dload STEM` | Allow `dload` of STEM (repeatable; `dload("c")` still denied) |
| `--ffi-search-path DIR` | Extra FFI lookup directory (repeatable; not a dload grant) |

Host grants match `coil` compile/run (`HostGrantFlags` → `Pipeline` `HostGrants`). They are CLI-only — `coil.toml` does not grant them. Dissect compiles in memory; gated calls without a flag fail typecheck (`E0406`–`E0411`) the same as `coil compile`. `--ffi-search-path` is lookup only.

Name filtering uses live compiler FQNs (`fib`, `mod::fib`, `Foo::bar`, `Show__int__show`, …). Archive-side symbol tables are a follow-up; v1 is source-first only.

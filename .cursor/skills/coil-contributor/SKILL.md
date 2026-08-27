---
name: coil-contributor
description: >-
  Modify the coil compiler, VM, pipeline, or language implementation in Rust.
  Use when changing parser/typechecker/codegen/VM crates, adding opcodes or
  language features, bumping ARCHIVE_VERSION, running poop/vm_bench, or when
  the user asks about coil internals, bytecode, or the compilation pipeline.
---

# coil contributor

coil is a Rust workspace: parse → HM typecheck → stack IL codegen → IL opts → lower → `.hyc` archive → VM.

Read [AGENTS.md](AGENTS.md) for user preferences and global invariants — this skill covers **where to change code** and **verification**.

## Workspace map

```
coil/
├── common/       # Instruction opcodes, Value, archive (ARCHIVE_VERSION)
├── parser/       # Pratt parser, AST
├── compiler/     # HM typechecker, stack IL (compiler/src/il/), pipeline
├── machine/      # VM, heap/GC, FFI, host natives (HostInvoke)
├── coil-simd/    # Stable std::arch SIMD kernels (packed LA, bytes)
├── src/main.rs   # CLI: run, compile, test, debug, dissect
├── examples/     # Runnable .hy demos
├── tests/        # Integration .hy tests (coil test)
└── docs/         # Contributor internals (user docs: coil-website)
```

Pipeline detail: [docs/internals/pipeline.md](docs/internals/pipeline.md).

## Change routing

| Task | Primary location |
|------|------------------|
| Syntax / grammar | `parser/` |
| Types / diagnostics | `compiler/src/typechecking/` |
| Codegen / IL opts | `compiler/src/lib.rs`, `compiler/src/il/` |
| Opcodes / VM handlers | `common/` (Instruction), `machine/src/vm.rs` |
| Archive format | `common/src/archive.rs` |
| Host natives / virtual modules | `compiler/` (resolve), `machine/` (HostInvoke) |
| CLI | `src/main.rs` |

Single compilation path: stack codegen in `compiler/src/lib.rs` — no register VM.

## Hard invariants (never skip)

1. **Append-only opcodes** — new `Instruction` variants only at end of enum (`#[repr(u8)]`). Then bump:
   - archive **minor** (`ARCHIVE_MINOR` / packed `ARCHIVE_VERSION` in `common/src/archive.rs`)
   - `promise!` ceiling in `machine/src/vm.rs`
   - `instruction_from_u8_covers_last_appended_variant` test
   Incompatible ABI/layout changes bump archive **major** (reset minor). Loaders accept same major with archive minor ≤ runtime minor.
2. **Virtual module natives** — use `HostInvoke`, not new opcodes for `io`/`thread`/etc.
3. **Reject benchmark-shaped opcodes** unless pattern is universal (see AGENTS.md user preferences).
4. **New language features** — full HM integration + user-doc updates in [coil-website](https://github.com/ardax-corp/coil-website) (`src/content/docs/`) + internals here when needed + minimal runnable example. Prefer **method-based APIs** (`impl` methods on classes) over free functions for operations tied to a receiver type; free generic fns returning enums are codegen-fragile (see [limitations.md](docs/internals/limitations.md)).

`STORE` vs deprecated `StorePop`: compiler emits `STORE` only. Match bindings skip store (value already in slot via `UNPACK`/`JUMP_IF_MATCH`) but codegen must reserve those slots in `variables` so arm-body temps cannot clobber them.

Stack IL: symbolic labels until `finalize_bytecode` → single `il::lower` after concat (not per-function lower). See [reference.md](reference.md) and `docs/internals/pipeline.md`.

## Verification checklist

```bash
cargo check --workspace          # lint gate (clippy has known Gc exception)
cargo test --workspace --lib --tests --bins   # required; includes */tests/* (skip Criterion benches)
# Bare optional stack (time tests cfg-skipped):
#   cargo test --workspace --lib --tests --bins --no-default-features
# Feature compile-gates / tooling (match CI matrix job titles):
#   cargo check --workspace --lib --tests --bins --no-default-features --features time
#   cargo test --workspace --lib --tests --bins --features dissect
cargo build --bin coil && (ulimit -v 65536; ./target/debug/coil test)  # leak smoke (64MB)
cargo build --release --workspace
./scripts/poop_baseline.sh       # soft CPU check before/after perf work
rm -f out.hyc && cargo run --release -- examples/fib.hy   # expect 55 (default run needs no out.hyc)
```

| Work type | Extra check |
|-----------|-------------|
| VM / alloc | valgrind memcheck on debug build |
| Perf | `./scripts/poop_baseline.sh` (precompiled `coil run` on mandelbrot/tak/nsieve/binary_trees); prefer alloc reduction over new opcodes |
| Debug info | `coil debug`, `coil dissect` |
| Formatting | `coil fmt` |
| Packaged apps | `coil package` defaults to `coil-embed` runner |

## Feature work workflow

1. Draft plan for large language changes (do not edit attached plan files during impl).
2. Parser + AST if syntax changes.
3. Typechecker — new types/constraints/diagnostics (`E####` in coil-website `src/content/docs/references/error-codes.md`).
4. Codegen — `BlockBuilder` / `IlBuilder`; extend IL opts only when justified.
5. VM only if new opcode (rare) or runtime behavior.
6. coil-website `src/content/docs/` (manual/references; routes `/docs/…`) + example in `examples/` or `tests/`.
7. Bump archive **minor** for additive bytecode; bump **major** if bytecode/tag/opcode incompatible.
8. Granular conventional commits; stage only related files.

## Virtual modules (compiler-provided)

Auto: `prelude`, `prelude::ops`, `prelude::test`, `prelude::math`.

Explicit `use`: `ffi`, `io`, `thread`, `time`, `env`, `string`, `gc`. Regex: [coil-regex](https://github.com/ardax-corp/coil-regex). TLS: [coil-tls](https://github.com/ardax-corp/coil-tls). Crypto: [coil-crypto](https://github.com/ardax-corp/coil-crypto).

Cargo feature `time` gates that virtual module (default on). There is no virtual crypto module; hashes/AEAD live in [coil-crypto](https://github.com/ardax-corp/coil-crypto).

Prefer compiler builtins over userland for core type machinery.

**API shape:** inherent and trait methods over free functions for type-tied ops (`map.insert(k, v)` not `insert(map, k, v)`). Virtual-module free fns are fine for host primitives (`io::read`); stdlib and new language surface should default to methods.

## Useful tools

| Tool | Use |
|------|-----|
| `coil dissect file.hy --fn pat` | IL/bytecode dump without running |
| `coil debug file.hy -x script --batch` | VM debugger (see coil-debug skill) |
| `coil fmt [--check] path` | Pretty-print `.hy` (AST; comments dropped) |
| `poop` | Instruction/HW counter baselines |
| `valgrind` | Leaks, callgrind |
| `heaptrack` | Allocation tracing |

## Additional resources

- Opcode list: [docs/internals/opcodes.md](docs/internals/opcodes.md)
- Debug line table: [docs/internals/debug-info.md](docs/internals/debug-info.md)
- Crate/file detail: [reference.md](reference.md)
- Writing `.hy` programs: **coil-language** skill

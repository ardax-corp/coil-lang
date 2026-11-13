---
name: coil-contributor
description: >-
  Modify the coil compiler, MIR passes, native backend, MIR interpreter, runtime
  or language implementation in Rust. Use when changing parser/typechecker/HIR/MIR
  crates, adding host natives or language features, bumping the archive version,
  running poop, or when the user asks about coil internals or the compilation
  pipeline.
---

# coil contributor

coil is a Rust workspace: parse → HM typecheck → HIR (inlining + optimization passes) → MIR for every body → MIR passes → native code (Cranelift, JIT or AOT), or the MIR interpreter for compile-time code, `coil debug` and wasm. A `.ccoil` archive holds MIR and data.

Read [AGENTS.md](AGENTS.md) for user preferences and global invariants — this skill covers **where to change code** and **verification**.

## Workspace map

```
coil/
├── common/          # Value, archive (ARCHIVE_MAJOR/MINOR), host native table, StartupOp
├── parser/          # Pratt parser, AST
├── compiler/        # HM typechecker, HIR (src/hir/), MIR (src/mir/), codegen plan (src/codegen/), pipeline
├── machine/         # Runtime: driver, heap/GC, tasks, reactor, FFI, host natives, native ABI
├── coil-native/     # MIR to Cranelift: JIT, AOT objects
├── coil-mir-interp/ # MIR interpreter
├── coil-rt/         # Runtime library AOT executables link
├── coil-embed/      # JIT runner for packaged apps
├── coil-simd/       # Stable std::arch SIMD kernels (packed LA, bytes)
├── src/main.rs      # CLI: run, compile, test, debug, dissect
├── examples/        # Runnable .coil demos
├── tests/           # Integration .coil tests (coil test)
└── docs/            # Contributor internals (user docs: coil-website)
```

Pipeline detail: [docs/internals/pipeline.md](docs/internals/pipeline.md).

## Change routing

| Task | Primary location |
|------|------------------|
| Syntax / grammar | `parser/` |
| Types / diagnostics | `compiler/src/typechecking/` |
| Optimizations | HIR passes in `compiler/src/hir/`, MIR passes in `compiler/src/mir/` (`specialize::optimize_mir`) |
| Codegen | `compiler/src/codegen/` (`emit_hir.rs` plan), `compiler/src/mir/from_hir.rs` |
| A new MIR op | `compiler/src/mir/` (`inst.rs`, `text.rs`, `archive.rs`), `coil-native/src/lib.rs`, `coil-mir-interp/src/exec.rs`: both engines |
| Runtime behaviour | `machine/` (`driver.rs`, `runtime.rs`, `native.rs` helpers) |
| Archive format | `common/src/archive.rs`, `compiler/src/mir/archive.rs` |
| Host natives / virtual modules | `compiler/` (resolve), `machine/` (HostInvoke) |
| CLI | `src/main.rs` |

Single compilation path: HIR → MIR (`compiler/src/mir/from_hir.rs`, planned by `compiler/src/codegen/emit_hir.rs`). A body MIR cannot take is an internal error.

## Hard invariants (never skip)

1. **Archive version** — `ARCHIVE_MAJOR` / `ARCHIVE_MINOR` in `common/src/archive.rs`. An additive change bumps the minor; any other change to the format bumps the major (reset minor). Loaders accept the same major with archive minor ≤ runtime minor. A change to the MIR section's layout bumps `FORMAT` in `compiler/src/mir/archive.rs`; a reader takes the current format and the one before inline scopes (16) and refuses any other. Today: archive 8.1, MIR section format 17 (history in `docs/internals/archive-history.md`).
2. **Virtual module natives** — use `HostInvoke`, wired in `machine/`. Frozen ids are the `*_ID` consts in `common/src/host.rs`: never renumber them.
3. **MIR is what every engine runs** — a new op needs both engines, and they must agree: `coil test` and `coil test --engine mir`. No case-specific optimizations that only serve a bench (see AGENTS.md).
4. **New language features** — full HM integration + user-doc updates in [coil-website](https://github.com/ardax-corp/coil-website) (`src/content/docs/`) + internals here when needed + minimal runnable example. Prefer **method-based APIs** (`impl` methods on classes) over free functions for operations tied to a receiver type; free generic fns returning enums are codegen-fragile (see [limitations.md](docs/internals/limitations.md)).

See [reference.md](reference.md) and `docs/internals/pipeline.md`.

## Verification checklist

Local checks before pushing (CI runs the full suites; don't duplicate them):

```bash
cargo build --release
cargo test -p <crate> <filter>                  # unit tests nearest the change
./target/release/coil test path/to/touched.coil   # .coil tests you added or changed
cargo clippy --workspace --lib --tests --bins -- -D warnings   # lint gate; Gc::payload_mut allows mut_from_ref
cargo build -p coil-fmt && ./target/debug/coil-fmt --check examples tests compiler/src/prelude comptime/tests
./scripts/poop_baseline.sh       # perf work only: before/after (CPU counters on `coil run` of an archive, JIT included)
```

Full-suite commands (what CI runs) are listed in AGENTS.md "Commands".

| Work type | Extra check |
|-----------|-------------|
| Runtime / alloc | valgrind memcheck on debug build |
| A new MIR op or pass | `coil test --engine mir`, `scripts/mir_diff.py PATH` (every level and engine against raw MIR) |
| Perf | `./scripts/poop_baseline.sh` (precompiled `coil run` on mandelbrot/tak/nsieve/binary_trees). Build flagships with `--fold-calls off`, or call folding turns `main` into a constant. If an opt does not fire on them, add `examples/perf` hit benches and prove those: an identical flagship `.ccoil` is not a skip. Prefer alloc reduction. **No PGO.** |
| Debug info | `coil debug`, `coil dissect` |
| Formatting | `coil fmt` |
| Packaged apps | `coil package` links an AOT executable; `--no-aot` uses the `coil-embed` JIT runner |

## Feature work workflow

1. Draft plan for large language changes (do not edit attached plan files during impl).
2. Parser + AST if syntax changes.
3. Typechecker — new types/constraints/diagnostics (`E####` in coil-website `src/content/docs/language/error-codes.md`).
4. Codegen — HIR lowering to MIR (`mir/from_hir.rs`); new optimizations go in HIR or MIR passes.
5. `machine/` for runtime behaviour; both engines for a new MIR op.
6. coil-website `src/content/docs/` (manual/language; generated API pages in `src/content/reference/`; routes `/docs/…`) + example in `examples/` or `tests/`.
7. Bump archive **minor** for an additive format change, **major** for any other.
8. Granular conventional commits; stage only related files.

## Virtual modules (compiler-provided)

Auto: `prelude`, `prelude::ops`, `prelude::math`.

Explicit `use`: `ffi`, `io`, `thread`, `env`, `string`, `gc`, `clock`. Time calendar/format: [coil-time](https://github.com/ardax-corp/coil-time). Regex: [coil-regex](https://github.com/ardax-corp/coil-regex). TLS: [coil-tls](https://github.com/ardax-corp/coil-tls). Crypto: [coil-crypto](https://github.com/ardax-corp/coil-crypto).

Cargo default features are `[]`. There is no virtual `time` module; consume [coil-time](https://github.com/ardax-corp/coil-time). Process clocks are virtual `clock` (`wall_nanos` / `mono_nanos` / `sleep_ms` → HostInvoke `clock_*`). There is no virtual crypto module; hashes/AEAD live in [coil-crypto](https://github.com/ardax-corp/coil-crypto). The archive version lives in `common/src/archive.rs` (`ARCHIVE_MAJOR` / `ARCHIVE_MINOR`); older minors are in [archive-history.md](../../../docs/internals/archive-history.md). Frozen HostInvoke ids are the `*_ID` consts in `common/src/host.rs`. `PI`/`E`/`TAU` are [coil-stdlib `num`](https://github.com/ardax-corp/coil-stdlib/blob/main/docs/modules.md).

Prefer compiler builtins over userland for core type machinery.

**API shape:** inherent and trait methods over free functions for type-tied ops (`map.insert(k, v)` not `insert(map, k, v)`). Virtual-module free fns are fine for host primitives (`io::read`); stdlib and new language surface should default to methods.

## Useful tools

| Tool | Use |
|------|-----|
| `coil dissect file.coil --fn pat` | MIR dump without running (`--hir`, `--effects`, `--ast`, `--expand`) |
| `coil debug file.coil -x script --batch` | Debugger, on the MIR interpreter (see coil-debug skill) |
| `coil fmt [--check] path` | Pretty-print `.coil` (AST; keeps `//` / `///` comments) |
| `poop` | Instruction/HW counter baselines |
| `valgrind` | Leaks, callgrind |
| `heaptrack` | Allocation tracing |

## Additional resources

- Engines: [docs/internals/native.md](docs/internals/native.md), [docs/internals/mir-interp.md](docs/internals/mir-interp.md)
- Debug tables: [docs/internals/debug-info.md](docs/internals/debug-info.md)
- Crate/file detail: [reference.md](reference.md)
- Writing `.coil` programs: **coil-language** skill

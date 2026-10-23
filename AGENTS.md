# coil — AGENTS

coil: statically typed `.hy` → typecheck → HIR (optimizations) → MIR (every body) → dense or LIR bytecode → `.hyc` archive → custom VM.

| Need | Read |
|------|------|
| Write / edit `.hy` | `.cursor/skills/coil-language` · [coil-website](https://github.com/ardax-corp/coil-website) `src/content/docs/` (`/docs/…`) |
| Compiler, VM, pipeline | `.cursor/skills/coil-contributor` · `docs/internals/` |
| Hangs, panics, breakpoints | `.cursor/skills/coil-debug` · `docs/internals/debugger.md` |
| Known gaps / workarounds | `docs/internals/limitations.md` |

## User preferences

- Tests: `cargo test --workspace --lib --tests --bins` (required gate; covers integration tests, skips Criterion benches). Default features are empty and the tooling crates unify `dissect` / `debugger` / `coverage` into every workspace build, so `--no-default-features` / `--features <dissect|debugger>` build the same thing (CI runs one feature set). VM-wire suites: `cargo test -p compiler --features vm-wire --test pipeline --test perf_metrics --test namespace --test cursor_model`. Leak smoke: `cargo build --bin coil --bin coil-test && (ulimit -v 65536; ./target/debug/coil test -j 1)`. GC stress (precise-root soundness): `cargo build -p coil-test --features gc-stress --target-dir target/gc-stress && ./target/gc-stress/debug/coil-test`. Soft CPU: `./scripts/poop_baseline.sh`.
- Large tasks: scoped sub-agents on disjoint modules.
- VM perf: alloc reduction, hot-loop tuning, bounds-check elimination, `promise!` — not benchmark-shaped opcodes unless universal.
- **Hit-bench prove:** if an opt is sound but flagship `.hyc` (`mandelbrot` / `tak` / `nsieve` / `binary_trees` / `fib`) are identical, add focused `examples/perf` hit benches and prove those. Build flagship `.hyc` with `--fold-calls off`: `mandelbrot` / `tak` / `nsieve` otherwise compile `main` to a constant (comptime call folding), so the VM never runs them. Do not skip merge solely because flagships did not change; skip only on hit-bench wash/regress. Flagships remain controls. Landed: InstCombine (#304), try flatten (#307), LICM+integer SR (#315), TailCall (#316), Local CSE (#317), DestProp (#318), MIR InstCombine (#329), MIR DestProp (#330), MIR IV SR (#331), MIR cross-block GVN/PRE (`mir_gvn_divf`). The IL passes among these were removed or moved into HIR in 2026-10 (`compiler/src/il/opt/README.md`).
- Language features: draft plans; full HM; update coil-website user docs (`src/content/docs/`) and `docs/internals/` here when needed; minimal runnable example.
- **Method-based APIs** — prefer inherent/`impl` methods over free functions for type-tied operations (stdlib, new language surface, codegen fixes). Free generic fns returning enums are fragile today; see `docs/internals/limitations.md`.
- Granular conventional commits; stage only related files.
- Prefer compiler virtual modules over userland for core interpreter machinery; extracted features (regex, TLS, HTTP, collections) live in separate repos (`ardax-corp/coil-regex`, `coil-tls`, `coil-http`, `coil-stdlib`).
- **Userland package tests** — demos, native builds, and integration tests for extracted packages stay in their repos, not coil-lang `compiler/tests` or CI.
- **VM vs `.hy` tests** — prefer `.hy` language tests (`tests/<domain>/`, `coil test`) over Rust VM bytecode tests when coverage overlaps; remove duplicates.
- `cargo build` builds `coil` + `coil-debug` / `coil-dissect` / `coil-fmt` / `coil-lsp` / `coil-test` / `coil-verify` / `coil-embed`. `coil test` re-execs `coil-test` and `coil mutate` re-execs `coil-test mutate` (test-only VM features live there, never in `coil`). `coil-embed` is the packaged-app runner (`coil package` prefers it); not an embed-the-VM library.
- IL inspection: `coil dissect` — no verbose debug-build dumps.
- `coil fmt`: preserve `//` and `///`; wrap long lines; trailing commas on multi-line lists.

## Invariants (do not break)

- **Append-only opcodes** (`common/src/opcode.rs`). New variants at end → bump archive **minor**, `promise!` in `machine/src/vm.rs`, `instruction_from_u8_covers_last_appended_variant`. ABI break → **major** (reset minor).
- **Virtual-module natives** via `HostInvoke` — host wiring in `machine/`. Leftover TLS/crypto/regex slots were dropped (holes collapse); they are not reserved panic stubs. Virtual-time names stay as panic stubs so later ids do not move. `stream_attach` / `stream_park` own **119** / **120**. Process clocks: `clock_wall_nanos` / `clock_mono_nanos` / `clock_sleep_ms` are **121** / **122** / **123** (`use clock::{…}`). The archive version and the minors from 24 on are recorded only in `common/src/archive.rs` (`ARCHIVE_MINOR`): read them there; older minors are in [docs/internals/archive-history.md](docs/internals/archive-history.md).
- **No PGO.** Removed (#301). Branch layout is heuristic. Do not revive `--pgo-*`, ingest, heat knobs, or `BranchProfile`.
- **Feature gates**: debugger `feature = "debugger"`; dissect `feature = "dissect"`; coverage `feature = "coverage"` (`coil-test` only) — on helper binaries, not default `coil`. They gate hook *state and APIs* only: the VM dispatch loop is `execute::<HOOKS>`, instantiated with hooks only while a debugger / coverage collector is attached, so a binary that carries the features through workspace unification (a bare `cargo build --release`) pays nothing per instruction (#558). Release still builds `coil` / `coil-embed` in their own cargo invocation.
- **Lint gate**: `cargo clippy --workspace --lib --tests --bins -- -D warnings` (CI: Linux and windows-gnu). `Gc::payload_mut` keeps `&self -> &mut T` and allows `clippy::mut_from_ref` on that method only.
- **Fuse-select (D4)**: one named pass on typed `IlOp` after concat (`fuse_select` → PC assign). Residual `Byte` is a cold refuse. No post-lower `adjust_target`, no production per-fn fuse.

- **IL** is instruction lowering + label resolution + fuse — not a semantic IR. DefIds / typed sidecar hold names, types, and call meaning. See `docs/internals/pipeline.md` (IL intent).
- **Single emit sink**: production codegen pushes through `CodeBuf` / `IlBuilder` only. Encode in `il::lower`.

Codegen / match / `STORE`: `.cursor/skills/coil-contributor/reference.md`. Pipeline: `docs/internals/pipeline.md`.

## Cloud agents

Pre-installed: `poop`, `valgrind`, `heaptrack`, `hyperfine`, `lua` (`.cursor/Dockerfile`). Use `--release` for benchmarks.

## Learned User Preferences

- Aim for a regular language: one spelling per construct; do not add case-specific optimization workarounds that only serve a bench.
- Dual syntax: drop C-style `for (init; cond; step)` (keep `while` and `for x in`); length is `x.len()` only (the free `len(x)` was removed; `Length` is the trait a user type implements for it); `readonly` is a prefix on a value (`readonly e` for any expression, `readonly new C(...)`) and on a type (`readonly T` in parameters, returns, lets, fields and generic arguments); `readonly class C` makes every instance immutable (`readonly C` is `C`, structural `==`).
- Linear tickets: one PR per issue, base on `main`, babysit until CI is green and merged before starting the next.

## Learned Workspace Facts

- Trait methods have three call spellings: `x.m()`, `m(x)` and `Trait::m(x)`. The qualified one is the function form limited to that trait (it disambiguates same-named methods) and lowers to the same call (`infer_qualified_trait_call`, `qualified_trait_call_at`; see `docs/internals/static-trait-methods.md`).
- Regularity target: one ground-call convention, panic on OOB (not `-1` / no-op); `a[i]` stays type `T`. `Option`/`Result` use niche / two-slot / boxed by shape (COI-92); arity-2 immediate products use two-slot `[a, b]` on direct `CALL`/`RETURN` (#302). Fuse opcodes are debt — rewrite existing ops rather than growing bench-shaped fuses.

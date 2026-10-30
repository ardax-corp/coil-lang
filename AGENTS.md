# coil — AGENTS

coil: statically typed `.hy` → typecheck → HIR (optimizations) → MIR (every body) → dense or LIR bytecode → `.hyc` archive → custom VM.

| Need | Read |
|------|------|
| Write / edit `.hy` | `.claude/skills/coil-language` · [coil-website](https://github.com/ardax-corp/coil-website) `src/content/docs/` (`/docs/…`) |
| Compiler, VM, pipeline | `.claude/skills/coil-contributor` · `docs/internals/` |
| Hangs, panics, breakpoints | `.claude/skills/coil-debug` · `docs/internals/debugger.md` |
| Known gaps / workarounds | `docs/internals/limitations.md` |

## Working agreement

- **Verify locally, let CI run the suites.** Before pushing: a release build, the tests nearest the change (`cargo test -p <crate> <filter>`, `coil test <file>.hy`; add `--features vm-wire` when touching compiler behaviour, see Commands), clippy and the fmt check below. Don't run the full workspace suite, the leak smoke or GC stress locally as a gate; push and fix what CI reports.
- **Before push:** `cargo clippy --workspace --lib --tests --bins -- -D warnings` and `cargo build -p coil-fmt && ./target/debug/coil-fmt --check examples tests compiler/src/prelude comptime/tests` (CI's `fmt` job also checks `compile_fail/` and `scripts/rep_matrix_known_wrong`).
- **Bug fixes:** the first commit adds a test that reproduces the issue, then the fix commits. Commit in small, granular steps (conventional commits, only related files staged); PRs are squash-merged.
- **Bugs found along the way:** don't widen the PR. File an issue in `ardax-corp/coil-lang` (search for duplicates first) with a minimal `.hy` repro, exact commands and output, expected vs actual, commit sha and a suspected cause; link it from the PR if related. Fix it in place only when the current change can't land without it.
- **PRs:** issues live on GitHub. Independent changes may go out as parallel PRs; stacked or conflicting ones one at a time. Merge your own PR once CI is green, except macOS jobs, which may still be queued (a macOS job that actually failed still needs a look). Cross-repo chains merge upstream first: coil-lang → coil-stdlib → libs → coil-website. A red check that is a known, already-filed flake does not block: comment naming the issue and merge.
- **Large features:** a long-lived `feature/<name>` branch with one PR per plan step; local verification gates the step PRs, and full CI runs on the final squash-merged PR into `main`. Draft a plan for large language changes and get it agreed before implementing.
- **Parallel work:** scoped sub-agents on disjoint modules are fine without asking.
- **Tests never pin emitted bytecode** (hashes, goldens, exact op lines). Assert behaviour; opcode presence/absence checks only where the opt is the point.
- **Perf changes:** prove on a bench the rewrite changes. Flagships (`mandelbrot` / `tak` / `nsieve` / `binary_trees` / `fib`) must be built with `--fold-calls off`, or comptime call folding turns `main` into a constant. If an opt is sound but the flagship `.hyc` are identical, add focused `examples/perf` hit benches and prove those; skip only on hit-bench wash/regress (rule: `docs/internals/optimization-roadmap.md`, "Hit-bench prove rule"). Use `--release` for benchmarks.

## Style and direction

- Aim for a regular language: one spelling per construct; no case-specific optimizations or opcodes that only serve a bench. Fuse opcodes are debt: rewrite existing ops rather than growing bench-shaped fuses.
- VM perf: alloc reduction, hot-loop tuning, bounds-check elimination, `promise!`.
- **Method-based APIs**: prefer inherent/`impl` methods over free functions for type-tied operations (stdlib, new language surface, codegen fixes). Virtual-module host primitives (`io::read`) stay free functions.
- Language features: full HM integration; update coil-website user docs (`src/content/docs/`) and `docs/internals/` here when needed; minimal runnable example.
- Prefer compiler virtual modules over userland for core interpreter machinery; extracted features (regex, TLS, HTTP, collections) live in their own repos (`ardax-corp/coil-regex`, `coil-tls`, `coil-http`, `coil-stdlib`). Their demos, native builds and integration tests stay in those repos, not in coil-lang `compiler/tests` or CI.
- Prefer `.hy` language tests (`tests/<domain>/`, `coil test`) over Rust VM bytecode tests when coverage overlaps; remove duplicates.
- IL inspection: `coil dissect`, not verbose debug-build dumps.
- `coil fmt`: preserve `//` and `///`; wrap long lines; trailing commas on multi-line lists.

## Invariants (do not break)

- **Append-only opcodes** (`common/src/opcode.rs`). New variants at end → bump archive **minor**, `promise!` in `machine/src/vm.rs`, `instruction_from_u8_covers_last_appended_variant`. ABI break → **major** (reset minor). The current version is only in `common/src/archive.rs` (`ARCHIVE_MAJOR` / `ARCHIVE_MINOR`); older minors are in [docs/internals/archive-history.md](docs/internals/archive-history.md).
- **Virtual-module natives** via `HostInvoke`, wired in `machine/`. Frozen ids are the `*_ID` consts in `common/src/host.rs`: never renumber them. Dropped TLS/crypto/regex slots collapsed; virtual-time names stay as panic stubs so later ids do not move.
- **No PGO.** Removed (#301). Branch layout is heuristic. Do not revive `--pgo-*`, ingest, heat knobs, or `BranchProfile`.
- **Feature gates**: debugger `feature = "debugger"`; dissect `feature = "dissect"`; coverage `feature = "coverage"` (`coil-test` only), on helper binaries, not default `coil`. They gate hook *state and APIs* only: the VM dispatch loop is `execute::<HOOKS>`, instantiated with hooks only while a debugger / coverage collector is attached, so a binary that carries the features through workspace unification pays nothing per instruction (#558). Release still builds `coil` / `coil-embed` in their own cargo invocation.
- **Lint**: `Gc::payload_mut` keeps `&self -> &mut T` and allows `clippy::mut_from_ref` on that method only.
- **Fuse-select (D4)**: one named pass on typed `IlOp` after concat (`fuse_select` → PC assign). Residual `Byte` is a cold refuse. No post-lower `adjust_target`, no production per-fn fuse.
- **IL** is instruction lowering + label resolution + fuse, not a semantic IR. DefIds / typed sidecar hold names, types, and call meaning. New optimizations go in HIR or MIR passes, never on the stack IL. See `docs/internals/pipeline.md`.
- **Single emit sink**: production codegen pushes through `CodeBuf` / `IlBuilder` only. Encode in `il::lower`.

Codegen / match / `STORE`: `.claude/skills/coil-contributor/reference.md`.

## Binaries

`cargo build` builds `coil` + `coil-debug` / `coil-dissect` / `coil-fmt` / `coil-lsp` / `coil-test` / `coil-verify` / `coil-embed`. `coil test` re-execs `coil-test` and `coil mutate` re-execs `coil-test mutate` (test-only VM features live there, never in `coil`). `coil-embed` is the packaged-app runner (`coil package` prefers it), not an embed-the-VM library.

## Commands (reference; CI runs these)

- Full suite: `cargo test --workspace --lib --tests --bins` (use `--lib --tests --bins`, not `--all-targets`: Criterion benches abort on test argv). Default features are empty and the tooling crates unify `dissect` / `debugger` / `coverage`, so feature variants build the same thing; one run covers them.
- VM-wire suites (`required-features = ["vm-wire"]`, skipped silently otherwise): `cargo test -p compiler --features vm-wire --tests`.
- `.hy` corpus: `./target/debug/coil test` (or a path).
- Leak smoke and GC stress: the `harness` matrix in `.github/workflows/ci.yml` (`scripts/test_corpora.py`; leak smoke is `-j 1 --limit-as-kb <code size + 128 MiB>`, GC stress is `coil-test` built with `--features gc-stress --target-dir target/gc-stress`, release cells only since #2131).
- Soft CPU baseline: `./scripts/poop_baseline.sh`.
- Under cargo the reactor runs one worker (`COIL_MAX_WORKER_THREADS=1` in `.cargo/config.toml`); override it when profiling auto-par.

## Stdlib

Userland stdlib is [coil-stdlib](https://github.com/ardax-corp/coil-stdlib). `coil.toml` module roots look for it at `./.deps/coil-stdlib/src` or `../coil-stdlib/src`; clone it to one of those. The `coil` CLI never reads `coil.toml` itself (spool turns it into flags); without spool pass `--root` yourself.

## Language facts agents get wrong

- Trait methods have three call spellings: `x.m()`, `m(x)` and `Trait::m(x)`. The qualified one is the function form limited to that trait and lowers to the same call (`docs/internals/static-trait-methods.md`).
- No C-style `for (init; cond; step)` (use `while` and `for x in`); length is `x.len()` only (`Length` is the trait a user type implements for it); `readonly` is a prefix on a value (`readonly e`, `readonly new C(...)`) and on a type (`readonly T`); `readonly class C` makes every instance immutable (`readonly C` is `C`, structural `==`).
- Regularity target: one ground-call convention, panic on OOB (not `-1` / no-op); `a[i]` stays type `T`. `Option`/`Result` use niche / two-slot / boxed by shape; arity-2 immediate products use two-slot `[a, b]` on direct `CALL`/`RETURN` (#302).

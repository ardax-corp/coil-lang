# coil — AGENTS

coil: statically typed `.coil` → typecheck → HIR (optimizations) → MIR (every body) → native code (Cranelift, JIT or AOT), or the MIR interpreter for compile-time code, `coil debug` and wasm. A `.ccoil` archive holds MIR and data.

| Need | Read |
|------|------|
| Write / edit `.coil` | `.claude/skills/coil-language` · [coil-website](https://github.com/ardax-corp/coil-website) `src/content/docs/` (`/docs/…`) |
| Compiler, runtime, pipeline | `.claude/skills/coil-contributor` · `docs/internals/` |
| Hangs, panics, breakpoints | `.claude/skills/coil-debug` · `docs/internals/debugger.md` |
| Known gaps / workarounds | `docs/internals/limitations.md` |

## Working agreement

- **Verify locally, let CI run the suites.** Before pushing: a release build, the tests nearest the change (`cargo test -p <crate> <filter>`, `coil test <file>.coil`), clippy and the fmt check below. Don't run the full workspace suite, the leak smoke or GC stress locally as a gate; push and fix what CI reports.
- **Before push:** `cargo clippy --workspace --lib --tests --bins -- -D warnings` and `cargo build -p coil-fmt && ./target/debug/coil-fmt --check examples tests compiler/src/prelude comptime/tests` (CI's `fmt` job also checks `compile_fail/` and `scripts/rep_matrix_known_wrong`).
- **Bug fixes:** the first commit adds a test that reproduces the issue, then the fix commits. Commit in small, granular steps (conventional commits, only related files staged); PRs are squash-merged.
- **Bugs found along the way:** don't widen the PR. File an issue in `ardax-corp/coil-lang` (search for duplicates first) with a minimal `.coil` repro, exact commands and output, expected vs actual, commit sha and a suspected cause; link it from the PR if related. Label engine bugs for triage: `vm` (the MIR interpreter and the runtime), `jit` (JIT runs, archive MIR sections) or `native` (the Cranelift backend in general, AOT `coil package`). Fix it in place only when the current change can't land without it.
- **PRs:** issues live on GitHub. Independent changes may go out as parallel PRs; stacked or conflicting ones one at a time. Merge your own PR once CI is green, except macOS jobs, which may still be queued (a macOS job that actually failed still needs a look). Cross-repo chains merge upstream first: coil-lang → coil-stdlib → libs → coil-website. A red check that is a known, already-filed flake does not block: comment naming the issue and merge.
- **Large features:** a long-lived `feature/<name>` branch with one PR per plan step; local verification gates the step PRs, and full CI runs on the final squash-merged PR into `main`. Draft a plan for large language changes and get it agreed before implementing.
- **Parallel work:** scoped sub-agents on disjoint modules are fine without asking.
- **Tests never pin emitted code** (hashes, goldens, exact MIR lines). Assert behaviour; op presence/absence checks only where the opt is the point.
- **Perf changes:** prove on a bench the rewrite changes. Flagships (`mandelbrot` / `tak` / `nsieve` / `binary_trees` / `fib`) must be built with `--fold-calls off`, or comptime call folding turns `main` into a constant. If an opt is sound but the flagship `.ccoil` are identical, add focused `examples/perf` hit benches and prove those; skip only on hit-bench wash/regress (rule: `docs/internals/optimization-roadmap.md`, "Hit-bench prove rule"). Use `--release` for benchmarks.

## Style and direction

- Aim for a regular language: one spelling per construct; no case-specific optimizations that only serve a bench.
- Runtime perf: alloc reduction, hot-loop tuning, bounds-check elimination.
- **Method-based APIs**: prefer inherent/`impl` methods over free functions for type-tied operations (stdlib, new language surface, codegen fixes). Virtual-module host primitives (`io::read`) stay free functions.
- Language features: full HM integration; update coil-website user docs (`src/content/docs/`) and `docs/internals/` here when needed; minimal runnable example.
- Prefer compiler virtual modules over userland for core runtime machinery; extracted features (regex, TLS, HTTP, collections) live in their own repos (`ardax-corp/coil-regex`, `coil-tls`, `coil-http`, `coil-stdlib`). Their demos, native builds and integration tests stay in those repos, not in coil-lang `compiler/tests` or CI.
- Prefer `.coil` language tests (`tests/<domain>/`, `coil test`) over Rust tests when coverage overlaps; remove duplicates.
- MIR / HIR inspection: `coil dissect` (`--fn`, `--hir`), not verbose debug-build dumps.
- `coil fmt`: preserve `//` and `///`; wrap long lines; trailing commas on multi-line lists.

## Invariants (do not break)

- **Archive version**: `common/src/archive.rs` (`ARCHIVE_MAJOR` / `ARCHIVE_MINOR`). An additive change bumps the minor; any other change to the format the major (reset minor). Today that is 8.1 (`ARCHIVE_MAJOR` 8, `ARCHIVE_MINOR` 1). The MIR section has a format of its own, `FORMAT` in `compiler/src/mir/archive.rs` (17, from `common::MIR_SECTION_SCOPES_FORMAT`; the reader also takes 16). Older minors and the history of the major are in [docs/internals/archive-history.md](docs/internals/archive-history.md); a format change adds its entry there.
- **Virtual-module natives** via `HostInvoke`, wired in `machine/`. Frozen ids are the `*_ID` consts in `common/src/host.rs`: never renumber them. Dropped TLS/crypto/regex slots collapsed; virtual-time names stay as panic stubs so later ids do not move.
- **No PGO.** Removed (#301). Branch layout is heuristic. Do not revive `--pgo-*`, ingest, heat knobs, or `BranchProfile`.
- **Feature gates**: dissect `feature = "dissect"` (`compiler`), on helper binaries, not default `coil`; `gc-stress` / `gc-stats` (`machine`, through `coil-test`) for GC testing only. Release still builds `coil` / `coil-embed` in their own cargo invocation.
- **Lint**: `Gc::payload_mut` keeps `&self -> &mut T` and allows `clippy::mut_from_ref` on that method only.
- **MIR** is what every engine runs. New optimizations go in HIR or MIR passes. See `docs/internals/pipeline.md`.

Codegen / match: `.claude/skills/coil-contributor/reference.md`.

## Binaries

`cargo build` builds `coil` + `coil-debug` / `coil-dissect` / `coil-fmt` / `coil-lsp` / `coil-test` / `coil-verify` / `coil-embed`. `coil test` re-execs `coil-test` and `coil mutate` re-execs `coil-test mutate` (test-only features live there, never in `coil`). `coil-embed` is the JIT runner for packaged apps (`coil package --no-aot`).

## Commands (reference; CI runs these)

- Full suite: `cargo test --workspace --lib --tests --bins` (use `--lib --tests --bins`, not `--all-targets`: Criterion benches abort on test argv). CI runs its two corpus-wide tests, `hir_corpus` and `fmt_round_trip`, in a separate `test corpus` job. Default features are empty and the tooling crates unify `dissect` / `debugger`, so feature variants build the same thing; one run covers them.
- `.coil` corpus: `./target/debug/coil test` (or a path). `--engine native|mir`; `COIL_NO_FIBERS=1 --engine mir` runs generators and tasks on parked frames, as wasm does; `COIL_CODE_IMAGE_CHECK=1 --engine native` runs each program from a code image, as a code cache hit does.
- Configuration cells (`harness clone-budget-0` in CI): `scripts/test_corpora.py --release -- --engine native|mir --clone-budget 0 --cell clone-budget-0 <grants and roots>`. `--cell NAME` turns on the `// @xfail NAME #issue` header lines: a listed file must fail there, and fails the cell once it passes (`docs/internals/test-runner.md`, "Configuration cells and known failures").
- `COIL_MIR_VERIFY=1` (any command that compiles; CI's `harness mir-verify` and `mir-opt` cells): the MIR verifier on every body as lowered and after each MIR pass in a release build too, about 1% slower. A violation panics with the pass and the function. Debug builds verify after each pass without it.
- Auto-par on against off (CI's `harness auto-par`): `scripts/auto_par_diff.py --release -- <grants and roots>` runs every program with a `main` (corpus, `examples/`, `examples/perf/`) with `--auto-par on` and `off` on 8 workers and compares stdout, stderr and the exit code. `// @skip auto-par <why>` for output that depends on time or scheduling; `// @xfail auto-par #issue` for a filed difference.
- Leak smoke and GC stress: the `harness-*` jobs in `.github/workflows/ci.yml` (`scripts/test_corpora.py`; leak smoke is `MALLOC_ARENA_MAX=1`, `-j 1 --limit-as-kb <code size + 256 MiB>`, GC stress is `coil-test` built with `--features gc-stress --target-dir target/gc-stress`, release cells only since #2131).
- Soft CPU baseline: `./scripts/poop_baseline.sh`.
- Under cargo the reactor runs one worker (`COIL_MAX_WORKER_THREADS=1` in `.cargo/config.toml`); override it when profiling auto-par.

## Stdlib

Userland stdlib is [coil-stdlib](https://github.com/ardax-corp/coil-stdlib). `coil.toml` module roots look for it at `./.deps/coil-stdlib/src` or `../coil-stdlib/src`; clone it to one of those. The `coil` CLI never reads `coil.toml` itself (spool turns it into flags); without spool pass `--root` yourself.

## Language facts agents get wrong

- Trait methods have two call spellings: `x.m()` and `Trait::m(x)` (the receiver first). Both resolve and lower the same way; a bare `m(x)` is not a trait call (E0153), also inside impl and trait default bodies (`docs/internals/static-trait-methods.md`).
- No C-style `for (init; cond; step)` (use `while` and `for x in`); length is `x.len()` only (`Length` is the trait a user type implements for it); `readonly` is a prefix on a value (`readonly e`, `readonly new C(...)`) and on a type (`readonly T`); `readonly class C` makes every instance immutable (`readonly C` is `C`, structural `==`).
- Regularity target: one ground-call convention, panic on OOB (not `-1` / no-op); `a[i]` stays type `T`. `Option`/`Result` use niche / two-slot / boxed by shape; arity-2 immediate products use two words `[a, b]` on direct calls (#302; MIR `Call` `dest_hi`, `Terminator::Return { hi }`).

# Test health report (2026-07-18)

> **Historical snapshot** — see CI for current test status. For open issues see [limitations.md](limitations.md).

Investigation of `main` after PRs #8–#17. This document records what was
broken, what looked flaky, incomplete implementations, and fixes applied.

## Broken tests

| Test | Symptom | Origin |
|------|---------|--------|
| `example_derive_show_eq_prints_expected` (`compiler/tests/pipeline.rs`) | Compile fails with E0119: `Ord` for `Color` requires `Lt`/`Le`/`Gt`/`Ge`; unknown methods `lt`/`le`/`gt`/`ge` on `Ord` | PR **#15** (`b34c15d`) header `derive Ord` expanded to a single `impl Ord` carrying comparison methods. PR **#14** (`84fd341`) had already split comparisons into `Lt`/`Le`/`Gt`/`Ge` with **empty** `Ord` as a convenience supertrait (see `compiler/src/typechecking/generics.rs`). |

**Fix:** `synth_ord_enum` / `synth_ord_class` in `compiler/src/derive.rs` now emit five synthetic impls (`Lt`, `Le`, `Gt`, `Ge`, empty `Ord`). Regression: `derive_ord_record_payload_lexicographic_compare`.

## Slow tests (not flaky)

| Path | Issue | Fix |
|------|-------|-----|
| `examples/fib.hy` used `fib(32)` | Millions of recursive calls; debug heap alloc traces made `example_fib_still_works` and shared suite wall time drag | Smoke example uses `fib(10)` → `55`; fair CPU baselines are `examples/perf/{mandelbrot,tak,nsieve,binary_trees}.hy` (`poop` / `vm_bench.sh` / `perf_mandelbrot_dispatch_regression`) |

## Flaky tests

| Test | Symptom | Root cause | Fix |
|------|---------|------------|-----|
| `example_derive_show_eq_prints_expected` / `tests/derive_ord.hy` | `Color::Red < Color::Blue` intermittently `false` (ASLR-dependent) | Concrete `<`/`>`/`<=`/`>=` codegen looked up empty `Ord` (no methods) and fell back to hardwired `LE`/`GT`/… which compare **heap pointer addresses** | Emit via `Lt`/`Gt`/`Le`/`Ge` in `emit_concrete_operator_call` (`compiler/src/lib.rs`). Regression: `derive_ord_unit_variants_compare_by_declaration_order` (8× stable). |

Namespace suite (`compiler/tests/namespace.rs`) passed repeatedly under `--test-threads=16`. Residual risks (not failing today):

- Process-wide `CWD_LOCK` + `chdir` for `coil.toml` discovery
- Shared `examples/libsum.so` build among FFI tests (must not truncate with `File::create`)

## Incomplete / false-green patterns

| Pattern | Risk | Mitigation |
|---------|------|------------|
| FFI tests `eprintln!("skipping…"); return;` when `cc` / `.so` / `libc` missing | CI can go green without exercising FFI | Soft-skip **panics when `CI` is set**; GitHub Actions installs `libffi-dev` + `build-essential` |
| No `.github/workflows` before this work | Regressions like #15 Ord derive landed unnoticed | Added `.github/workflows/ci.yml` |
| CLI `out.hyc` cache | Stale bytecode on manual runs (not pipeline goldens) | **Doc was wrong:** default `coil file.hy` does not read/write `out.hyc`; `coil run out.hyc` warns, does not rebuild. See [pipeline.md](pipeline.md). |

## Coverage (post edge-case expansion)

`cargo llvm-cov --workspace` region / line coverage by crate:

| Crate | Regions | Lines |
|-------|---------|-------|
| common | 84.7% | 84.5% |
| compiler | 83.3% | 84.1% |
| machine | 78.4% | 78.9% |
| parser | 84.9% | 86.9% |
| reporting | 92.6% | 94.0% |
| CLI (`src/main.rs`) | 62.6% | 61.5% |
| **Average (6 crates)** | **~81%** | **~82%** |
| Workspace TOTAL | 82.6% | 83.5% |

Target band was 60–80% average; suites now sit at the high end / slightly above, with CLI as the previous gap (lifted from ~35% via archive/mtime/`collect_test_files` unit tests).

### Edge-case suites added

- **CLI:** `try_load_archive` (missing/corrupt/version/ok), mtime recompile heuristic, nested `collect_test_files`, stricter `parse_args`
- **Diagnostics goldens:** FFI/IO without import, declare/invoke arity, array/tuple OOB, index-on-int, record pattern missing/dup/shape, const assign, panic non-string, array element mismatch
- **VM defensive:** JumpIfMatch/Unpack/UnpackAt/LoadField on non-enum, empty-stack JumpIfMatch, GetField miss → `-1`, DoneCoro edges
- **Parser negatives:** unclosed constructs, bad `use`/`mod`/`match`, `(1)` vs `(1,)`
- **Reporting:** exhaustive `ErrorCode` uniqueness; `create_sink` / `emit_all`
- **common:** opcode pack/unpack, archive round-trip, ArrayVec spill/iter, SeekableIterator

### Bugs found by edge coverage

- `ArrayVec::push` had `promise!(offset >= N)` that fired on first heap spill (UB in release)
- `SeekableIterator::next` asserted before honoring Iterator exhaustion

## Coverage follow-ups applied

- In-tree `proptest` property tests (parser no-panic; small-program compile no-panic)
- Extra `./tests/*.hy` exercised by `coil test` (derive Ord, assert edges)
- GitHub Actions: `cargo test --workspace` + `cargo run -- test`

## Out of scope (still known)

- Documented `examples/strlen.hy` CLI segfault path (pipeline golden passes)
- Commented-out allocator unit tests in `machine/src/memory/allocator.rs`
- Overnight `cargo-fuzz` / libFuzzer corpus

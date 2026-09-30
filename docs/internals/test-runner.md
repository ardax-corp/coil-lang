# Test runner

`coil test` runs the language harness. The main `coil` binary **re-execs** the
sibling `coil-test` helper (git-style, like `coil debug` / `coil dissect`) and
forwards every flag unchanged. `coil test` is the documented spelling;
`coil-test` can also be invoked directly (CI's GC-stress job does) and takes
the same flags with the same output.

```bash
cargo build              # coil + coil-test (+ other helpers)
coil test                # ./tests
coil test tests/positive --fail-fast -O0
coil test --root examples/src --root .deps/coil-stdlib/src
coil-test --help         # same flags as `coil test --help`
```

## Why a separate binary

Test-only VM mechanisms (coverage maps, a lower step budget, per-test output
capture) are Cargo features on `machine`. Only `coil-test` enables them, so
`coil` and `coil-embed` never compile them: no runtime flags or `Option`
checks in the plain VM. `coil-test` also forwards `gc-stress` / `gc-stats`.

Host wiring (`Pipeline` → `Machine`: FFI paths, dload gate, thread program)
lives in the `coil-host` crate, shared by `coil` and `coil-test`.
`coil-embed` runs archives only and does not link it.

## What it runs

| Path | Expectation |
|------|-------------|
| any `.hy` under a `compile_fail/` segment | the compiler must reject it with a diagnostic (a compiler panic is a failure) |
| a file with `test("…") { … }` / `#[test] fn` cases | each case runs on a fresh `Machine`: static init, then the case; it fails on `panic` or an `Err` return |
| a file without cases | `main` runs once as a single opaque case |

Each file compiles in memory with `Pipeline::set_include_tests(true)` at the
requested `-O` level (default Standard, same as `coil compile`). Nothing is
serialized to `.hyc`, so test-only data never touches the archive format.

## Layout

| File | Role |
|------|------|
| `coil-test/src/args.rs` | argv (`--fail-fast`, `-O`, `--root`, host grants, `--log-*`) |
| `coil-test/src/runner.rs` | discovery, per-file compile, per-case VM, summary |
| `coil-host/src/lib.rs` | `wire_pipeline_vm`, `wire_pipeline_threads`, `execute_pipeline` |

Plan for parallel runs, coverage and mutation testing: the *coil-test:
Coverage, Parallel Runner & Mutation Testing* design doc.

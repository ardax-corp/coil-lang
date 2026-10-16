# Test runner

`coil test` runs the language harness. The main `coil` binary **re-execs** the
sibling `coil-test` helper (git-style, like `coil debug` / `coil dissect`) and
forwards every flag unchanged. `coil test` is the documented spelling;
`coil-test` can also be invoked directly (CI's GC-stress job does) and takes
the same flags with the same output.

```bash
cargo build              # coil + coil-test (+ other helpers)
coil test                # ./tests
coil test tests/patterns --fail-fast -O0
coil test --root examples/src --root .deps/coil-stdlib/src
coil-test --help         # same flags as `coil test --help`
```

## Why a separate binary

Test-only VM mechanisms (coverage maps, per-test output capture) are Cargo
features on `machine`. Only `coil-test` enables them, so
`coil` and `coil-embed` never compile them: no runtime flags or `Option`
checks in the plain VM. `coil-test` also forwards `gc-stress` / `gc-stats`.
The step budget (`Machine::set_step_budget`) is not one of them: the VM has
it anyway (it also bounds compile-time evaluation); a test job sets it per case
(`TestCase::step_budget`) and reports `steps` / `timed_out`.

Host wiring (`Pipeline` → `Machine`: FFI paths, dload gate, thread program)
lives in the `coil-host` crate, shared by `coil` and `coil-test`.
`coil-embed` runs archives only and does not link it.

## What it runs

| Path | Expectation |
|------|-------------|
| any `.hy` under a `compile_fail/` segment | the compiler must reject it with one of the error codes its header declares (`// Expected: E0209 — why`; several: `E0410 or E0409`). A rejection for another reason, a missing declaration, or a compiler panic is a failure |
| a file with `test("…") { … }` / `#[test] fn` cases | each case is a reactor job: static init, then the case; it fails on `panic` or an `Err` return |
| a file without cases | `main` runs once as a single opaque case |
| a function with `requires` / `ensures` (in a test file, or in a project source a test file imports) | a generated `contract: f` case calls it with random arguments (`--contract-runs N`, default 100, 0 off); see [contracts](contracts.md#generated-tests) |
| a code example in the `///` docs of a test file, or of a project source a test file imports | a `doc: module::item (file:line)` case compiles and runs it as its own program; see [doc-tests](#doc-tests) |

### `env::exit` in a case

Every case runs inside the harness process, so `env::exit` (granted with
`--allow-exit`) must not exit it: an `exit(0)` would end the run early with
status 0 and green CI (#1401). `run_test_suite` calls
`machine::env::trap_exit(true)`; from then on the `env_exit` native records
its code on the thread and raises a VM panic
(`env::exit(N) ends only this job under the test harness`), so the job's
`defer`s run and the harness goes on with the next case.

| Where `exit(N)` runs | Verdict |
|----------------------|---------|
| a `test` case (or its static init) | fails, whatever `N`: the rest of the body never ran |
| a case-less file's `main` | passes when `N == 0` (a program's clean end), fails otherwise |
| a `thread::spawn` job the case started | that job panics on its own VM (its join fails); the case sees only the failed join |

A test that needs a real exit status runs the program as a subprocess.

Outside `coil test` (`coil --include-tests tests/foo.hy`), a test file
with no `main` gets one that runs every case in one VM, prints
`> Test "<name>" failed` for each `Err`, and panics with `tests failed` if
any did. The compiler builds that `main` as HIR
([`hir::synth`](../../compiler/src/hir/synth.rs)) and lowers it like any
other body; its `PRINT` is the HIR builtin `Print`, which keeps it off
direct MIR.

A case body is checked in `Result<(), string>` mode, but its `?` is
looser than a function's (#628). The checker records each such site in
`Checker::test_try_sites`. A `Result` whose error type has `Show` but is
not `string`, and any `Option`, lower in `emit_test_try`. That function
returns ``Err("`?` got Err(<shown>)")`` or ``Err("`?` got None")`` instead
of unifying with the case's error type. Helper functions and lambdas
inside a case keep the ordinary `?` rules (`in_test_case` is reset).

## Doc-tests

The code examples in `///` comments run as test cases in the same run and
report (`--no-doc-tests` turns them off), the way `cargo test` runs Rust
doc-tests. The rules are `parser::doc_examples`, which the language server's
hover also uses:

- A fence opened with ```` ``` ```` or ```` ```coil ```` is an example; other
  languages (```` ```text ````) are not. Tags, comma-separated: `ignore` (not
  compiled, not reported), `no_run` (compiled only), `should_panic` (passes
  only when the program panics).
- `# ` lines (and a lone `#`) are hidden on hover but run.
- `use` lines are hoisted; an example without `fn main` is wrapped in
  `fn main() { … }`. The wrapper's error type is inferred from the body, so
  an example cannot mix `?` on an `IoError` with `assert!` (a `string`):
  `match` instead, or write the example's own `fn main`.
- An example passes when it compiles and runs without a panic. An `Err`
  reaching the end of `main` (a failed `assert!`, an uncaught `raise`)
  panics (#1353), so it fails the example too.

**Which docs.** Every documented function, method, class, field, enum,
variant, trait, trait method, type alias and macro of each test file (not
`compile_fail/` ones), then, after the suite (with the contract tests), of
every project source a test file imports: under the current directory,
outside the test root and `.deps/`. Dependencies under `--root` elsewhere
(a coil-stdlib checkout) are not doc-tested; run their examples with
`--doc-tests-only <dir>`.

**Naming.** `doc: <module>::<item> (<file>:<line>)`, the line of the opening
fence. The module path is the file's path under the deepest of the test
root, the `--root` directories and `src` that holds it (`src/io/sync.hy`
→ `io::sync::println`). The cases are part of their file's `file` event under
`--json` and are shuffled with the seed like `test` cases.

**Isolation.** Each example compiles as its own program, with the run's
roots, grants and `-O`, as an in-memory file next to the one it documents
(`<stem>__doc_<line>.hy`, the name in its diagnostics and panic locations),
and runs as a reactor job like a file's `main`. A compile error fails the
case with the diagnostics as its output. Doc-tests are not counted for
`--coverage` and `coil mutate` does not run them.

**`--doc-tests-only [PATH]`** parses every file under PATH for examples
without compiling it. CI runs the builtin doc stubs this way, since no test
file imports them:

```bash
coil test --doc-tests-only -A --root .deps/coil-stdlib/src compiler/src/builtin_docs
```

An example that ends the process (`env::exit`) must be `no_run`: cases run
inside the harness process.

## Order

Files and the cases inside each file run in a **seeded random order** by
default, so tests that silently depend on each other (usually through host
state: files, env, cwd, ports) show up. The header prints the seed:

```text
running 252 files (seed 0x5eed, 4 jobs)
…
test result: FAILED. 628 passed; 1 failed; 629 total
rerun in this order with `--seed 0x5eed`
```

The process **exits 1** whenever that summary is `FAILED` (and 0 when it is
`ok`). Callers (`coil test` re-exec, CI) must use that status; a red summary
with exit 0 would green the job.

| Flag / env | Effect |
|------------|--------|
| `--seed N` | shuffle with `N` (decimal or `0x` hex) |
| `COIL_TEST_SEED=N` | same, when `--seed` is absent |
| `--no-shuffle` | sorted paths, cases in source order (cannot combine with `--seed`) |

The shuffle is two-level: files first, then each file's cases with a seed
derived from the run seed and the file's path under the test root. A file
keeps its case order when rerun alone with the same seed. Files are shuffled
rather than a flat list of all cases so only one compiled file is in memory at
a time (the leak smoke runs under `ulimit -v 65536`). splitmix64 +
Fisher–Yates live in `coil-test/src/order.rs`; there is no `rand` dependency.

## Parallel runs

Each case runs as a job on the CPU reactor (`machine/src/reactor.rs`,
[IO reactor](io-reactor.md)): `Reactor::submit_test` queues it, a worker VM
runs the file's static-init prologue (its `JMP main` patched to `HALT`), then
calls the case; `TestHandle::wait` returns a `TestReport` (passed, and the
`Err` text of a failing `assert`). `-j N` (default: available CPUs) sets the
worker count and the number of compile threads.

| `-j` | Compile | Cases |
|------|---------|-------|
| `1` | runner thread | inline on the runner thread (`Reactor::run_test_here`, the same job path) |
| `N > 1` | `N` threads, at most `2N` files ahead of the report | reactor pool; the runner helps while it waits |

- **Output is deterministic.** Files are reported in start order whatever
  `-j` is; compiler diagnostics and each case's prints / panic message are
  captured and replayed next to their verdict. Passing cases' output is
  hidden unless `--show-output`.
- **Isolation.** A worker VM is reused across jobs. Statics are reset and
  re-initialized per case, and the heap, finalizer registry (drop PCs), C
  struct layouts and thread program are replaced per job, so a case never sees
  another program's state.
- **`--fail-fast`** stops at once with `-j 1`; with `-j N` files already
  started still finish and are reported.
- **Leak smoke** runs `-j 1`: under `ulimit -v 65536` every extra thread's
  glibc malloc arena (64 MiB of address space) alone exceeds the cap.
- **GC mode.** Cases run with the same precise frame / class / static word
  maps as `coil <file>` (`wire_pipeline_threads`), so a `collect()` in a test
  exercises production roots.
- Cases that call `thread::spawn` put their jobs on the same reactor.

## Corpora in parallel

`tests/` holds one corpus per part of the language (`tests/README.md`).
`scripts/test_corpora.py` runs each corpus as its own `coil test` process,
largest first, several at once, and prints one line per corpus plus the
failing files' output:

```bash
scripts/test_corpora.py -- --root examples/src --root ../coil-stdlib/src
scripts/test_corpora.py patterns traits -- -O 0
scripts/test_corpora.py --shard 2/3 --no-build -- …   # every 3rd corpus from the 2nd
```

It builds `coil` / `coil-test` first (`--release` for the release build;
cargo's cache makes that a no-op when nothing changed). `--bin DIR` uses
binaries already built, `--harness PATH` runs a `coil-test` directly (the
GC-stress build), `--limit-as-kb N` caps each process's address space like
`ulimit -v` (the leak smoke, `-j 1` per process). Logs land in
`target/corpora/<corpus>.log`.

CI builds the binaries once per OS and build mode (`harness-build`: debug
and release, each with its GC-stress `coil-test`) and runs every setting
(default, `-O 0`, `COIL_HIR_INLINE=0`, leak smoke, GC stress) in both modes
as separate `harness` cells, side by side. The same corpus must pass in both
modes, so a build-mode difference (optimizer, overflow checks, debug
assertions) fails one of them.

On four cores the whole corpus takes ~14 s this way against ~31 s for one
`coil test` process on a release build (2026-10-15).

## Coverage

`coil test --coverage` reports line coverage of the project's own sources and
writes an lcov tracefile (default `target/coverage/lcov.info`,
`--coverage-out FILE`); `--coverage-per-test FILE` also writes which lines each
test case hit (JSON: `{"tests":[{"file","name","lines":{"src/a.hy":[3,4]}}]}`).

```text
test result: ok. 721 passed; 0 failed; 721 total

 57.1%      4/7      src/mathx.hy
 57.1%      4/7      total
coverage: lcov written to target/coverage/lcov.info
```

- **VM.** `machine` feature `coverage` (only `coil-test` enables it) adds a
  hit counter per PC, filled at the main dispatch while a job asks for it;
  dense streaks are off while counting, as under the debugger. Test jobs
  (`TestJob::coverage`) return the counts in `TestReport::hits`. Threads a case
  spawns run on other VMs and are not counted. Off, it costs nothing per
  instruction: the dispatch loop runs its hook-free instantiation
  (`execute::<false>`) unless a collector is attached (#558).
- **Lines.** Each PC's `DebugLoc` start gives its line (a statement's first
  line). A line is coverable when some instruction carries it; counts are
  summed over every test program that contains it.
- **What counts.** Sources under the current directory, outside `.deps/`.
  Test case bodies are left out unless the line also has code elsewhere, so
  a helper inlined into a test still counts.
- **Never-called code.** A coverage compile sets
  `Pipeline::set_keep_fns_in`: every function compiled from a project file is
  a tree-shake root, so an unused function is emitted and reports as uncovered
  instead of vanishing. Generic functions never instantiated have no code and
  do not appear.
- **Opt level.** Coverage runs at the tests' `-O` (default Standard). Rewrites (HIR inlining,
  MIR / dense) can still move a few lines: on the repo suite -O2 reports 8 of
  1326 lines uncovered that `-Og` covers.
- **Release builds** build `coil` / `coil-embed` in their own cargo invocation:
  a workspace-wide build unifies helper-only `machine` features (`debugger`,
  `coverage`) into every binary.

## Mutation testing

`coil mutate` (re-execs `coil-test mutate`) changes project sources one small
edit at a time and checks that some test notices. It reuses the runner: same
test root, `-O`, `--root`, grants, `--seed`, `-j`.

```text
$ coil mutate --root src
mutate: baseline run
running 1 file (seed 0x…, 4 jobs)
ok   tests/clamp.hy

mutate: 8 mutants in 1 file (4 covered, 4 jobs)
killed      src/mathx.hy:2  `x < lo` → `!(x < lo)`  [cond]  (tests/clamp.hy: clamps low)
survived    src/mathx.hy:2  `<` → `<=`  [boundary]
…
no coverage src/mathx.hy:12  `*` → `/`  [arith]

mutation score: 50.0% (2 killed, 0 timed out, 2 survived; 0 unviable, 4 without coverage)
```

1. **Baseline.** Run the suite once with line coverage, keeping each case's
   covered lines and step count (`TestReport::steps`). Any failure stops
   here: mutants need a green suite.
2. **Enumerate.** Parse each covered project source (not under the test
   root unless `--files` selects it) and list sites
   (`coil-test/src/mutate/sites.rs`). Test code (`test("…")`, `#[test] fn`)
   is never mutated.

   | Operator | Change |
   |----------|--------|
   | `boundary` | `<` ↔ `<=`, `>` ↔ `>=` |
   | `negate` | `==` ↔ `!=` |
   | `arith` | `+` ↔ `-`, `*` ↔ `/`, `%` → `*` |
   | `logic` | `&&` ↔ `\|\|` |
   | `cond` | `if c` / `while c` → `!(c)` |
   | `bool` | `true` ↔ `false` |
   | `int` | `0` ↔ `1`, `n` → `n + 1` |

   Operator sites are found as the only operator token between the operand
   spans. `// coil:no-mutate` on a line skips its sites; on (or just above)
   a `fn` header it skips the function.
3. **Coverage.** A site maps to the nearest coverable line at or above it in
   the same outermost `fn` (a statement's code carries its first line). No
   case hits that line → `no coverage`, nothing runs.
4. **Build + run.** The patched text is an in-memory overlay
   (`Pipeline::set_file_text`, keyed by every spelling of the path: debug
   info keeps it as resolved, reads join it onto the current directory).
   Only test files with a covering case are recompiled, and only covering
   cases run, each with a step budget of `--timeout-factor` (10) × its
   baseline steps + 10 000. First failure → `killed`; budget exhausted →
   `timeout` (a kill); compile error → `unviable` (not scored).
   Before any mutant, each target file is compiled once with a broken
   overlay: if that still compiles, the overlay is not reaching the compiler
   and the run stops instead of reporting every mutant as survived.
5. **Isolation.** Each mutant runs in a child `coil-test __mutant-worker`
   (same flags; the job on stdin, the verdict on stdout), `-j` at a time.
   A mutant can crash the VM (see the arithmetic row in
   [limitations](limitations.md)) or block outside the step budget (threads,
   host waits): a crash counts as `killed`, and `--wall-timeout` (60 s) kills
   a stuck worker as `timeout`.

Score = (killed + timeout) / (killed + timeout + survived). Results print in
source order whatever `-j` is.

| Flag | Effect |
|------|--------|
| `--files GLOB` | only sources whose path (relative to the current directory) matches (`*`, `**`, `?`; repeatable) |
| `--operators LIST` | subset of the operators above, comma-separated |
| `--timeout-factor N` | step budget multiplier |
| `--wall-timeout S` | per-mutant worker time limit |
| `--min-score P` | exit 1 below P percent |
| `--json` | NDJSON events on stdout instead of the text report (see [JSON events](#json-events)) |

On coil-stdlib (~2k mutants) a release `coil mutate -j 4` takes about five
minutes. Not yet: `--since REV` (changed lines only), statement deletion and
body replacement operators, dependency sources (`--include-deps`), and
compiling all of a file's mutants into one program (mutant schemata).

## JSON events

`coil test --json` and `coil mutate --json` print one JSON object per line on
stdout, flushed as it happens, instead of the text on stderr. Tools (spool)
render them. `--json` cannot be combined with `--log-json` / `--log-lsp`.

`coil test`:

| `event` | Fields |
|---------|--------|
| `start` | `files`, `seed` (`"0x…"`, `null` with `--no-shuffle`), `jobs` |
| `file` | `file`, `ok`, `passed`, `failed`, `message` (file-level verdict or `null`), `diagnostics` (compiler output or `null`), `cases`: `[{name, ok, timed_out, reason, output}]` (`output` only for failures or with `--show-output`) |
| `error` | `message` (harness error; the run then exits 1) |
| `summary` | `ok`, `passed`, `failed`, `total`, `seed`, `coverage`: `null` or `{hit, total, lcov, files: [{file, hit, total}]}` |

Files are reported in start order, as in the text report.

`coil mutate` (the baseline run is silent):

| `event` | Fields |
|---------|--------|
| `baseline` | none (the baseline suite started) |
| `plan` | `mutants`, `files`, `covered`, `jobs` |
| `mutant` | `file`, `line`, `operator`, `from`, `to`, `status` (`killed`, `timed_out`, `survived`, `unviable`, `no_coverage`), `killed_by` |
| `error` | `message` |
| `summary` | `ok` (false below `--min-score`), `score` (`null` when nothing scored), `min_score`, `killed`, `timed_out`, `survived`, `unviable`, `no_coverage` |

## Compile

Each file compiles in memory with `Pipeline::set_include_tests(true)` at the
requested `-O` level (default Standard, same as `coil compile`). Nothing is
serialized to `.hyc`, so test-only data never touches the archive format.

## Layout

| File | Role |
|------|------|
| `coil-test/src/args.rs` | argv (`--fail-fast`, `--seed`, `--no-shuffle`, `-j`, `--show-output`, `--coverage*`, `--no-doc-tests`, `--doc-tests-only`, `-O`, `--root`, host grants, `--log-*`; `mutate` flags) |
| `parser/src/doc_examples.rs` | doc example rules: fences, hidden lines, the doc-test program, documented items |
| `coil-test/src/order.rs` | seeded file / case order |
| `machine/src/reactor.rs` | `TestJob` (incl. `step_budget`), `submit_test` / `run_test_here`, `TestHandle` |
| `coil-test/src/coverage.rs` | PC → line maps, summing, lcov / summary / per-test JSON |
| `coil-test/src/runner.rs` | discovery, per-file compile, per-case VM, summary |
| `coil-test/src/mutate/` | `coil mutate`: sites, baseline / plan / report (`mod.rs`), per-mutant job and worker process (`job.rs`) |
| `coil-host/src/lib.rs` | `wire_pipeline_vm`, `wire_pipeline_threads`, `execute_pipeline` |

Plan for parallel runs, coverage and mutation testing: the *coil-test:
Coverage, Parallel Runner & Mutation Testing* design doc.

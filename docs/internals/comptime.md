# Comptime: code that runs while compiling

Coil runs code while it compiles in a few forms. All of them use one
compile-time VM (`MacroHost`, installed by `comptime::install()`), the same
limits and the same IO grants. This page maps the forms and the stages and
links to the details, most of which are in [Macros](macros.md).

Design: the Claude Doc "Coil comptime: compile-time code with IO" (rev 11,
2026-10-20). It was built on the `feature/comptime` branch in 15 steps
(coil-lang#1809).

## Forms

| Written | Runs | Gives | Details |
|---------|------|-------|---------|
| `const X = f();` that does not fold | after codegen (`Pipeline::compute_consts`) | a value, restored from a heap snapshot | [Compile-time consts](macros.md#compile-time-consts) |
| `comptime e` | as a hidden const `__comptime_<offset>` | a value | [Compile-time expressions](macros.md#compile-time-expressions) |
| `comptime { … }` in an expression | as a hidden `fn __comptime_<offset>_body()` read by a hidden const | a value | [Compile-time expressions](macros.md#compile-time-expressions) |
| `comptime { … }` at the top level | in the expansion stage, before typechecking | `Code`: items that replace the block | [Compile-time blocks](macros.md#compile-time-blocks) |
| `derive`, `attr`, `macro` | in the expansion stage | `Code` | [Macros](macros.md) |
| a pure `static let`, a pure call with constant arguments at `-O2`+ | silently, after codegen | a value, or the code unchanged | [Compile-time calls](macros.md#compile-time-calls) |

A macro reads the constant value of an argument with `Expr.int()`,
`float()`, `bool()` and `string()` ([The `macro` module](macros.md#the-macro-module)).
Generated code may declare macros and use them; they run in a later
expansion round ([Pipeline stage](macros.md#pipeline-stage)).

## Stages

1. Parse. `comptime_expr::hoist` (from `const_fold::lift_startup_consts`)
   turns `comptime e` and value blocks into hidden consts.
2. Expansion rounds (`Pipeline::expand_user_macros`). Macro calls and
   top-level blocks are grouped into expansion programs, which are compiled by
   a sub-pipeline and run on the VM. Their output is spliced in, and the
   hoisting runs again on it. A round's output may hold more macro calls
   and macro declarations, so rounds repeat until nothing is pending (at most
   `MAX_ROUNDS`).
3. Typecheck and codegen of the whole program. A const initializer must be
   pure, or have IO its module was granted (E0134 otherwise). A const that
   only opens a process handle (`stdout()`, `declare(…)`) is computed at
   startup instead.
4. `compute_consts`. Every const runs in setup order on one VM. The values
   become one `RestoreSnapshot`, and pure calls are folded at `-O2`+. A
   const whose value exists only at run time (a function value, a handle)
   keeps its startup initializer.

The staging rule follows from step 2: a top-level block or a macro runs
before its own module is typechecked, so it can only use code from other
modules. A value block or `comptime e` runs in step 4 and may call its own
module's functions.

## Limits

| Tier | Steps | Heap | Time | On failure |
|------|-------|------|------|------------|
| Explicit (`const`, `comptime`, macros, blocks) | `--comptime-steps` (1B) | `--comptime-mem` (1024 MiB) | `--comptime-timeout` (300 s), checked at IO calls | an error that names the flag |
| Silent (pure `static let`, call folding) | 1M per call, 20M per module | the same | none | the code is left as written |

Details: [Compile-time host](macros.md#compile-time-host).

## IO

Compile-time code gets no IO by default. Each grant is a flag and may be
scoped to one module with `MODULE=`:

| Flag | Allows |
|------|--------|
| `--comptime-read PATH` | reading files under `PATH` |
| `--comptime-write PATH` | writing files under `PATH` (such an evaluation is never cached) |
| `--comptime-env NAME` | reading one environment variable |
| `--comptime-exec NAME` | running one program, matched by stem |
| `--comptime-net HOST[:PORT]` | TCP connections to that host |

Results that did IO are kept in the result cache (`--comptime-cache`,
`--comptime-refresh`, `--comptime-offline`). A changed file read is a miss.
The environment, programs and the network are used again only on a miss or
on refresh. Details: [Compile-time IO](macros.md#compile-time-io) and
[Compile-time result cache](macros.md#compile-time-result-cache).

Download pins (`--comptime-pin`) are not built yet; see coil-lang#1755.

## Diagnostics

- E0134: a const or `comptime` could not be computed: an effect without a
  grant, a refused IO call, a panic, a limit, or (for `comptime` only) a
  value that exists only at run time (a function, a handle). The help says
  which.
- E0135 (info): `--comptime-refresh` got a different result than the cached
  one.
- Errors in a top-level block's body are reported in the user's file. A
  name declared in the same module gets help about staging.

## Tooling

- `coil lsp` replays compile-time IO from the result cache only
  (`CacheConfig::replay`). On a miss the environment, programs, the network
  and writes are refused, and the diagnostic says to build the project.
  Reads of granted files still run. Pass the editor the same `--comptime-*`
  flags as the build and start it in the same working directory, so it uses
  the same cache.
- `coil dissect --expand` prints the file after expansion. Each run of
  generated items starts with a comment naming the macro or block and its
  position. The hidden function and const of a `comptime` name the position
  of the `comptime` they came from.
- `--opt-stats` reports how many macro calls and blocks ran, how many reused
  a result, how many consts were computed (and how many came from the
  cache), how many pure calls were tried, and the wall time of each.
  `--opt-stats-json` gives the same numbers as `comptime_*` fields.

## Performance

Release build, 2026-10-21:

- A file with one top-level block that calls a generator module takes 244 ms
  cold and 109 ms with the block's program in the user cache.
- A file with one `assert!` takes 146 ms cold and 21 ms warm. Compiling the
  `assert!` provider costs about 115 ms per process, and loading it from
  disk takes under 2 ms.
- On a second build, a const that read the environment takes its value from
  the result cache and does not read the environment again.

## Known gaps

- coil-lang#1755: download pins.
- coil-lang#1798: a unit-valued `comptime` (only side effects) does not lower.
- coil-lang#1799: a block-bodied lambda called in place does not lower.
- coil-lang#1810: folded string consts keep `\"` and `\\` undecoded. So
  `Expr.string()` only accepts a literal for now.
- A value block cannot be a statement that expands into several statements.
  It always gives one value.
- A macro is called by its bare name (`name!`). A path (`m::name!`) does not
  parse.

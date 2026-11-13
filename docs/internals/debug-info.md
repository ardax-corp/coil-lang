# Debug info (`.ccoil`)

Compiled programs carry their debug tables inside the versioned `ArchivedProgram`
envelope. This is separate from the in-memory `reporting::SourceMap` used for
compile-time ariadne diagnostics.

## What is stored

`common::ProgramDebug` holds the tables, in memory and in the archive:

| Field | Meaning |
|-------|---------|
| `source_files` | Project-relative paths (stable indices) |
| `debug_locs` | Each location the bodies use, once, sorted by file, start and end |
| `debug_lines` | Line and column of each of `debug_locs`, resolved at compile time |
| `fn_symbols` | `(name, body_id)` per body, sorted by `body_id`; `body_id` is `label.0 + 1`. Names backtrace frames |
| `inline_scopes` | The callees inlining spliced in ([below](#inline-scopes)); in an archive, at the head of the MIR section |

A `DebugLoc` records:

- `file`: index into `source_files` (`u32::MAX` is unknown or synthetic)
- `start_byte` / `end_byte`: UTF-8 byte range in that source file
- `scope`: `0` for code of the function it is in, else `k` for the inline scope
  `inline_scopes[k - 1]` it was spliced in from

MIR carries the locations: `MirFunc::value_locs` and `op_locs` give each op's, and
`MirFunc::op_loc` reads them. `finalize_bodies` collects every location the bodies
use into `debug_locs`. `Pipeline::program_debug_full` then resolves lines while the
sources exist, so archives and packaged binaries never read them back (#580).

`ProgramDebug::line_of(loc)` answers a panic site: a binary search for the location,
where a span that differs only in its end stands in for it. The table stays keyed by
the locations as the MIR has them; `debug_lines` holds the remapped lines (below).

`coil debug` builds a `ProgramDebug` of its own that numbers every MIR op
(`prepare_debug`), and `loc_at` / `file_line_at` read that by op number.

## Inline scopes

HIR inlining (`hir::inline`) splices a callee's nodes into its caller. Each splice
adds a `HirInlineScope` to the caller's body: the callee's name, the call's span,
the scope the call was in (`call_scope`), where the callee computes its result (its
`return` statement) and `at_call`. Every spliced node gets the new scope in
`HirExpr::scope`; scopes the callee had nest under it, so a callee spliced into a
callee that was spliced into the caller gives nested frames. Nodes the splice makes
at the call (a parameter's `let`, the result's temp) are in the call's scope.

MIR lowering (`Lower::at_stmt`, `Lower::in_scope`) turns a node's scope into the
program's id (`LowerCx::inline_scope`, `Compiler::intern_inline_scope`: the same
callee and call give the same id) and puts it in the op's `DebugLoc`. The result
of an inlined callee is lowered inside the caller's statement, with no statement of
its own: `in_scope` gives it the callee's `return` statement, as `-O0` reports it.
The scope is part of the location, so every MIR pass that moves, clones or merges
ops keeps it as it keeps the span: a merged op keeps one op's location, the one the
pass keeps (deterministic, as the passes are). The table of locations
(`debug_locs`) holds spans only, plus each scope's call.

The panic report (`Locator::backtrace`, both engines, JIT, archive and AOT) expands
a site's location: one frame per scope, innermost first (`in boom at x.coil:5:11`),
then the function the code is in at the call of the outermost scope
(`in run at x.coil:11:24`). What else makes code of one function run in another:

- **Another module's callee** (a portable body, a resolved method or mono clone
  from any module): its spliced nodes take the call's span, since the planner's
  facts for them are keyed by it, and keep their own in `HirExpr::src` (#2666). The
  scope records the callee's file (`HirInlineScope::file`, from
  `Compiler::hir_splice_files`, noted when the body was kept for splicing), and
  lowering locates the scope's nodes in that file (`Lower::scope_file`): an op's
  `DebugLoc::file` is the callee module's entry in `source_files`, as any other, so
  the archive needs nothing beyond the scope table. A clone's frame is named by its
  entry (`helper::pick$inst$int`), as a call of it shows. Only when the file is not
  known (a prelude instance) is the scope `at_call`: the callee's frame has no line,
  the caller's is at the call. A foreign callee's cold helper keeps the call's span.
- **Cold helpers** of partial inlining (`hir::outline`, `{function}$cold{k}`) are cut
  from a callee before it is spliced: the helper's frame shows as the function's,
  and the frame of the scope that called it (the same activation) is not repeated.
- **Auto-par workers** are the loop's body: a site in a worker is shown in the loop's
  function, with its scopes.
- **Generator bodies**, `defer` pads and outlined helpers lower through the same
  path and keep their nodes' scopes. A body with a `defer` is not inlined into, and a
  callee with one is not spliced.
- **Comptime-folded calls** leave a constant: nothing at run time panics there.
- There is no MIR-level inlining. `coil debug` compiles at `-O0`, with no inlining.

## Function symbols

`coil compile` writes `fn_symbols` into the `.ccoil`, and the loader
(`coil_cli::load_archive_bytes`, shared by `coil run x.ccoil`, packaged binaries and
`coil dissect`) restores them into `ProgramDebug`, so a backtrace from an archive
names its frames like one from source (`in inner at …`, not `in <unknown> at …`;
coil-lang#1490). `coil package` keeps them too; `--strip-debug` drops them along with
the other tables.

## Debug levels (`-g0` / `-g1` / `-g2`)

`coil`, `coil compile`, `coil package` and `coil test` take `-g LEVEL`
(coil-lang#1491). `Pipeline::program_debug` applies it
(`ProgramDebug::with_level`), so the in-memory run, the `.ccoil` and the packaged
payload all keep the same thing:

| Level | Keeps | Panic output |
|-------|-------|--------------|
| `-g2`, `-g1` | every table | `panic: m at x.coil:3:8` / `  in inner at x.coil:3:8` |
| `-g0` | nothing | `panic: m` / `  in <unknown>` |

`-g1` and `-g2` keep the same tables: a panic site is looked up by span, so every
program that runs MIR keeps spans (`Pipeline::debug_level`, `set_runs_mir`).

Defaults: `-g2` for `coil`, `coil compile` and `coil test`; `-g1` at `-Os` and for
`coil package`. `coil test --coverage` refuses `-g0` and `-g1`. `--strip-debug` is
`-g0`. `coil debug` and `coil dissect` always use the full tables.

## Archive version

Inline scopes came with minor 1 of major 8: the MIR section's format 17 holds the
scope table after its format byte and, per body, a sparse list of the locations in a
scope. A format 16 section (minor 0) loads with no scopes.

The version is packed `major.minor`. A loader takes its own major and a minor it
knows, nothing else ([archive-history.md](archive-history.md)).

## Runtime `panic` output

`panic` **aborts the process**; `raise` is catchable (**Q5**,
[language-quirks.md](language-quirks.md)). Checksum / CLI boards use `panic`.

When a panic site has a known location, the message is printed as:

```text
panic: <message> at <path>:<line>:<column>
```

Line numbers are **1-based**; columns are **0-based** UTF-8 character offsets on that
line. Both come from `debug_lines` (`Locator::native_site_location`); no source file
is read at run time.

If the location is unknown, only `panic: <message>` is shown. Runtime errors
(out-of-bounds index, resume of a finished coroutine, …) also print the call stack
(`  in <fn> at <file:line>`, repeated recursion collapsed). An explicit `panic`
prints it when `COIL_BACKTRACE=1` is set; by default it does not, because checksum
boards print their result with `panic` and compare the text.

## Limitations

- **Macro expansion:** snippets are parsed at byte offsets past the original file.
  `Pipeline::program_debug_full` rewrites their lines onto the `#[derive]` / attribute
  / `name!(…)` use site, so line breakpoints and `coil debug` `list` read the source
  the user wrote.
- **Debugger usefulness:** line breakpoints need a known location at that line.
  Unmapped lines stay unverified. `coil debug` `list` may snap to a nearby known
  location in the same function; `bt` / DAP stack frames stay exact.

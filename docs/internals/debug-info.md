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
| `fn_symbols` | `(name, entry_pc)` per body, sorted by `entry_pc`; `entry_pc` holds the body's identity (`label.0 + 1`). Names backtrace frames |

A `DebugLoc` records:

- `file`: index into `source_files` (`u32::MAX` is unknown or synthetic)
- `start_byte` / `end_byte`: UTF-8 byte range in that source file

MIR carries the locations: `MirFunc::value_locs` and `op_locs` give each op's, and
`MirFunc::op_loc` reads them. `finalize_bytecode` collects every location the bodies
use into `debug_locs`. `Pipeline::program_debug_full` then resolves lines while the
sources exist, so archives and packaged binaries never read them back (#580).

`ProgramDebug::line_of(loc)` answers a panic site: a binary search for the location,
where a span that differs only in its end stands in for it. The table stays keyed by
the locations as the MIR has them; `debug_lines` holds the remapped lines (below).

`coil debug` builds a `ProgramDebug` of its own that numbers every MIR op
(`prepare_debug`), and `loc_at` / `file_line_at` read that by op number.

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

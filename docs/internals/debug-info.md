# Debug line table (`.hyc`)

Compiled programs carry a **debug line table** inside the versioned
`ArchivedProgram` envelope. This is separate from the in-memory
`reporting::SourceMap` used for compile-time ariadne diagnostics.

## What is stored

| Field | Meaning |
|-------|---------|
| `source_files` | Project-relative paths (stable indices) |
| `line_table` | Run-length line table (minor 40+): `(file, line, column)` per run of slots |
| `span_table` | Run-length byte spans (minor 40+, `-g2` only): `(file, start, end)` per run |
| `fn_symbols` | `(name, entry_pc)` per function, sorted by `entry_pc`; names backtrace frames |
| `cleanup_ranges` | `defer` pads a panic runs; behaviour, kept at every `-g` level |
| `debug_locs` / `debug_lines` | The dense per-slot tables of minors 30–39; new archives leave them empty |

The compiler still records one `DebugLoc` per bytecode slot in memory
(`Pipeline::program_debug_full`); the archive writers
(`ArchivedProgram::set_debug`) turn that into runs at the chosen level.

Each `DebugLoc` records:

- `file` — index into `source_files` (`u32::MAX` = unknown / synthetic)
- `start_byte` / `end_byte` — UTF-8 byte range in that source file

Debug locs are attached to IL ops and carried through `il::lower` fuse-select
(one loc per final bytecode slot). Codegen gives every op a statement emits
the statement's span (`Compiler::fill_statement_locs` after each block
statement); ops that already have a narrower location (a nested statement,
`panic`, a call) keep it. Labels carry none.

## Function symbols

`coil compile` writes `fn_symbols` into the `.hyc`, and the loader
(`coil_cli::load_archive_bytes`, shared by `coil run x.hyc`, packaged
binaries and `coil dissect`) restores them into `ProgramDebug`, so a backtrace
from an archive names its frames like one from source (`in inner at …`, not
`in <unknown> at …`; coil-lang#1490). `coil package` keeps them too;
`--strip-debug` drops them along with `source_files`, `debug_locs` and
`debug_lines`. `fn_symbols` stays its own field, so a finer debug-info level
(coil-lang#1491) can keep names while dropping the line table.

Size: each entry is an rkyv `ArchivedString` (8 bytes, names of up to 8 bytes
stored inline) plus a `u32` pc, so 12 bytes plus the name bytes past 8. The
`.hyc` size did not change with #1490 (it already carried the symbols). The
payload `coil package` embeds grew by:

| Program (default `-O`) | Payload before | After | Growth |
|------------------------|---------------:|------:|-------:|
| `tests/testing/doc_tests.hy` | 22,496 B | 26,660 B | +4,164 B (+18.5%) |
| `tests/traits/trait_method_bound_dispatch.hy` | 35,800 B | 36,332 B | +532 B (+1.5%) |
| `tests/contracts/contract_failure_messages.hy` | 15,080 B | 15,484 B | +404 B (+2.7%) |
| `examples/derive_show_eq.hy` | 13,652 B | 13,908 B | +256 B (+1.9%) |

Most programs grow by 1–3%; `doc_tests.hy` is an outlier because it has many
small functions with long qualified names. `--strip-debug` payloads are
unchanged (e.g. `doc_tests.hy` 9,400 B). Against the ~1.4 MB `coil-embed`
runner every packaged binary grows by well under 1%.

## Debug levels (`-g0` / `-g1` / `-g2`)

`coil`, `coil compile`, `coil package` and `coil test` take `-g LEVEL`
(coil-lang#1491). `Pipeline::program_debug` applies it
(`ProgramDebug::with_level`), so the in-memory run, the `.hyc` and the
packaged payload all keep the same thing:

| Level | Keeps | Panic output |
|-------|-------|--------------|
| `-g2` | `source_files`, `fn_symbols`, line runs, span runs | `panic: m at x.hy:3:8` / `  in inner at x.hy:3:8` |
| `-g1` | `source_files`, `fn_symbols`, line runs | the same (lines and columns come from the line runs) |
| `-g0` | nothing (only `cleanup_ranges`) | `panic: m` / `  in <unknown>` |

Defaults: `-g2` for `coil`, `coil compile` and `coil test` (and always with
`coil test --coverage`, which maps hits through byte spans; `-g0`/`-g1` with
coverage is an error); `-g1` at `-Os` and for `coil package`.
`--strip-debug` is `-g0`. `coil debug` and `coil dissect` always use the full
tables (`program_debug_full`).

## Compact line table (minor 40)

`common::LineTable` holds runs sorted by `pc`; a run starts wherever the
location changes, and a run with `file == u32::MAX` marks slots with none.
Lookups (`line_at`, `span_at`) binary-search the runs, so the VM's panic path
and the debugger's `debug_pc_line` read the table directly; there is no
per-pc cache when the program came from a minor-40 archive (the dense cache is
still built for source runs and older archives).

Encoding (all LEB128 varints, `zigzag` for signed deltas):

```text
line_table: len  nruns  { pc_delta  file+1  [zigzag(line - prev_line)  column] }*
span_table: len  nruns  { pc_delta  file+1  [zigzag(start - prev_start)  end - start] }*
```

`file+1 == 0` is an unknown run and carries no other fields. `len` is the
bytecode length; the loader checks it against the bytecode, checks that run
pcs increase and that every file indexes `source_files`, and rejects a table
that does not decode as a corrupt archive. Archives of minors 33–39 load
through `ArchivedProgramV39` with their dense tables, which the VM still
reads (and falls back to the sources for lines before minor 30).

### Size (coil-lang#1491)

`.hyc` bytes, before (dense tables, minor 39, `coil compile` at `b1b08919`)
and after at each level. spool is `spool.hy` with coil-stdlib, coil-toml and
coil-json (the revisions spool pins); the examples are compiled with
`--root examples/src --root ../coil-stdlib/src`.

| Program | Before | `-g2` | `-g1` | `-g0` |
|---------|-------:|------:|------:|------:|
| spool `-O0` | 1,142,808 | 630,456 (−44.8%) | 584,616 (−48.8%) | 525,292 (−54.0%) |
| spool `-O2` | 1,253,884 | 667,968 (−46.7%) | 620,908 (−50.5%) | 563,340 (−55.1%) |
| spool `-Os` | 1,229,804 | 661,032 (−46.2%) | 614,788 (−50.0%) | 557,432 (−54.7%) |
| `examples/task_files.hy` `-O2` | 14,112 | 7,060 (−50.0%) | 6,496 (−54.0%) | 5,672 (−59.8%) |
| `examples/derive_show_eq.hy` `-O2` | 13,928 | 8,436 (−39.4%) | 8,064 (−42.1%) | 7,388 (−47.0%) |

For spool at `-O2` the run tables cost 47 KB of span runs at `-g2` and 58 KB
of line runs plus function names and paths at `-g1`, against 690 KB for the
dense `debug_locs` and `debug_lines` with the same names and paths (before
minus `-g0`). `coil package` embeds the same
archive bytes, so its payload shrinks the same way; it now defaults to `-g1`
(spool's packaged payload: 1,253,884 → 620,908 B). `-Os` defaults to `-g1`
too, so a plain `coil compile -Os` of spool writes 614,788 B.

## Archive version

Archive version is packed `major.minor`. Older `.hyc` files without `strings`,
`source_files`, or `debug_locs` are rejected at load time.

## Runtime `panic` output

`panic` **aborts the process**; `raise` is catchable (**Q5**,
[language-quirks.md](language-quirks.md)). Checksum / CLI boards use
`panic`.

When a `panic` aborts the VM and the panic instruction has a known
`DebugLoc`, the message is printed as:

```text
panic: <message> at <path>:<line>:<column>
```

Line numbers are **1-based**; columns are **0-based** UTF-8 character
offsets on that line. The VM reads source text from disk using the
stored path (relative paths are resolved against the entry script’s
directory).

If the location is unknown or the file cannot be read, only
`panic: <message>` is shown. Runtime errors (out-of-bounds index, resume of a
finished coroutine, …) also print the call stack (`  in <fn> at <file:line>`,
repeated recursion collapsed). An explicit `panic` prints it when
`COIL_BACKTRACE=1` is set; by default it does not, because checksum boards
print their result with `panic` and compare the text.

## Limitations (MVP)

- Many codegen sites still emit **unknown** locations; coverage grows
  incrementally (`panic`, `raise`, formatting/stdout calls, and padded slots elsewhere).
- Fused super-instructions keep the **first** slot’s span only.
- **Macro expansion:** snippets are parsed at byte offsets past the original
  file. `Pipeline::program_debug` rewrites those `DebugLoc`s onto the
  `#[derive]` / attribute / `name!(…)` use site so line breakpoints and
  `coil debug` `list` read the source the user wrote.
- **Debugger usefulness:** line breakpoints need a known loc at that line.
  Unmapped lines stay unverified. `coil debug` `list` may snap to a nearby
  known loc in the same function; `bt` / DAP stack frames stay exact
  (no path / line 0).

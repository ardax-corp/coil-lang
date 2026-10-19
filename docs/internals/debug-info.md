# Debug line table (`.hyc`)

Compiled programs carry a **debug line table** inside the versioned
`ArchivedProgram` envelope. This is separate from the in-memory
`reporting::SourceMap` used for compile-time ariadne diagnostics.

## What is stored

| Field | Meaning |
|-------|---------|
| `source_files` | Project-relative paths (stable indices) |
| `debug_locs` | One entry per bytecode slot (same length as `bytecode` after finalize) |
| `fn_symbols` | `(name, entry_pc)` per function, sorted by `entry_pc`; names backtrace frames |

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

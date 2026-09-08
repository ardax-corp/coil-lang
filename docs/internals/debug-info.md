# Debug line table (`.hyc`)

Compiled programs carry a **debug line table** inside the versioned
`ArchivedProgram` envelope. This is separate from the in-memory
`reporting::SourceMap` used for compile-time ariadne diagnostics.

## What is stored

| Field | Meaning |
|-------|---------|
| `source_files` | Project-relative paths (stable indices) |
| `debug_locs` | One entry per bytecode slot (same length as `bytecode` after finalize) |

Each `DebugLoc` records:

- `file` — index into `source_files` (`u32::MAX` = unknown / synthetic)
- `start_byte` / `end_byte` — UTF-8 byte range in that source file

Debug locs are attached to IL ops and carried through `il::lower` fuse-select
(one loc per final bytecode slot).

## Archive version

Archive version is packed `major.minor`. Older `.hyc` files without `strings`,
`source_files`, or `debug_locs` are rejected at load time.

## Runtime `panic` output

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
`panic: <message>` is shown.

## Limitations (MVP)

- Many codegen sites still emit **unknown** locations; coverage grows
  incrementally (`panic`, `raise`, formatting/stdout calls, and padded slots elsewhere).
- Fused super-instructions keep the **first** slot’s span only.
- **Debugger usefulness:** line breakpoints need a known loc at that line.
  Unmapped lines stay unverified. `coil debug` `list` may snap to a nearby
  known loc in the same function; `bt` / DAP stack frames stay exact
  (no path / line 0).
- No call-stack walk on panic yet (planned follow-up).

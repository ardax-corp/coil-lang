# Compiler peak memory

Where the compiler's own peak RSS comes from while it compiles a program, and
what was done about it (2026-10-27). The VM's heap and GC are a separate
subject.

## Measuring

- Peak RSS: `ru_maxrss` of the `coil compile --no-build-cache` child (for
  example `python3 -c 'import resource,subprocess,sys; subprocess.run(sys.argv[1:]); print(resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss)' coil compile …`).
- Per phase: a throwaway local patch that prints `VmRSS` / `VmHWM` from
  `/proc/self/status` and glibc `mallinfo2()` live heap at each step of
  `Pipeline::compile_src_from_file` and after each module. Not committed.
- Attribution: `heaptrack` on a build with symbols
  (`CARGO_PROFILE_RELEASE_STRIP=false CARGO_PROFILE_RELEASE_LTO=off CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`),
  then `heaptrack_print -p 1 -a 0 -T 0`. Massif and heaptrack's flame graph
  output run to tens of GB on this compiler (deep parser and checker
  recursion); the peak list does not.

Inputs: spool (`spool.hy` with coil-stdlib, toml, json, process: the largest
program at hand, ~2.7 s to compile) and coil-http's examples (stdlib + tls).

## Where the peak was

spool, before:

| Step | Live heap | RSS |
|---|---:|---:|
| parse every file | 19.5 MiB | 27 MiB |
| macro expansion sub-compile | (freed after) | 42 MiB |
| every module checked + HIR + MIR | 61.5 MiB | 74 MiB |
| final lowering (MIR tiers, flatten, fuse) | transient | **93 MiB** |

- Lowering copied the program two to three times: `IlModule::from_flat`
  cloned every `IlFunc` with its `Box<MirFunc>` (the `CodeBuf` kept its own),
  `to_flat` cloned the whole module again, and every body's LIR draft stayed
  alive to the end.
- Every file's AST stayed cached until the compile ended (~18 MiB on spool).
- glibc keeps the pages of freed small blocks: the sub-compile's memory and
  freed trees still counted toward the later peak.
- Smaller programs (coil-http) peak inside the module loop instead, where all
  ASTs plus the accumulating checker tables and per-function MIR live.

## What changed

| Change | spool | ws_loopback |
|---|---:|---:|
| before | 93 MiB | 49.5 MiB |
| lowering moves MIR and ops, drops a body's MIR / LIR draft once its tier is settled | 81 MiB | 49.5 MiB |
| drop ASTs after the module loop; `malloc_trim` after the expansion sub-compile and the module loop (glibc only) | 73 MiB | 49.5 MiB |
| drop a module's AST right after its codegen unless it keeps a `ModuleCtx` for foreign clones | **65 MiB** (−30%) | **42.5 MiB** (−14%) |

Archives are byte-identical before and after (spool, the coil-http examples,
`examples/`). Compile time unchanged within noise.

Tried and not taken: jemalloc and mimalloc (via `LD_PRELOAD`) are 10–15%
faster to compile but peak 2–8% higher.

## What is left at the peak

From the heaptrack peak list after the changes, roughly:

- Per-function MIR for the whole program, kept until final lowering (the
  dense tier is leaf-first across the program), with its sidecars: debug
  locals (`record_debug_local`), deopt maps, `relayout`.
- Checker tables per node (`NodeId → Ty`, overload candidates, substitution)
  and the typed sidecar, kept for the whole compile.
- `ModuleCtx` env copies of modules other modules may clone from.

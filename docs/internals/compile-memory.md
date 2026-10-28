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

## Round two: MIR and checker tables

Live at the end of the module loop on spool, measured with the per-phase patch
plus a size walk of `MirFunc` and the checker maps:

| What | Live |
|---|---:|
| per-function MIR (1074 bodies), of which spare `Vec` capacity | 11 MiB, 3.3 MiB |
| `ModuleCtx` checker snapshots (core, collections, text) | 9 MiB |
| the live checker's per-node tables | ~3.5 MiB |

- Each `ModuleCtx` holds a full `Checker` clone. It is only read while a later
  module's foreign clones compile, so it was dead weight through final
  lowering.
- MIR instruction vectors were built by pushing and never trimmed: half their
  storage was spare capacity (88-byte `MirInst`).

| Change | spool | ws_loopback |
|---|---:|---:|
| before (round one) | 64.8 MiB | 41.9 MiB |
| drop the kept `ModuleCtx`s after the module loop; trim a body's MIR vectors when it is stored | **55.5 MiB** (−14%) | **40.1 MiB** (−4%) |

spool at `-O0` 61.3 → 53.1 MiB and `-O3` 65.7 → 56.4 MiB. Live heap entering
final lowering 43 → 28.5 MiB. Archives byte-identical (spool at three opt
levels, all 14 coil-http examples). Only `Vec`s are trimmed: shrinking a
`HashMap` changes its iteration order.

## What is left at the peak

The peak is now the end of the module loop rather than final lowering:

- Per-function MIR (~8 MiB on spool), needed by final lowering. Shrinking it
  further means a smaller `MirInst` or fewer sidecars (deopt sites 1.3 MiB,
  value locations 0.7 MiB).
- `ModuleCtx` checker snapshots while modules still compile: each is a full
  clone, so global tables (env, schemes, overload sets) are copied once per
  kept module. Sharing or pruning them is the next lever.
- The live checker's per-node tables and the typed sidecar.

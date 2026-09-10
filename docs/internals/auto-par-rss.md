# Auto-par RSS, wall time, and `.hy` vs `.hyc`

Investigation of three reports when running a `.hyc` or a `coil-embed`
packaged app:

1. `COIL_AUTO_PAR=1` shows a large `peak_rss` spike
2. Little wall-time difference vs `COIL_AUTO_PAR=0` on the same run
3. Running `.hy` directly is faster than the `.hyc` / packaged binary

**Verdict:** (1) and (2) are explained by the current isolate-per-job reactor
plus AlwaysPar nested specializations. They are **expected costs of the
design**, not a one-line correctness bug. (3) did **not** reproduce on
`fib(32)` / a dense counted loop in this checkout: `.hyc` was as fast or
faster; `.hy` RSS is higher because the compiler stays mapped. Remaining
`.hyc`/embed taxes are real (slot heuristic, missing stack maps, extra
clones, reading the whole packaged exe) and can dominate other workloads.

`COIL_AUTO_PAR` is **compile-time only**
([`auto_par_enabled`](../../compiler/src/codegen/mod.rs)). Setting it when
*running* an already-built `.hyc` or packaged app does nothing. Recompile
to change IPA.

Related: [auto-par.md](auto-par.md), [stack-bounds.md](stack-bounds.md),
[mir-stack-maps.md](mir-stack-maps.md), [heap-identity.md](heap-identity.md).

## What auto-par actually does at runtime

Codegen ([`emit_one_par_specialization`](../../compiler/src/codegen/compiler.rs))
emits nullary `__coil_par_*` clones that **always** `thread_spawn` arm 0,
evaluate other arms inline, `thread_join`, and combine. Failed spawn/join
falls back to sequential arms.

`thread::spawn` / IPA share one work-stealing pool
([`machine/src/reactor.rs`](../../machine/src/reactor.rs)). The pool is
**lazy**: OS workers start on the first `Reactor::submit`, not at VM
construct. Sequential programs (`COIL_AUTO_PAR=0`, or auto-par on but no
site above `COIL_PAR_THRESHOLD`) never start the pool.

Each pool thread:

- is created with an **8 MiB** OS stack (`Reactor::ensure_started`) because
  nested join-help can be deep
- owns a persistent `Machine` whose operand stack is grown to
  `ThreadProgram.operand_stack_slots`
- runs jobs on a **private heap** (isolate model in
  [`machine/src/thread.rs`](../../machine/src/thread.rs)); slabs stay mapped
  after sweep ([heap-identity.md](heap-identity.md))

Join help-steals onto **new** boxed `Machine`s
(`wait_join_on_worker` / `help_once` → `machine_for_program`) instead of
the persistent worker VM. Nested IPA join-help can therefore stack several
full VMs on one OS thread.

Every job (`run_job_on_vm`):

1. `Natives::clone_registry` (id table copy; closures are `Arc`)
2. `load_program`: **memcpy** of the whole bytecode + constant pool +
   string table into `Machine::program_code` even though `Job.program` is
   already `Arc<ThreadProgram>`
3. does **not** reset the worker heap between jobs

Bytecode is shared via `Arc` at the `ThreadProgram` layer; the extra
per-job `to_vec` is avoidable.

## Symptom 1 — `peak_rss` spike with auto-par on

**Verified** on release `fib(32)` (4 CPUs, this VM):

| Run | wall | peak RSS |
|-----|------|----------|
| `.hyc` sequential (`COIL_AUTO_PAR=0` at **compile**) | ~50 ms | ~4.3 MB |
| `.hyc` IPA default (`N=4` workers) | ~31 ms | ~19–21 MB |
| `.hyc` IPA `COIL_MAX_WORKER_THREADS=1` | ~54 ms | ~26 MB |
| `.hy` sequential | ~51 ms | ~8.2 MB (compiler + VM) |
| `.hy` IPA | ~34 ms | ~23–30 MB |
| packaged `coil-embed` (IPA archive) | ~34 ms | ~20–21 MB |

Archive size: 596 B sequential vs 6504 B IPA (`__coil_par_fib_22` … `_32`).

Causes (code, not guesses):

1. **N × 8 MiB reactor stacks** plus N isolate heaps / slabs, started on
   first spawn (`Reactor::ensure_started`, `WorkerCap` /
   `COIL_MAX_WORKER_THREADS`, default `available_parallelism` min 2).
2. **Nested help VMs.** With `workers=1`, RSS was *higher* than with 4
   workers: join-help allocates extra `Machine`s (operand stack + heap)
   on the joining thread. Expected, not a leak of the sequential heap.
3. **AlwaysPar tree.** `fib(n)` for `n > 20` forks at every specialization
   down to the cutoff, so inflight jobs and help VMs overlap.
4. **`.hyc` operand-stack heuristic.**
   [`archive_operand_slots`](../../coil-cli/src/lib.rs) does not persist
   the compiler’s bound ([stack-bounds.md](stack-bounds.md)). If the
   archive contains `Seek` **and** `CALL`/`TailCall`, the loader requests
   [`MAX_OPERAND_STACK_SLOTS`](../../machine/src/lib.rs) (**1 048 576** ×
   8-byte `Value` ≈ 8 MiB) **per** root and per worker/help VM.
   Compile-and-run uses the analyzed size (`fib(32)` → 512). Dense loop
   bytecode in this tree *does* contain `Seek` + prologue `CALL`.
5. **Packaged apps** (`try_run_embedded`): `std::fs::read` of the **entire**
   executable, then rkyv deserialize (another owned copy), then
   `ThreadProgram` clones, then `run_with_pool` copies bytecode again.
   `coil-embed` itself is ~1 MiB here; a full-`coil` runner is ~6 MiB.

Not a process-global spool leak: spool/native lock is packaging/FFI cache,
not per-worker heaps.

**Expected vs bug:** the isolate + 8 MiB stacks + lazy pool are intentional.
The MAX-slot `.hyc` heuristic and per-job bytecode memcpy are **blunt /
wasteful**, not IPA semantics.

## Symptom 2 — little wall-time win vs sequential

**Verified** on the same `fib(32)`: ~1.6× wall on 4 cores (50 ms → 31 ms),
not ~4×. `workers=1` IPA is **slower** than sequential (~54 ms).

Causes:

1. **Two-way AlwaysPar.** One spawn, one inline arm, join. Nested specs
   still fork all the way down to `fib(21)`. Near-cutoff nodes pay
   `MakeFn` + `HostInvoke` spawn/join + `Result` packing for little work.
   [auto-par.md](auto-par.md) already warns that forking below the
   threshold is typically slower; nested AlwaysPar recreates that at the
   leaves of a large tree.
2. **Per-job isolate tax** (`load_program` memcpy, natives clone, cold
   heap, help-VM construct) on the join path.
3. **Flagship sequential benches often never spawn.**
   [optimization-roadmap.md](optimization-roadmap.md): with the work
   score, `mandelbrot` / `tak` / `nsieve` / `binary_trees` stay sequential
   at the default threshold, so auto-par on vs off is noise — and RSS
   should *not* spike unless some other `thread::spawn` runs.
4. **Cargo test default.** [`.cargo/config.toml`](../../.cargo/config.toml)
   sets `COIL_MAX_WORKER_THREADS=1` (`force = false`). `cargo run` of a
   forking program looks like “auto-par does nothing / is slower.” Direct
   `./target/release/coil` uses `available_parallelism`.

**Expected vs bug:** limited speedup on `fib` is expected given the
fork shape and isolate cost. Not a scheduler deadlock in the fib case
(pool starts; 4-worker run is faster than 1-worker).

## Symptom 3 — `.hy` faster than `.hyc` / packaged

**Not reproduced** on `fib(32)` or a 2e7-trip dense loop: sequential wall
matched (~50 ms / ~234 ms); IPA `.hyc` was slightly **faster** than IPA
`.hy` (no compile in the timed `.hyc` path). Sequential `.hy` RSS is
higher (~8 MB vs ~4.3 MB) because `cmd_build_and_run` keeps the compiler
mapped.

If a workload still shows `.hy` winning on wall time, these code
differences are the candidates (ranked by how much they can move RSS or
GC, not by whether they showed up on fib):

| Difference | Compile-and-run (`.hy`) | `.hyc` / `coil-embed` |
|------------|-------------------------|------------------------|
| Operand stack | Analyzed slots (`Pipeline::operand_stack_slots`) | `archive_operand_slots`: 256 or **MAX** if any `Seek`+`CALL` |
| S2b stack maps | Installed (`wire_thread_program_with_maps`) | **Dropped** (`stack_maps: Vec::new()` in `execute_archived_program`). Bytecode was still *compiled* with maps; the interpreter just cannot use them for GC relocate |
| Bytecode copies | `run_raw` + `Arc` image | rkyv deserialize + `loaded.bytecode.clone()` into `ThreadProgram` + `run_with_pool` copy + per-job `load_program` |
| Debug sidecar | In-memory `program_debug` | Full `debug_locs` (one per byte) deserialized; `fn_symbols` cleared on package **and** on load |
| Embed I/O | N/A | Whole exe `read` into a `Vec`, then slice the trailer |

Fat LTO (`[profile.release] lto = "fat"`) links the interpreter into both
`coil` and `coil-embed`. `Machine::execute` is `#[inline(never)]` so the
compiler crate should not paste into dispatch; i-cache still differs
because `coil` is ~6× larger than `coil-embed`.

A common **measurement mix-up** (verified): `COIL_AUTO_PAR=0` on
`coil run foo.hyc` does not sequentialize an IPA archive. Compare
archives compiled with the same env, or recompile.

## Recommended next fixes (no semantics change)

Do not change AlwaysPar / threshold without a hit bench. Ranked:

1. **Persist `operand_stack_slots` in `.hyc`** (archive **minor** bump) and
   use it in `execute_archived_program` / embed. Same bound as
   compile-and-run. Removes the `Seek`+`CALL` → 1 MiB-slot hammer.
   Highest leverage for packaged RSS if that heuristic is live.
2. **Execute workers from `Arc<[Byte]>`** — skip `load_program`’s
   `to_vec` when `program_code` can borrow/pin the `ThreadProgram` arc.
   Cuts per-job CPU that eats IPA speedup.
3. **TLS helper VM for join-help** instead of `Box::new(Machine)` per
   stolen job. Directly targets the `workers=1` RSS spike and fork-join
   construct cost.
4. **Reset or bound worker heaps** after a job (or reuse one heap with a
   generation). Isolates stay correct; peak RSS should follow live data,
   not the high-water of every job on that worker.
5. **Persist S2b maps** in the archive (another minor) so `.hyc` GC
   matches compile-and-run. Needed before claiming packaged dense GC is
   identical to `coil file.hy`.
6. **Embed: mmap / slice the exe** instead of `fs::read` of the whole
   binary. Small for `coil-embed`; large if packaging falls back to full
   `coil`.
7. **IPA policy (later, evidence-gated):** only fork the *top* profitable
   site, or raise nested cutoff, so `fib(32)` does not AlwaysPar every
   `n∈(20,32]`. That *is* a semantics/perf policy change — not this doc’s
   patch.

## What not to do

- Do not treat `COIL_AUTO_PAR` as a runtime VM switch.
- Do not start the reactor at `Machine` construction (would spike RSS on
  sequential programs).
- Do not drop isolate heaps without a sendability story; workers cannot
  share the root `Heap` today.

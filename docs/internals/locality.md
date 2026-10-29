# Locality and loop optimizations

`feature/locality`: transforms that change which work the VM runs and which
memory it touches, without changing what a program computes. Step PRs go
into the feature branch; the branch merges into `main` when every step is
in. Assessment and probes: the "Reordering and locality optimizations for
Coil" doc (2026-10-28).

## Why these, on a bytecode VM

The CPU runs the interpreter, not Coil's instructions, so reordering
bytecode for latency or pipeline slots buys nothing. What pays is fewer
dispatches, fewer heap accesses, and data laid out in the order it is
read. Native-style scheduling, tiling, partial unrolling and field
reordering measured as no gain and are out of scope.

## Steps

| Step | What | Probe gap (before) | Status |
|------|------|--------------------|--------|
| 1 | Vectorize inner loops that index `a[base + j]` with a base the inner loop does not change (flattened 2D arrays in a nest) | `nested.hy`: `a[row + j]` 274 ms vs `r[j]` on a row 19 ms (14x), 1024² x 20 | planned |
| 2 | Vectorize one loop that both stores a map and folds a reduction | `fusion.hy`: fused 417 ms vs split 162 ms (2.6x) | planned |
| 3 | Field-sensitive mod/ref summaries (which slots and array kinds a call may write) | enables 4 | planned |
| 4 | Scalar promotion: keep an object field in a register across a loop, store it back on exit and on the panic path | `promo.hy`: 3.1x (8 vs 3 dispatches per iteration) | planned |
| 5 | Copy the objects a pointer array holds in index order when the nursery promotes or evacuation compacts | `objorder.hy`: 24 vs 147 ns per object (6.2x) | planned |
| 6 | Prefetch the object behind `a[i + k]` in counted scans of pointer arrays | part of step 5's gap | planned |
| 7 | Loop interchange for affine nests | `interchange.hy`: rows 277 ms vs columns 749 ms (2.7x) | planned |

Probes: `/mnt/project-files/reordering/probes/` (project files). Numbers are
release builds, `--auto-par off --fold-calls off`, median of 5, setup time
subtracted.

## Semantics rules every step keeps

1. **Panics are precise.** Overflow, division by zero and failed index
   checks panic; `defer` pads, `drop` and a joining task can observe the
   heap afterwards. No store moves across a trapping op unless the panic
   path writes it back.
2. **Effects are barriers.** Host, FFI, IO, `yield`, park/attach, GC and
   unknown calls keep their order (`common::EffectFlags`).
3. **Tasks and threads.** A suspension point ends a promotion. An object
   another thread can reach is never promoted.
4. **Copy-on-write.** The uniqueness check runs where the first write
   was; `shared class` and `Ref<T>` are refused.
5. **`drop` timing.** Classes with `fn drop()` keep their allocation and
   free points.

## Progress log

Numbers land here as each step merges.

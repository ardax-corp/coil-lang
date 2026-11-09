# Locality and loop optimizations

`feature/locality`: transforms that change which work a program does and which
memory it touches, without changing what a program computes. Step PRs go
into the feature branch; the branch merges into `main` when every step is
in. Assessment and probes: the "Reordering and locality optimizations for
Coil" doc (2026-10-28).

## Why these

The steps were chosen and measured on the bytecode VM, since deleted, where
fewer instructions, fewer heap accesses and data laid out in read order paid.
Every time on this page comes from that VM. The passes below run in `mir::specialize::optimize_mir`.

## Steps

| Step | What | Probe gap (before) | Status |
|------|------|--------------------|--------|
| 1 | Vectorize inner loops that index `a[base + j]` with a base the inner loop does not change (flattened 2D arrays in a nest) | `nested.coil`: `a[row + j]` 274 ms vs `r[j]` on a row 19 ms (14x), 1024² x 20 | done |
| 2 | Vectorize one loop that both stores a map and folds a reduction | `fusion.coil`: fused 417 ms vs split 162 ms (2.6x) | done |
| 3 | Field-sensitive mod/ref summaries (which fields a call may read or write) | lets step 4 keep a field across a call that cannot touch it; `promocall.coil`: 1327 vs 985 ms | done |
| 4 | Scalar promotion: keep an object field in a register across a loop, store it back on exit and on the panic path | `promo.coil`: 3.1x (8 vs 3 VM instructions per iteration) | the pass exists; nothing runs it (see below) |
| 5 | Copy the objects a pointer array holds in index order when the nursery promotes them | `objorder.coil`: 24 vs 147 ns per object (6.2x); `young_shuffle.coil` | done (minor collections) |
| 6 | Prefetch the object behind `a[i + k]` in counted scans of pointer arrays | part of step 5's gap; `scattered_scan.coil` | marked in MIR; no engine reads the mark (see below) |
| 7 | Loop interchange for affine nests | `interchange.coil`: rows 277 ms vs columns 749 ms (2.7x); `fmapcols.coil` | done (non-trapping map bodies) |

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
3. **Tasks and threads.** A suspension point ends a promotion. No other
   thread writes an object a loop holds: `thread::spawn` copies, and
   auto-par shares only frozen or immediate values.
4. **Copy-on-write.** The uniqueness check runs where the first write
   was; a loop holding a `Cow` is refused.
5. **`drop` timing.** Classes with `fn drop()` keep their allocation and
   free points.

## Progress log

Numbers land here as each step merges.

- **Step 1** (`vec_loops` value offsets): a stream's lanes may sit at
  `j + base` with `base` invariant in the loop (`vectorize::Off::V`). The
  guard checks `i0 >= 0`, `0 <= base <= len` with compares only, then
  `n <= len - base` in a second guard block, so building the guard never
  overflows. A store loop with a value offset takes only loads at that
  same value. `nested.coil` 1024² x 20: flat sum 274 → 32 ms (now equal to
  the row-of-rows walk), flat map `b[row + j] = a[row + j] * 3` 465 →
  106 ms.
- **Step 2** (map-and-fold vector loops): `VecLoopKind::Reduce` carries
  the body's stores, so `b[i] = v; s = s + v` is one `VLoop` (stores, then
  the fold). The same store-loop rules hold (reads before the first store,
  one store offset, no read behind it), and each trapping instruction must
  come after as many stores as the vector body runs before it
  (`vectorize::traps_in_store_order`). The lane code defers a block's stores
  until its other words ran; a block that traps restores its fold slots
  and runs again one lane at a time (`machine::lanes::vloop_lanes`), so the
  stores before the panic are exactly the scalar loop's
  (`tests/collections/vec_loop_panic_stores.coil`).
  `fusion.coil` 4M x 5: fused loop 417 → 167 ms, equal to the two split loops
  (161 ms, unchanged).
- **Step 4** (`mir::promote_fields`, since removed: no capture ever ran it,
  since neither Cranelift nor the interpreter could honour its
  `MirInst::PanicStore`s). The pass did this: a field
  a loop writes, touched only through one object made before the loop and
  holding an `int` or `float`, is read once before the loop, carried in a
  φ and stored once on each exit. The loop may hold arithmetic, compares,
  casts, element and field reads and writes, explicit panics and pure
  natives; a call, allocation, yield or any other effect refuses it, as
  does a second object value touching the slot. Before each instruction
  that may trap the pass places `MirInst::PanicStore`s naming the word each
  field holds there, which a backend must store before the unwind goes on.
  `promo.coil` 1M x 20 on the VM: field accumulation 820 → 270 ms, equal to the
  hand-written locals (265-300 ms); the loop body went from 8 ops to 4.
- **Step 5** (GC copy order): a minor collection copies the young objects
  an array reached first, in the array's index order, then the rest in
  nursery (allocation) order as before (`Heap::minor_plan`). Marking pushes
  only what it newly marks, so an array's share of the gray stack is its
  new elements in order: no second walk. Fresh old-space chunks are now
  handed out in address order (`Slab::carve_slots` reverses the slots it
  files, since the free list pops from its back); before, consecutive
  copies landed at falling addresses. `examples/perf/young_shuffle.coil`
  (16384 objects shuffled while young, then 2000 walks): 979 → 658 ms,
  equal to the unshuffled walk (657 ms). `binary_trees` (depth 18),
  `gc_churn`, `gc_rebuild`, `gc_frag`, `region_rounds`: flat within run
  noise. Limit: objects already old when a program reorders them stay put
  (old chunks move only when sparse evacuation picks them), so
  `objorder.coil`'s 1M-object shuffle is unchanged. Closing that needs a
  major collection that relocates for order, which is a separate decision.
- **Step 7** (loop interchange, `mir::interchange_loops`): a perfect
  two-loop nest whose body is one block and indexes every array at the
  same `inner * stride + outer` swaps its loops, so the inner loop walks a
  row and step 1's vectorizer takes it. It runs after the first
  `loop_opts`, and `loop_opts` runs again on what it changed. Exactness:
  every iteration touches one distinct cell and the body cannot trap, so
  any order gives the same stores. A guard in front checks both ranges
  are non-empty and non-negative, `stride >= outer extent`, that the
  largest index does not overflow, and that it is in bounds for every
  array; otherwise a clone of the source nest runs, and an out-of-bounds
  index panics after the same stores as before. The body may hold
  non-trapping `Const` / `Bin` / `Cmp` / `Unary` and int-to-float casts,
  so float maps, bit-op maps and copies qualify. `fmapcols.coil` (2048²
  float map, column order, 4 passes): 1303 → 298 ms, equal to row order
  (295 ms). Limit: int arithmetic traps on overflow, and which cell
  overflows first is observable, so an int map like `* 3` stays in column
  order; so does a column reduction (`interchange.coil`), whose partial
  sums would overflow in a different order. Those would need a proof
  that no overflow happens (range analysis), which is not planned.
- **Step 3** (field summaries, removed): the HIR effect summary of each
  body carried `fields`, one bit per field name it or its callees might
  touch, and MIR kept the bits per call and per heap slot for
  `promote_fields`, which kept a slot in a register across calls that
  could not touch it. The pass went in #2343 and the summaries, the MIR
  tables and their archive encoding with archive major 8.
  `promocall.coil` (the step 4 probe with a non-inlined recursive call in
  the loop, 1M x 20) measured 1250-1370 → 930-1020 ms while it ran.
- **Step 6** (prefetch): `mir::mark_prefetch_scans` marks each `Index`
  that reads a heap word from an array made before its loop, at a header
  φ the loop steps up by a positive constant (`MirFunc::prefetch_loads`).
  Neither engine reads the mark, and nothing calls `ObjArray::prefetch_ahead`,
  so no prefetch is issued. On the VM it read ahead by 8 elements:
  `examples/perf/scattered_scan.coil` (1M old objects walked 10
  times in a scattered order): 1190-1290 → 910-1060 ms end to end, about
  840 → 540 ms for the walks alone. The ordered walk and an int-array
  scan with a branch are unchanged. A distance of 8, 16 or 32 measured
  the same.

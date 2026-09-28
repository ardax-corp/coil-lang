# Moving GC — design proposal (not implemented)

**Status:** proposal for review (2026-09). Nothing here is on `main`.
[heap-identity.md](heap-identity.md) and [gc-incremental.md](gc-incremental.md)
still list moving / compacting as refused or deferred; this note is the case
for lifting that, and the order to do it in. No code lands until the plan is
accepted.

Today's collector is non-moving mark + lazy sweep over a mapped slab
(size-class free lists, 64 KiB chunks, header poison). `Value` is a raw
address. Payload `Vec`s (array elements, string bytes, large class / enum
spills) are ordinary Rust allocations outside the slab.

## What moving would buy — and the bar

1. **Compaction.** Evacuate sparse slab chunks so live objects pack densely,
   and return empty chunks to the OS (RSS after a peak). Better locality for
   header-heavy walks (`binary_trees`, `gc_churn` lists).
2. **Bump allocation (later, separate decision).** A copying nursery makes
   allocation a pointer bump and frees short-lived garbage without a sweep.
   That needs an old→young write barrier, which S4 deliberately removed.

Neither is free: every pointer the collector cannot find or update is silent
memory corruption. Per the hit-bench rule, **Stage 0 measures first** and the
work stops if the numbers do not justify it.

## Where heap addresses live today

| Location | Precise? | Plan under compaction |
|----------|----------|-----------------------|
| Frames with a trusted `PreciseFrameMap` (most bodies) | yes — listed heap slots | rewrite slot in place |
| Conservatively scanned frames: unmapped bodies, `yield from`, FFI, native re-entry (`call_function`), coroutine segments, dense frame extents | **no** | pin every object a word hits |
| Dense registers | same as their frame | same as their frame |
| `statics: Vec<Value>` | **no** (untyped words) | add a static heap bitmap from the checker; until then pin |
| `ObjInstance` / `ObjEnum` / `ObjBoxed` members | `Member::Object` yes; `Member::Value` resolved by lookup | `Object` rewrite; `Value` words: see below |
| `ObjArray` / `ObjTuple` elements, `ObjFn` captures | **no** — raw `Value`s resolved by `mark_value` | **needs element kinds** (Stage 2) or everything they reference pins |
| `ObjCoroutine::saved_stack` | precise when `saved_live_mask != 0`, else conservative | rewrite masked slots; pin otherwise |
| Tagged words: `Result` heap-heap `Err` = `ptr \| 1`, `Option` niche `0` | traced via `heap_addr()` (bit 0 stripped) | rewrite keeping bit 0 |
| `frame_pins` (`ArrayPin` / `IndexPin*`, dense `frame_pins` with address check) | holds `Object` + cached element access | invalidate (re-probe) after any move |
| `resume_stack` coroutine pointers, GC handle table (`Root` / `Weak`) | yes | rewrite |
| `program_string_cache` | not a root, already invalidated after GC | unchanged |
| Dense-index last-address cache (COI-372) | cache | invalidate |
| `immortal_enums` (unit variants) | yes | allocate in a **non-moving** immortal space |
| FFI: addresses handed to C, `userland_libraries` keyed by handle address, callback state | outside the VM's view | **pin** anything reachable from a `Root` used for FFI; key libraries by id, not address |
| Shared-heap steal worker stacks (C1/C2) | STW, maps mandatory | rewrite like the main stack |
| Address-derived identity (pointer `EQ` on unit enums, any address-keyed `HashMap<u64,…>` in the VM) | — | audit: unit enums never move (immortal space); every address-keyed map must be rekeyed or rebuilt after a move |

Payload data behind `Vec`s does not live in the slab, so moving a header
does not move element / byte buffers. A C pointer into a buffer stays valid
across compaction as long as the buffer is not reallocated (grow already
invalidates it today).

## Options

**A. Precise copying / generational nursery.** Requires zero conservative
frames, precise statics and aggregates, and an old→young write barrier on
every opcode store. Largest change; reverses S4's "no write barrier" and
needs a precise answer for FFI and native re-entry frames. Not first.

**B. Mostly-copying compaction (Bartlett style). Recommended.** Ambiguous
words (conservative frames, untyped statics / aggregates) **pin** the object
they hit; everything reachable only through precise references may move.
Evacuate at full collections only: pick sparse chunks, copy unpinned live
objects into dense chunks, leave a forwarding address in the poisoned
source header, then one fix-up pass rewrites precise roots and every heap
member. Works with today's mixed precise / conservative stack and keeps the
S4 safepoint mark (no write barrier).

**C. Handle table (`Value` = index).** Refused in heap-identity.md: an extra
indirection on every heap access and an archive-major change.

## Stage 0 results (2026-09, `--features gc-stats`)

Census after each mark (release build, `COIL_AUTO_PAR=0`):

| Workload | Mapped | Live | Reclaim by unmapping empty chunks | Extra by compaction | Pinned (ambiguous) |
|----------|-------:|-----:|----------------------------------:|--------------------:|-------------------:|
| Flagships (`mandelbrot`, `tak`, `nsieve`, `binary_trees`, `fib`) | — | — | never collect (under the 1 MB budget) | — | — |
| `gc_churn` steady state | 8.8 MB | 0 | **8.8 MB** | 0 | 0 |
| `result_heap_churn` | 1.1 MB | ~0 | 0.96 MB | 64 KB | 0 |
| Probe: 60k-node list, half copied to a new list, 20k nodes in a `Vec<Node>` | 7.6 MB | 3.8 MB | **3.7 MB** (65 empty chunks) | ~35 KB | 20k nodes (36–40% of live) |
| Probe: same list thinned **in place** (every other node unlinked) | 5.6 MB | 3.8 MB | 64 KB | **1.7 MB** (30%) | 20k nodes (36%) |

Findings:

1. **Most reclaimable memory is whole empty chunks, not fragmentation.**
   Allocation fills a size class sequentially, so a generation that dies
   together frees whole chunks. That needs no moving: idle-chunk release
   (`madvise(MADV_DONTNEED)`, chunk stays mapped and reads as poisoned) now
   ships separately — `gc_shrink.hy` RSS ~96 MB → ~40 MB after its peak,
   no regression on churn benches.
2. **Compaction only pays for survivors scattered in place** (~30% in the
   in-place-thinning probe), which is exactly the long-lived mutable
   structure case.
3. **Interior ambiguity dominates pinning.** Every object held only by a
   `Vec` / tuple / capture word is ambiguous today — 36–40% of the live set
   in the probe. Conservative *stack* roots were 0 in every run (precise
   frame maps cover these programs). Stage 2 (precise element kinds) is
   therefore the real prerequisite, not stack precision.
4. **Precise maps are type-precise, not liveness-precise.** A dead temp in a
   mapped slot kept a 60k-node list alive in the first probe; liveness in
   precise maps would reclaim more than compaction on such code.

Recommendation after Stage 0: keep idle-chunk release; do Stage 1 (landed
with it: `for_each_vm_root` / `Object::for_each_reference` tag every root
and interior reference precise / ambiguous / pinned) and Stage 2 before any
evacuation work; evaluate slot liveness in precise maps as a cheaper win.

## Plan (option B)

0. **Measure.** Fragmentation (live bytes vs mapped chunk bytes after each
   full collect) and alloc / sweep share of time on `binary_trees`,
   `gc_churn`, `result_heap_churn`, and a long-running allocation-heavy
   program (server-shaped). Also: how often conservative words would pin
   (count ambiguous hits per collect). **Stop here** if fragmentation and
   RSS-after-peak are small — compaction would not pay.
1. **Root classification, no behavior change.** Every root source reports
   `Precise(&mut Value)` or `Ambiguous(u64)` through one API; mark consumes
   it. Add a debug verifier: every precise root must be a live slot origin
   (catches map bugs before anything moves). CI keeps `gc-stress`.
2. **Precise aggregates and statics.** Record whether an `ObjArray` /
   `ObjTuple` / capture list holds heap words (the element type is known at
   compile time: `MakeArray` / `MakeTuple` operand bit, or a runtime kind
   byte set on first store), and emit a static heap bitmap. Without this,
   everything referenced from a `Vec<Obj>` pins and compaction barely helps
   array-heavy heaps. Archive minor bump.
3. **Pinning + non-moving spaces.** Immortal unit enums and FFI-reachable
   objects go to non-moving chunks; library handles keyed by id.
4. **Evacuation behind a feature** (`gc-compact`), plus a
   `gc-stress-compact` CI job that moves every movable object at every
   collection — the cheapest way to find a missed pointer. Invalidate pins
   and address caches after a move.
5. **Default on** only after hit benches win (RSS after peak, `binary_trees`
   / `gc_churn` wall) with no flagship regression; unmap empty chunks.
6. **Nursery / bump allocation** — separate proposal (needs a write barrier).

**First PR if accepted:** Stage 0 measurements plus the Stage 1 root API and
verifier. Both are behavior-neutral.

## Risks

- A missed precise pointer corrupts memory silently. Stage 1's verifier and
  `gc-stress-compact` are the mitigation; do not skip them.
- Ambiguous words that happen to be integers pin garbage for a cycle
  (bounded, same as today's conservative retention).
- The fix-up pass is O(heap) at full collections; evacuation must stay off
  the incremental sweep path.
- Shared-heap steal must be at a STW safepoint for any move (already true).

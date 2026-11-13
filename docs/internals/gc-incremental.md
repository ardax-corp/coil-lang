# GC: safepoint mark + lazy sweep (S4 / COI-309)

How a major cycle runs. Moving objects is separate: minor collections
([escape-memory.md](escape-memory.md)) and evacuation
([gc-evacuation.md](gc-evacuation.md)).

## Landed

Mark is **stop-the-world at an alloc safepoint**; only the sweep is spread
across safepoints:

1. **O(1) root seed** — gray is seeded with `find_object_by_addr` (slab +
   header poison). The collector no longer walks the intrusive list to match
   root addresses.
2. **Safepoint mark** — `begin_mark` then drain gray to completion, remark the
   roots, drain again. The mutator never runs while there are gray objects.
3. **No write barrier for marking.** Because gray is empty whenever user code runs, a store
   can only move a pointer to an already-marked object. The one exception is a
   finalizer running before weaks are cleared: `gc::upgrade` may return an
   unmarked target, so `Heap::resurrect_during_mark` marks it and drains before
   returning. Finalizable objects are marked with `Heap::shade_for_finalizer`
   before their `drop` runs. New objects allocated while a cycle is open are
   **black**. (The earlier Yuasa SATB deletion barriers on vec / IO / unroot
   never fired with an empty gray list and were removed. The nursery has a
   barrier of its own, for the remembered set: [escape-memory.md](escape-memory.md).)
4. **Lazy sweep** — after weaks are cleared, `sweep_quantum` walks the slab's
   resident chunks slot by slot from a cursor (`gc_sweep_quantum` objects,
   doubled under pressure; free slots are skipped) and frees unmarked
   objects. There is no per-object list link. An allocation during sweep
   may land ahead of the cursor, so it is tagged `fresh`: the sweep keeps it
   and clears the tag (and any mark) instead of freeing it. One allocated
   behind the cursor keeps the tag to the next sweep, which is harmless
   floating garbage for at most one cycle.

`Heap::collect` and `collect::collect` finish any in-flight sweep, then
drain mark + sweep so `gc::collect()` still reclaims in one call (`gc_churn`).

Objects do not move during mark or sweep. Evacuation runs between cycles
([gc-evacuation.md](gc-evacuation.md)).

## Pacing

A safepoint with the collector idle starts a major cycle when one of two
budgets is spent (`Heap::should_collect_old`):

- **Old bytes.** Old-space bytes pass `gc_next_threshold`, which the end
  of each sweep sets to what the mark kept times the growth factor (2, or
  4 while most of the heap survives), never under 1 MiB.
- **Held chunks.** Nursery chunks held for pinned survivors
  ([escape-memory.md](escape-memory.md), step 4) reach
  `held_budget`: the held chunks the last major found live plus a
  nursery's worth or that many again, whichever is more
  (`Heap::set_held_after_major`). Only a major frees a held object, so
  stale pins (a dead frame word) still get their chunks back after a
  nursery's worth.

Both budgets grow with what the last major kept, so the marking a run
does is linear in what it allocates, and each bounds its part of the heap
to about twice the live part. A fixed held budget broke this (#2350): a
stdlib `HashMap`'s values sit in a `Vec<V>` that a shared generic body
made (no per-type clone), so its elements are words of unknown kind and
every minor pinned every value. A generic `Bag<V>` at `--clone-budget 0`
does the same.
Once 32 chunks were held and live, every safepoint started a major that
marked the whole map again. Filling a `HashMap<int, Node>` (release,
`instructions:u`, best of 3, peak RSS):

| entries | majors | before | after |
|---|---|---|---|
| 100,000 | 235 → 2 | 6.37 G, 310 ms, 25 MB | 0.48 G, 37 ms, 27 MB |
| 400,000 | 678 → 3 | 42.4 G, 2374 ms, 63 MB | 2.01 G, 118 ms, 70 MB |
| 1,600,000 | 1081 → 4 | 176.3 G, 10393 ms, 210 MB | 8.06 G, 425 ms, 230 MB |

Instructions now grow as N^1.02. `examples/perf/gc_map_fill.coil` is
the bench.

## Memory return

After a sweep cycle the slab gives back pages of chunks idle for a whole
release window (see [heap-identity.md](heap-identity.md)). RSS drops after a
peak without moving anything: `examples/perf/gc_shrink.coil` goes from ~96 MB
to ~40 MB resident while it keeps running. Run it with a `coil-test` built
with `--features gc-stats` (`cargo build --release -p coil-test --features
gc-stats`, then `target/release/coil-test --show-output <dir>` on a
directory holding `gc_shrink.coil`; the runner takes a test root, not a
file) for a per-mark census (RSS, mapped / released slab, live bytes, reclaim by
unmap vs compaction, precise vs ambiguous roots and interior references) —
the numbers in [gc-evacuation.md](gc-evacuation.md). The same build
prints a line per minor collection
([escape-memory.md](escape-memory.md#remembered-arrays-2687)).

## Clean arrays

`ObjArray::may_hold_refs` lets marking skip an array proven reference-free:
the flag clears when a mark scans every element and none lies in the slab
range, and any element write sets it again (one byte, no compare — every
write goes through the `ObjArray` API; `elements` is private). A live
`Vec<int>` is therefore scanned once, not every collection
(`examples/perf/gc_int_vec.coil`: −7% instructions). Not type-based, so boxed
values from generic shared bodies are safe. Cost: the flag test on every
element store (+2.6% instructions on the tight `stride_store_iv` loop,
`nsieve` neutral). For a moving collector a clear array pins nothing.

## Deferred

- Incremental mark interleaved with the mutator (would need a write barrier
  for marking on every store)
- Handle-table `Value` (refused)
- Concurrent mark on OS worker threads
- Multi-mutator GC while steal jobs run. Shared-heap C0 is STW (epoch
  collect-after-join first, cooperative handshake if a steal must collect).
  See [shared-heap-sendability.md](shared-heap-sendability.md). GC stays
  single-mutator.

## Invariants

- Resurrection of `drop` is still allow-once (finalizers run after mark
  drains; queued instances are shaded and marking continues).
- Weak handles clear at the mark→sweep transition, before any reclaim.

## Finalizer and weak side lists (#1870)

Before each sweep a cycle must find two kinds of object: unmarked objects that still owe a `drop`, and `Weak` handles whose referents died. It no longer walks every slab slot to find them. The heap keeps two address lists: `Heap::finalizable` and `Heap::weaks`.

- **Joining.** A `Weak` joins its list at allocation. An instance or enum joins the finalizable list in three cases:
  - it is allocated with a `type_id` that has a `drop` (a bitset mirrors the runtime's finalizer registry);
  - `TagEnumType` gives it such a type later;
  - its type is registered after it was allocated, which costs one heap walk at registration.
- **Superset.** Both lists may hold more than the live candidates. Each pass sorts and dedups its list. It drops an entry whose slot is poisoned, which happens when a sweep freed it. It also drops an entry whose slot now holds another kind of object, a weak that is already cleared, or an object that is already finalized. An evacuation forwards both lists before it frees the old slots. Epoch allocations collect their entries in the thread's batch and fold them in when the batch flushes.
- **Cost.** Each cycle now costs O(candidates) instead of O(heap). Measured with `callgrind` on a 300k-object live heap plus 3M short-lived objects:
  - one live `drop` type: overhead went from 4.6% to 1.3% of instructions;
  - one `Weak`: overhead went from 1.7% to none.

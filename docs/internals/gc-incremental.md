# GC: safepoint mark + lazy sweep (S4 / COI-309)

Interpreter alloc + GC path tax on tree/churn benches. **Not** a moving collector
and **not** Cranelift / a register VM.

## Landed

Mark is **stop-the-world at an alloc safepoint**; only the sweep is spread
across safepoints:

1. **O(1) root seed** — gray is seeded with `find_object_by_addr` (slab +
   header poison). The collector no longer walks the intrusive list to match
   root addresses.
2. **Safepoint mark** — `begin_mark` then drain gray to completion, remark VM
   roots, drain again. The mutator never runs while there are gray objects.
3. **No write barrier.** Because gray is empty whenever user code runs, a store
   can only move a pointer to an already-marked object. The one exception is a
   finalizer running before weaks are cleared: `gc::upgrade` may return an
   unmarked target, so `Heap::resurrect_during_mark` marks it and drains before
   returning. Finalizable objects are marked with `Heap::shade_for_finalizer`
   before their `drop` runs. New objects allocated while a cycle is open are
   **black**. (The earlier Yuasa SATB deletion barriers on vec / IO / unroot
   never fired with an empty gray list and were removed.)
4. **Lazy sweep** — after weaks are cleared, `sweep_quantum` walks the slab's
   resident chunks slot by slot from a cursor (`gc_sweep_quantum` objects,
   doubled under pressure; free slots are skipped) and frees unmarked
   objects. There is no per-object list link. An allocation during sweep
   may land ahead of the cursor, so it is tagged `fresh`: the sweep keeps it
   and clears the tag (and any mark) instead of freeing it. One allocated
   behind the cursor keeps the tag to the next sweep, which is harmless
   floating garbage for at most one cycle.

`Heap::collect` and `Machine::gc_collect` finish any in-flight sweep, then
drain mark + sweep so `gc::collect()` still reclaims in one call (`gc_churn`).

Objects do not move during mark or sweep. Evacuation runs between cycles
([gc-evacuation.md](gc-evacuation.md)).

## Memory return

After a sweep cycle the slab gives back pages of chunks idle for a whole
release window (see [heap-identity.md](heap-identity.md)). RSS drops after a
peak without moving anything: `examples/perf/gc_shrink.hy` goes from ~96 MB
to ~40 MB resident while it keeps running. Build with `--features gc-stats`
for a per-mark census (RSS, mapped / released slab, live bytes, reclaim by
unmap vs compaction, precise vs ambiguous roots and interior references) —
the numbers in [gc-evacuation.md](gc-evacuation.md).

## Clean arrays

`ObjArray::may_hold_refs` lets marking skip an array proven reference-free:
the flag clears when a mark scans every element and none lies in the slab
range, and any element write sets it again (one byte, no compare — every
write goes through the `ObjArray` API; `elements` is private). A live
`Vec<int>` is therefore scanned once, not every collection
(`examples/perf/gc_int_vec.hy`: −7% instructions). Not type-based, so boxed
values from generic shared bodies are safe. Cost: the flag test on every
element store (+2.6% instructions on the tight `stride_store_iv` loop,
`nsieve` neutral). For a moving collector a clear array pins nothing.

## Deferred

- Incremental mark interleaved with the mutator (would need a real write
  barrier on opcode stores, not only host natives)
- Default-on compaction; handle-table `Value` (refused)
- Generational / nursery split
- Concurrent mark on OS worker threads
- Cranelift, PGO, register-VM, extra MIR island score-chasing
- Multi-mutator GC while steal jobs run. Shared-heap C0 is STW (epoch
  collect-after-join first, cooperative handshake if a steal must collect).
  See [shared-heap-sendability.md](shared-heap-sendability.md). GC stays
  single-mutator.

## Invariants

- Resurrection of `drop` is still allow-once (finalizers run after mark
  drains; queued instances are shaded and marking continues).
- Weak handles clear at the mark→sweep transition, before any reclaim.
- Archive / opcodes unchanged (runtime-only).

## Finalizer and weak side lists (#1870)

Before each sweep a cycle must find two kinds of object: unmarked objects that still owe a `drop`, and `Weak` handles whose referents died. It no longer walks every slab slot to find them. The heap keeps two address lists: `Heap::finalizable` and `Heap::weaks`.

- **Joining.** A `Weak` joins its list at allocation. An instance or enum joins the finalizable list in three cases:
  - it is allocated with a `type_id` that has a `drop` (a bitset mirrors the VM's finalizer registry);
  - `TagEnumType` gives it such a type later;
  - its type is registered after it was allocated, which costs one heap walk at registration.
- **Superset.** Both lists may hold more than the live candidates. Each pass sorts and dedups its list. It drops an entry whose slot is poisoned, which happens when a sweep freed it. It also drops an entry whose slot now holds another kind of object, a weak that is already cleared, or an object that is already finalized. An evacuation forwards both lists before it frees the old slots. Epoch allocations collect their entries in the thread's batch and fold them in when the batch flushes.
- **Cost.** Each cycle now costs O(candidates) instead of O(heap). Measured with `callgrind` on a 300k-object live heap plus 3M short-lived objects:
  - one live `drop` type: overhead went from 4.6% to 1.3% of instructions;
  - one `Weak`: overhead went from 1.7% to none.

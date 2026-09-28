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
4. **Lazy sweep** — after weaks are cleared, `sweep_quantum` unlinks unmarked
   objects from a cursor (`gc_sweep_quantum`, doubled under pressure).
   Allocations during sweep go at list head and are not visited this cycle
   (unmarked; next mark treats them as white).

`Heap::collect` and `Machine::gc_collect` finish any in-flight sweep, then
drain mark + sweep so `gc::collect()` still reclaims in one call (`gc_churn`).

Objects **do not move**.

## Memory return

After a sweep cycle the slab gives back pages of chunks idle for a whole
release window (see [heap-identity.md](heap-identity.md)). RSS drops after a
peak without moving anything: `examples/perf/gc_shrink.hy` goes from ~96 MB
to ~40 MB resident while it keeps running. Build with `--features gc-stats`
for a per-mark census (RSS, mapped / released slab, live bytes, reclaim by
unmap vs compaction, precise vs ambiguous roots and interior references) —
the Stage 0 numbers in [moving-gc.md](moving-gc.md).

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

## Evacuation (`gc-compact`, experimental, off by default)

`--features gc-compact` adds mostly-copying evacuation of sparse chunks
(`machine/src/memory/compact.rs`), run after a finished sweep:

- **Every VM root pins** (stack — precise-map slots only say a word *may*
  be a reference — statics, pins, caches), as do targets of ambiguous
  interior words (arrays that may hold references, tuples, captures,
  `Member::Value`), `Root` / `Weak` targets, immortal unit enums and every
  kind other than instances, enums with payload, boxes, tuples and arrays.
  So the VM never needs pointer rewriting; only precise heap-interior
  references (`Member::Object`, masked coroutine slots) are rewritten.
- Chunks are chosen per size class, sparsest first, while their objects fit
  in the free slots of the chunks that stay; emptied chunks are released at
  once. At most 64 chunks per step; a capped step continues next cycle.
- Skipped while a re-entrant `call_function` is active (a host frame below
  may hold raw handles), in a shared-heap epoch, or under the debugger.
  Attempts back off from every 8 to every 256 collections while
  unproductive.
- `gc-stress` + `gc-compact` moves every movable object at every
  collection; CI runs `coil test` that way.

Default builds do not compile any of it. It is not default-on: on the
probes so far, allocation refills holes before evacuation pays, and a
copying step raises peak RSS. See [moving-gc.md](moving-gc.md) for the plan.

## Deferred

- Incremental mark interleaved with the mutator (would need a real write
  barrier on opcode stores, not only host natives)
- Moving / compacting GC, forwarding pointers, handle-table `Value`
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

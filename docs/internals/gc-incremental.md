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

# Incremental GC (S4 / COI-309)

Interpreter alloc + GC path tax on tree/churn benches. **Not** a moving collector
and **not** Cranelift / a register VM.

## Landed

Stop-the-world mark-sweep is still the completion path (`gc::collect`, tests,
finalizer remake). Mutator safepoints after heap alloc now run a **bounded**
slice of an incremental cycle:

1. **O(1) root seed** — gray is seeded with `find_object_by_addr` (slab +
   header poison). The collector no longer walks the intrusive list to match
   root addresses.
2. **Safepoint mark** — `begin_mark` / `mark_quantum` drain gray
   (`GC_MARK_QUANTUM`, doubled under pressure). The interpreter **finishes
   mark at the alloc safepoint** (plus a root remark) so the mutator does not
   run while `GcPhase::Marking`.
3. **Yuasa SATB** — `Heap::satb_shade_*` logs overwritten pointers. Host
   mutations (vec clear/pop/remove, `gc::unroot`, IO fills) shade. Opcode
   `SetField` / `StoreIndex` skip the barrier because mark is safepoint-atomic.
   New objects allocated while marking are **black**.
4. **Lazy sweep** — after weaks are cleared, `sweep_quantum` unlinks unmarked
   objects from a cursor. Allocations during sweep go at list head and are not
   visited this cycle (unmarked; next mark treats them as white).

`Heap::collect` and `Machine::gc_collect` finish any in-flight sweep, then
drain mark + sweep so `gc::collect()` still reclaims in one call (`gc_churn`).

Objects **do not move**. `relocate_mapped_slots` stays identity.

## Deferred

- Moving / compacting GC, forwarding pointers, handle-table `Value`
- Generational / nursery split
- Concurrent mark on OS worker threads
- Write barriers on stack stores (remark covers roots)
- Cranelift, PGO, register-VM, extra MIR island score-chasing

## Invariants

- Resurrection of `drop` is still allow-once (finalizers run when mark
  completes; incremental path shades the instance and continues marking).
- Weak handles clear at the mark→sweep transition, before any reclaim.
- Archive / opcodes unchanged (runtime-only).

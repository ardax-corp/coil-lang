# MIR stack maps (I5 roadmap)

I5 ([COI-300](https://linear.app/ardax/issue/COI-300/i5-alloc-gc-barriers-in-mir))
makes **alloc edges visible** in the SSA sidecar (`Alloc` + `GcBarrier`).
S2a ([COI-305](https://linear.app/ardax/issue/COI-305/s2a-live-root-sidecar-at-mir-gcbarrier-alloc))
fills **live-root lists** at those edges. It does **not** ship interpreter
frame maps or rooted native/JIT.

## What exists today

- `MakeArray` / `MakeTuple` / `MakeEnum` / `InitTyped` can lower to
  `MirInst::Alloc` (`heapref`) plus a `GcBarrier` safepoint.
- [`fill_live_roots`](../../compiler/src/mir/gc.rs) (on `MirBuilder::finish`
  and text parse) sets `GcBarrier.roots` and `MirFunc.gc_roots` to the live
  heap-word SSA values at the edge: the new object plus other live heap
  refs (params, prior allocs, niche words). `GcBarrier` dest is a token,
  not a second object. IL slots that held those values are recorded when
  the builder snapshotted `current_def` (`LiveRootSet.slots`).
- Dense specialize (`try_specialize_body`) still refuses allocating IL.
- MIR→LIR (`try_lower_abi_body` / `emit_lir`) **bails to fuse-IL** when it
  sees alloc or a GC edge (no S2b consumer yet).
- The interpreter GC already walks VM frames. Fuse-IL bodies that allocate
  stay on that path. Cranelift (P5) stays parked.

This is an **honest refuse**: do not assume SSA `HeapRef` values are
relocatable across a safepoint in a specialized or native body. Live-root
lists are a sidecar only.

## Later (not this island)

1. ~~**Live-root sidecar**~~ — **S2a (this note).** At each `GcBarrier` /
   `Alloc`, record which SSA values (and IL slots when known) are live
   heap words. Still compile to fuse-IL until a consumer exists.
2. **Slot / frame maps** — encode those roots for the interpreter or a
   deopt edge ([mir-deopt.md](mir-deopt.md), I7) so a collect can update slots. Archive bump only if the
   map is required at load. [COI-306](https://linear.app/ardax/issue/COI-306/s2b-slot-frame-stack-maps-for-interpreter-gc).
3. **Specialize across GC** — only after (2), and only for a body that
   actually emits the map. Default remains refuse.
   [COI-307](https://linear.app/ardax/issue/COI-307).
4. **Native / Cranelift** — parked (P5). Native must not keep an unmapped
   heap pointer across a helper or alloc. Do not invent rooted JIT here.

Write barriers (`GcBarrier` kind `write`) are named so later field
stores can mark them. They are not implemented. I6 marks impure
HostInvoke / CALL as effect barriers instead of growing GC maps.

See [mir-islands.md](mir-islands.md) (I5) and
[specialize-refuse.md](specialize-refuse.md).

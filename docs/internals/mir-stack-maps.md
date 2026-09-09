# MIR stack maps (I5 roadmap)

I5 ([COI-300](https://linear.app/ardax/issue/COI-300/i5-alloc-gc-barriers-in-mir))
makes **alloc edges visible** in the SSA sidecar (`Alloc` + `GcBarrier`).
S2a ([COI-305](https://linear.app/ardax/issue/COI-305/s2a-live-root-sidecar-at-mir-gcbarrier-alloc))
fills **live-root lists** at those edges.
S2b ([COI-306](https://linear.app/ardax/issue/COI-306/s2b-slot-frame-stack-maps-for-interpreter-gc))
encodes those roots as **slot / frame maps** the interpreter uses to root
and relocate mapped slots on collect.

## What exists today

- `MakeArray` / `MakeTuple` / `MakeEnum` / `InitTyped` can lower to
  `MirInst::Alloc` (`heapref`) plus a `GcBarrier` safepoint.
- [`fill_live_roots`](../../compiler/src/mir/gc.rs) (on `MirBuilder::finish`
  and text parse) sets `GcBarrier.roots` and `MirFunc.gc_roots` to the live
  heap-word SSA values at the edge: the new object plus other live heap
  refs (params, prior allocs, niche words). `GcBarrier` dest is a token,
  not a second object. IL slots that held those values are recorded when
  the builder snapshotted `current_def` (`LiveRootSet.slots`).
- [`try_build_draft`](../../compiler/src/mir/stackmap.rs) lifts allocating
  fuse-IL leftovers (`allow_alloc`) and encodes S2a slots per alloc site.
  After fuse/PC assign, [`bind_drafts`](../../compiler/src/mir/stackmap.rs)
  attaches [`FrameStackMap`](../../common/src/stack_map.rs) rows to those
  bodies. Compile-and-run installs them on the VM. `.hyc` load does **not**
  require maps (no archive bump): older archives stay conservative-stack GC.
- Dense specialize / MIR→LIR **may cross alloc** when
  [`has_real_maps`](../../compiler/src/mir/stackmap.rs) is true (S2c /
  [COI-307](https://linear.app/ardax/issue/COI-307/s2c-specialize-lir-across-alloc-when-maps-exist)).
  LIR-across-alloc is straight-line only (looping alloc stays fuse-IL so
  invert+fuse remains). Unmapped allocating bodies stay fuse-IL. S3
  heap-index / `ArrayLen` / `StoreIndex` ride MIR exec (dense residual
  or LIR). Dense+match stays I2 LIR.
- The interpreter GC walks VM frames. Mapped slots are extra roots and are
  rewritten if a live object address changes. Unmapped alloc bodies stay
  fuse-IL + conservative stack scan. Cranelift (P5) stays parked.

## Later (not this island)

1. ~~**Live-root sidecar**~~ — **S2a.** At each `GcBarrier` /
   `Alloc`, record which SSA values (and IL slots when known) are live
   heap words.
2. ~~**Slot / frame maps**~~ — **S2b (this note).** Encode those roots
   for the interpreter (and later deopt) so a collect can update slots.
   Archive bump only if load-time requires maps — S2b does not.
3. ~~**Specialize across GC**~~ — **S2c.** Mapped allocating bodies may
   take dense / LIR when otherwise eligible. Default remains refuse
   without maps.
4. **Native / Cranelift** — parked (P5). Native must not keep an unmapped
   heap pointer across a helper or alloc. Do not invent rooted JIT here.

Write barriers (`GcBarrier` kind `write`) are named so later field
stores can mark them. They are not implemented. I6 marks impure
HostInvoke / CALL as effect barriers instead of growing GC maps.

See [mir-islands.md](mir-islands.md) (I5) and
[specialize-refuse.md](specialize-refuse.md).

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
  `ArrayPush` / `DenseArrayPush` lower to `MirInst::ArrayPush` plus a
  barrier (B6 grow). `FORMAT` / `STRINGIFY` pair the same way (Q9 R3).
  Heap `GetField` / `SetField` / `LoadField` lower on the map path so
  CALL/`InitTyped`+field drafts bind (D1). Dense emit reconstructs those as
  `DenseFieldLoad` / `DenseFieldStore` and Object `DenseMakeObject` (D2).
  `bind_drafts` counts those opcodes and `DenseMake`. Escaping `self`
  stays boxed-once (Q2); I3 non-escaping stays unboxed.
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
  bodies. Compile-and-run and `.hyc` / embed load (archive **minor 14+**)
  install them on the VM via `wire_thread_program_with_maps`. Older
  envelopes load with empty maps and stay conservative-stack GC.
- Dense specialize / MIR→LIR **may cross alloc** when
  [`has_real_maps`](../../compiler/src/mir/stackmap.rs) is true (S2c /
  [COI-307](https://linear.app/ardax/issue/COI-307/s2c-specialize-lir-across-alloc-when-maps-exist)).
  S2d ([COI-314](https://linear.app/ardax/issue/COI-314/s2d-map-backed-looping-alloc-further-alloc-opts))
  lets **preheader** `Make*` + index loops take dense when maps exist.
  S2e ([COI-316](https://linear.app/ardax/issue/COI-316)) drops per-residual
  `Seek` restore after stack residuals.   S2l
  ([COI-322](https://linear.app/ardax/issue/COI-322)) tries mapped
  **in-loop** `Make*` dense after SROA/LICM and keeps it only when the
  reconstruct is Make*-free inside loops (LOAD/STORE boxing still loses;
  [s2d-inloop-make-tax.md](s2d-inloop-make-tax.md)). Compare-only leftovers
  may take LIR. Draft lift keeps inferred param types (not forced `heapref`)
  and snapshots the stack-IL map **before** dense replace so `DenseBin`
  bodies still bind. Post-loop-only `return [x]` after a counted loop stays
  fuse-IL so invert+fuse (COI-87) remains.   Unmapped allocating bodies stay
  fuse-IL. B6 maps `ArrayPush` and lets map lift type one-word `CALL` so
  CALL+alloc drafts bind (no silent refuse). S3b heap-index takes dense unpinned residuals; S2e leaves tell
  at the StorePop frame high-water (prologue `Seek` only). Dense+match
  stays I2 LIR.
- The interpreter GC walks VM frames. Mapped slots are extra roots and are
  rewritten if a live object address changes. Unmapped alloc bodies stay
  fuse-IL + conservative stack scan. Cranelift (P5) stays parked.

## Later (not this island)

1. ~~**Live-root sidecar**~~ — **S2a.** At each `GcBarrier` /
   `Alloc`, record which SSA values (and IL slots when known) are live
   heap words.
2. ~~**Slot / frame maps**~~ — **S2b (this note).** Encode those roots
   for the interpreter (and later deopt) so a collect can update slots.
   Archive **minor 14** (COI-359 E1) persists maps for `.hyc` / embed.
   Older same-major envelopes still load with empty maps.
3. ~~**Specialize across GC**~~ — **S2c.** Mapped allocating bodies may
   take dense / LIR when otherwise eligible. Default remains refuse
   without maps.
4. ~~**Looping alloc**~~ — **S2d / S2e.** Mapped preheader `Make*` + index
   may take dense. Per-residual `Seek` restore is gone (StorePop already
   returns tell to the dense frame). Residual in-loop `Make*` stays
   fuse-IL unless SROA/LICM deletes it (S2l). Still refuse: post-loop-only
   heap return (invert+fuse); computed-element stack scalarize; compiler
   write-barrier opcodes. Mapped `ArrayPush` / CALL+`Make*` is B6. Class
   `new` / field maps are D1. Sibling / mutual `TailCall` is B7; self
   two-slot `CALL` / `RETURN` is C1 (cost gate).
5. **Native / Cranelift** — parked (P5). Native must not keep an unmapped
   heap pointer across a helper or alloc. Do not invent rooted JIT here.

Write barriers (`GcBarrier` kind `write`) stay named only. S4 SATB
already shades at VM field / vec stores; a compiler opcode would not
pay rent. I6 marks impure HostInvoke / CALL as effect barriers instead
of growing GC maps.

See [mir-islands.md](mir-islands.md) (I5),
[specialize-refuse.md](specialize-refuse.md), and
[opt-generalization.md](opt-generalization.md) (S2l cost gate: never
denser-but-slower by default; box once on escape).

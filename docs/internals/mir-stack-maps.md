# MIR stack maps (I5 roadmap)

I5 ([COI-300](https://linear.app/ardax/issue/COI-300/i5-alloc-gc-barriers-in-mir))
makes **alloc edges visible** in the SSA sidecar (`Alloc` + `GcBarrier`
placeholders). It does **not** ship precise stack maps or rooted native/JIT.

## What exists today

- `MakeArray` / `MakeTuple` / `MakeEnum` / `InitTyped` can lower to
  `MirInst::Alloc` (`heapref`) plus a `GcBarrier` safepoint whose `roots`
  list is a placeholder (identity of the new object, or empty).
- Dense specialize (`try_specialize_body`) still refuses allocating IL.
- MIR→LIR (`try_lower_abi_body` / `emit_lir`) **bails to fuse-IL** when it
  sees alloc or a GC edge.
- The interpreter GC already walks VM frames. Fuse-IL bodies that allocate
  stay on that path. Cranelift (P5) stays parked.

This is an **honest refuse**: do not assume SSA `HeapRef` values are
relocatable across a safepoint in a specialized or native body.

## Later (not this island)

1. **Live-root sidecar** — at each `GcBarrier`, record which SSA values
   (and eventually IL slots) are live heap words. Still compile to fuse-IL
   until a consumer exists.
2. **Slot / frame maps** — encode those roots for the interpreter or a
   deopt edge (I7) so a collect can update slots. Archive bump only if the
   map is required at load.
3. **Specialize across GC** — only after (2), and only for a body that
   actually emits the map. Default remains refuse.
4. **Native / Cranelift** — parked (P5). Native must not keep an unmapped
   heap pointer across a helper or alloc. Do not invent rooted JIT here.

Write barriers (`GcBarrier` kind `write`) are named so I6 / later field
stores can mark them. They are not implemented.

See [mir-islands.md](mir-islands.md) (I5) and
[specialize-refuse.md](specialize-refuse.md).

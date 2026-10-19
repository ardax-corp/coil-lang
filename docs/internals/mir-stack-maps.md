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
- `mir_maps_ok` checks that every allocation in a body's MIR is followed by
  its `GcBarrier`; dense / LIR emit across an alloc only then.
- **One map kind: precise frame maps.** Until 2026-10 every dense / LIR body
  also got an S2b [`FrameStackMap`](../../common/src/stack_map.rs) (archive
  **minor 14+**), drafted from MIR live roots in the registers emission
  assigns and bound to alloc PCs after fuse. The VM rooted its slots only
  where the frame's precise map had them stored (#967), and a frame with no
  trusted precise map is scanned anyway: a LIR body's `Seek`'d frame sits
  below the cursor, and an allocating dense body's registers are within its
  frame extent. So the S2b rows added no root (a `gc-stress` run of `tests/`
  that reported every S2b slot not otherwise scanned found none), and the
  compiler stopped emitting them. Archives still have the `stack_maps` field
  (written empty); the VM still reads and roots S2b maps an older archive
  carries. (Before M5 the drafts were lifted from stack IL by
  `try_build_draft`, and unmapped allocating bodies stayed stack-lowered;
  both are gone.)
- Mapped **preheader** and **in-loop** `Make*` may take dense (S2d / S2l,
  [s2d-inloop-make-tax.md](s2d-inloop-make-tax.md)); dense keeps such a body
  only when it wins the cost gate against the body's LIR form.
- The interpreter GC walks VM frames. Mapped slots are extra roots and are
  rewritten if a live object address changes. Cranelift (P5) stays parked.
- **Precise frames** (archive **minor 21**). A
  [`PreciseFrameMap`](../../common/src/stack_map.rs) describes a body's
  frame completely where it applies: `any_pc` (valid at every PC) or
  `at_pc` (exact PCs). The GC roots only the listed slots of such a frame
  and scans every other frame word by word, so a stack whose frames all
  have maps is not scanned at all.
  [`bind_precise_frames`](../../compiler/src/codegen/precise_frames.rs) runs
  on the final bytecode of every body (any tier):
  - heap-free functions (`fn_is_heap_free`: every parameter and expression
    has a numeric / `unit` / `never` checker type, no closures) whose
    bytecode has no heap-producing op get an empty `any_pc` map;
  - every other body gets a forward dataflow over the physical words of its
    frame (one "may hold a heap word" bit per word, a cursor range after
    joins of differing heights, callee clobber above a `CALL` base). It
    records the complete heap slots after each allocating / host op and the
    caller's words below each `CALL`'s arguments. Words above the cursor
    count only when last written as a slot (store, dense register, match
    payload). `JumpIfMatch` payload arity comes from lowering
    (`Lowered::match_arities`).
  Entry state comes from the callers: direct `CALL` / `TailCall` arity,
  the declared entry height (params, `self`, dictionaries) for bodies only
  code pointers or the host reach (tests, finalizers, callbacks, thread
  spawn), the `MakeCoro` arity for coroutine bodies (yields keep the words
  below the cursor), and `0` for `main` from the prologue. Closure bodies
  compiled inside a function (`CodePtr` fed to `MakeFn`) are analysed from
  their own entry with `[captures..., params]` and merged. A body is
  refused (no map, conservative) on an opcode the dataflow does not model
  (`yield from`, FFI), on any other entry into its middle, or when its
  entry height is unknown.
  The VM trusts a map only at a known PC: the top frame's safepoint PC, or a
  return PC that follows `CALL` / `CallIndirect`. A frame that entered
  native code which re-entered the VM (`call_function`), or that a coroutine
  segment starts above, stays conservative. Worker VMs share the maps by
  `Arc`. The `gc-stress` feature collects at every allocation safepoint;
  CI runs the language harness (`coil-test`) with it.
- **Frame extents** (archive **minor 22**). Allocating dense bodies need
  more than S2b maps: their registers can sit past the cursor, and
  generic host results are heap words typed `i64` that S2b maps miss. So
  every allocating dense body gets its frame extent (highest slot + 1,
  decoded from the final bytecode) in `PreciseFrameMap::frame_words`, and a
  conservatively scanned frame covers at least that many words. Only those
  bodies get an extent; widening every conservative frame would also root
  stale dead words (finalizers stopped running). A body whose ops do not all
  decode is scanned to the end of the stack.
- **Rebinds drop the old value** (#1402). The frame-word liveness keeps
  every word to the function's exit (scope lifetime), so a word only stops
  rooting when it is overwritten. HIR emission overwrites a local's slot on
  `x = e`; MIR gives each SSA value its own register, so `from_hir` emits a
  `ClearWord` of the old value after a heap local's rebind.
  `cleared_words` drops the zero while another live value shares that
  register, or when the block's back-edge φ copy rewrites it before
  anything can collect (a loop-carried local rebound in the body). The
  loop-entry copy of a rebound header φ leaves its source register holding
  the pre-loop value, so `entry_clears` zeroes that register once per loop
  entry. Constants root nothing and are skipped. Flagship `.hyc` are
  unchanged; on `examples/perf` only `gc_frag` (one zero per inner-loop
  entry), `gc_rebuild` (one per loop entry) and `gc_churn` (one fewer frame
  word) change, and their run times stayed within noise (medians of 11:
  `gc_frag` 303–353 → 326–341 ms, `gc_rebuild` 705–800 → 687–697 ms,
  `gc_churn` 144–147 → 149–151 ms, run-to-run spread larger than the gaps).
  `tests/memory/gc_weak_after_reassign.hy`.
- **Join copies drop the branch value** (#1409). An `if`/`else` that
  assigns a heap local on both arms meets in a φ; each arm's edge copy
  that is not coalesced leaves the arm's value in its own register, so a
  rebind after the join (a `ClearWord` of the φ) zeroed only the φ's word.
  `entry_clears` covers every forward `Jump` into a block whose φ is
  rebound, directly or through a nested join's φ copied into it, not only
  loop headers. A zero is skipped when no op that may collect (or trap, in
  a body with a cleanup pad) runs before the register is rewritten or the
  frame exits, and when the word is a cast (`Option::Some`) of a value
  another register still holds, since zeroing one copy cannot unroot it.
  Without those two skips `gc_churn`, `gc_frag`, `gc_int_vec`,
  `gc_rebuild` and `gc_shrink` gained a zero in their hot loop and ran
  5–15% slower (medians of 11, e.g. `gc_int_vec` 268–277 → 315–333 ms).
  With them flagship `.hyc` and every other `examples/perf` program are
  byte-identical to #1402; `gc_frag` loses one dead loop-entry zero
  (336 → 346 ms median, within noise). `tests/memory/gc_weak_after_join.hy`.
- **Copy sources drop the value too** (#1411). A cast copies its word into
  a register of its own (`Option::Some(x)` is a cast of `x`), and so does a
  φ, so after a rebind the registers the old value was copied from still
  root it until rewritten: in a loop, the previous trip's object until the
  next allocation. `source_clears` zeroes, beside each `ClearWord`, the
  cleared value's copy sources (casts and φ incomings, transitively, not
  past a live value) that no live value shares and that a collection can
  still scan before they are rewritten. Inside a loop it follows a φ only
  along in-loop forward edges, so a loop-entry value (which `entry_clears`
  zeroes once) or a back edge's is not zeroed on every trip. A loop whose
  join resets an `Option` on one arm and wraps a fresh object on the other
  (`gc_int_vec`) rebinds it right after the join, where the new value is
  live and the walk stops, so `entry_clears` also zeroes, on each forward edge into a rebound join, the
  copy sources of the other forward edges' values; the zeros land on the
  reset arm only. On `examples/perf` only `gc_int_vec` and `gc_shrink`
  change (two zeros on the arm taken once in 64 trips); flagship `.hyc` and
  every other program are byte-identical to #1409. Medians of 11,
  interleaved runs: `gc_int_vec` 272–289 → 267–285 ms (3 runs each),
  `gc_shrink` 435–485 → 443–477 ms (6 runs each), within the run-to-run
  spread.
  `tests/memory/gc_weak_some_in_loop.hy`.
- **Collection budget.** After a sweep the next threshold is
  `live × 4` when more than half the heap survived (a growing live set) and
  `live × 2` otherwise, never below the initial 1 MB budget.

## Later (not this island)

1. ~~**Live-root sidecar**~~ — **S2a.** At each `GcBarrier` /
   `Alloc`, record which SSA values (and IL slots when known) are live
   heap words.
2. ~~**Slot / frame maps**~~ — **S2b (this note).** Encode those roots
   for the interpreter (and later deopt) so a collect can update slots.
   Archive **minor 14** (COI-359 E1) persists maps for `.hyc` / embed.
   Older same-major envelopes still load with empty maps. Superseded by
   precise frame maps (minor 21+), which the compiler now emits alone.
   Shared-heap steal (C0) **refuses** a program with no frame maps —
   isolate + `PortableValue` instead
   ([shared-heap-sendability.md](shared-heap-sendability.md)).
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

Write barriers (`GcBarrier` kind `write`) stay named only. S4 marks
to completion before the mutator runs, so no store needs a barrier; a
compiler opcode would not pay rent. I6 marks impure HostInvoke / CALL as effect barriers instead
of growing GC maps.

See [mir-islands.md](mir-islands.md) (I5),
[specialize-refuse.md](specialize-refuse.md), and
[opt-generalization.md](opt-generalization.md) (S2l cost gate: never
denser-but-slower by default; box once on escape).

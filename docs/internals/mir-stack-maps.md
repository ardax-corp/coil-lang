# MIR stack maps (I5 roadmap)

I5 (COI-300)
makes **alloc edges visible** in the SSA sidecar (`Alloc` + `GcBarrier`).
S2a (COI-305)
fills **live-root lists** at those edges.
S2b (COI-306)
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
- **Copies and call arguments drop the value too** (#1625). A `match` or
  `if let` arm's binding of an `Option::Some`'s object is a cast of the
  scrutinee, which the dense tier gives a register of its own to pass to a
  call; a call whose arguments are not in consecutive registers also moves
  them into the gather window (`gather_base`), which no value owns. After
  the scrutinee was rebound both still rooted the object until rewritten.
  `source_clears` now also zeroes, beside each `ClearWord`, the cleared
  value's dead copies (`CopySinks`: casts, match payloads, and the
  elements of an enum or tuple it makes, transitively, not past a live
  value) and the gather registers a dense push copied the value or one of
  those copies into (`Gathers`; the LIR tier pushes from the value's own
  register and gathers nothing). The rewrite scan counts a push's gather
  writes as coming before the push, and `prune_clears` counts them as the
  block's writes, so a gather register the next push rewrites before
  anything can collect is not zeroed (a loop that rebinds a string it
  formats, `examples/variadic.hy`, keeps its loop as it was). No per-call
  zero is added: the zeros sit only at the clears that already exist.
  Flagship `.hyc` and all 223 `examples/*.hy` and `examples/perf/*.hy`
  archives are byte-identical to #1635. On a 3M-trip hit loop of the
  pattern itself (`junk = Some(new Node(i, None))`, an arm passing `n` to a
  call, `junk = None`), the loop gains one zero (two with a three-argument
  call that gathers); medians of 30 interleaved runs on a loaded 4-core
  host: 521 → 539 ms (one zero), 573 → 636 ms (two zeros), 437 → 455 and
  436 → 476 ms with the helper inlined. The zero is `CONST 0; STORE`, two
  dispatches; a dense `DenseConst r, 0` would be one, for every dense
  clear.
  `tests/memory/gc_weak_match_arg.hy`.
- **A temporary passed to a call drops too** (#1650). A word passed
  straight to a call (`weak(new Node(..))`) has no binding and so no
  `ClearWord`; its register, and the gather register a dense call copied
  it into, kept the object rooted until rewritten. `arg_clears` zeroes,
  right after a call (`Call`, `HostInvoke`, `CallIndirect`, not a tail
  call), the registers of the heap words it takes whose last use it is,
  and their gather registers, in both tiers (LIR: a call inside an
  expression tree gets its zeros after the tree's root, as below). Not
  for a local's
  word: the builder records each word `def_local` bound and each φ that
  merges a local (`MirFunc::bound_words`), and the frame keeps those
  until the local is rebound or the function returns, as HIR emission's
  slots do (a `gc::Root` held in a local guards its payload,
  `tests/memory/gc_frame_maps_collect.hy`). Nor for a parameter, a
  constant, a box of plain words (an `Option<int>` or a pair of numbers
  boxed to pass to `weak`: nothing else can hold that box, so a weak to
  it keeps upgrading until the register is rewritten,
  `tests/memory/gc_weak_option_arg.hy`), a register a value live past the
  call shares, or where no collection can come before the register is
  rewritten (`RewriteScan`). A call ending its block whose result the
  branch reads from the stack (`write(..)?`'s `JumpIfMatch`) gets its
  zeros at the top of each successor it is the only predecessor of, so
  the result still feeds the branch directly. The zero made a register
  dead at the allocations between its last push and the call
  (`precise_frames` drops words every path overwrites first), while a
  `CallIndirect`'s row was kept whole: an evacuation at such an
  allocation moved the object and left the register naming its old
  address, which the call's row then listed as a must-pointer (gc-stress,
  an over-applied closure in `tests/functions/closure_over_application.hy`).
  A `CallIndirect` row is now filtered by liveness like a `CALL`'s,
  keeping only its base word, which a partial application still uses.
  Straight-line code with calls of temporaries grows by `CONST 0; STORE`
  per such word (125 of the 223 `examples/*.hy` and `examples/perf/*.hy`
  archives change, mostly in `main`'s output calls); a 3M-trip loop
  passing `new Node(i)` to a recursive helper (two zeros a trip, the
  argument and its gather register) ran 0.511 s against 0.512 s user,
  medians of 15 interleaved runs on a loaded host.
  `tests/memory/gc_weak_temp_arg.hy`.
- **A temporary stored into an object drops too** (#1690). The same
  zeros follow every op that stores a word into an object, whose object
  holds the word from there on: `ArrayPush` (`v.push(new Node(..))`),
  `Alloc` (an enum, tuple, record or closure built from its elems:
  `Holder::One(new Node(..))`), `HeapFieldStore` (`new Pair(new
  Node(..))` sets each field), `StoreIndex` and `StoreStatic`, with the
  same exclusions. An op that only reads a temporary (`f().x`) is left
  as it was. Three cases a call does not have:
  - A push's array operand goes even when a local bound it: the push's
    dest is the same array, and the local is rebound to it. The old word
    otherwise kept the whole vec, and so every element, rooted after
    `v = Vec::new()`.
  - A field or element store's dest is the stored word (the VM leaves it
    on the stack). One nothing reads (`unused_store_dests`) is popped by
    the LIR tier instead of stored to a register; the dense tier, which
    moves it into the dest's register, zeroes that register after the
    store like an operand's (the zero never names a register a local's
    word shares).
  - The LIR tier emits a single-use operand inside its user (an
    expression tree: the enum alloc of `v.push(Holder::One(..))` is an
    inline operand of the push), so an op there has no place of its own
    for its zeros. They go after the tree's root (`EmitPlan::tree_roots`),
    checked there: no op up to the root writes the register or takes a
    local's word in it, and no value live past the root shares it. A
    root that is a terminator's operand, a tail call or a compare fused
    into the branch gets none.
  `tests/memory/gc_weak_temp_consumed.hy`.
- **A heap payload moving as an `i64` drops too** (#1769). A two-word
  enum (`Option`, `Result` or an enum of at most one payload word) passed
  by value travels as a pair, payload and tag, and a heap payload rides
  in it as an opaque `i64` word: a boxed enum is matched apart into a
  `MatchPayload` typed `i64` and a φ of it, or its object is cast to
  one. Not heap words, those registers were zeroed neither beside the
  rebound local's `ClearWord` nor after the call, and kept the object
  rooted until rewritten. `payload_words` marks them: an `i64`
  `MatchPayload` or cast of a heap word, and the `i64` φs and casts that
  copy one. `CopySinks` and `arg_clears` treat them as heap words, and a
  call (or other consuming op) taking one also zeroes the dead registers
  it was copied from (`payload_sources`: the φ's incomings, the match and
  the enum it read, the object cast to it, not past a live value nor
  into a register a local's word shares), since the LIR tier passes such
  a word as an inline operand with no register of its own. A number
  payload read the same way (`Option<int>` unboxed into a pair) is
  zeroed too: harmless, and its type is not known there. A pair a call
  returns (`f(make())` at `-O0`) was not covered until #1791 (below).
  Flagship `.hyc`
  and every `examples/perf` archive are byte-identical to #1690; of the
  225 `examples/*.hy` and `examples/perf/*.hy` archives that compile,
  only `thread_reply` changes (two straight-line zeros, a received string
  and the `stdout()` handle after their last call).
  `tests/memory/gc_weak_enum_arg.hy`.
- **A rebound field and a returned pair drop too** (#1791). A class
  local that never escapes is scalar-replaced: each field gets a frame
  slot of its own (`HirMirInput::sroa` / `fields`), and `b.n = e` lowered
  to a `def_local` of the field's slot with no `ClearWord` of the word
  it held, so the old field value stayed rooted in its register. The
  field store now clears the slot's old heap word after binding the new
  one, exactly as a local's rebind does (#1402), and the existing
  `cleared_words` / `entry_clears` / `source_clears` place or drop the
  zero. The fix sits in `from_hir`, not HIR SROA: HIR emission already
  overwrites the field's frame slot (which ends the old value), and only
  MIR gives the new value a register of its own.
  The low word of a pair a call returns is an `i64` too, so passing it
  straight on (`weak_in(make(3))` at `-O0`) kept its payload rooted.
  The MIR cannot tell a heap payload from a number there, so HIR
  emission lists the two-word expressions whose enum has a payload that
  is no scalar word (`HirMirInput::heap_pairs`, from the same variant
  payloads as `hir_mir_pair_tys`); `from_hir` marks such a call's low
  dest (`MirFunc::heap_pair_calls`), and `payload_words` takes it as a
  payload word. A number pair (`Option<int>`, an enum of ints) is not
  marked: zeroing every call's pair put two dead zeros in the hot loop of
  `examples/perf/match_enum_loop` at `-O0`. `Gathers` also take payload
  words now: a dense call copying one into its gather window (an
  inlined `make` in `weak_in(make(i))` in a loop) left the object rooted
  there after its other registers were zeroed.
  Of the 225 `examples/*.hy` and `examples/perf/*.hy` archives that
  compile, at `-O2` and `-O0`, only three change against #1769:
  `raise_try` (two straight-line zeros, a `Result` with a heap payload
  after the call that took it), `perf/s2g_escape_edges` (one straight-line
  zero of the rebound field `h.a` in `pack_field`) and, at `-O0` only,
  `perf/match_enum_loop` (one zero a trip after `score_res(wrap_res(i))`,
  whose `Result<int, string>` may carry a string; run time within noise,
  medians of 41 interleaved runs 55.9 → 54.4 ms).
  `tests/memory/gc_weak_field_rebind.hy`,
  `tests/memory/gc_weak_pair_result.hy`.
- **A niche `Option` built as an argument drops too** (#1802). An
  `Option` of a heap type is one word, and `Option::Some(new Node(..))`
  is a cast of the object's word (`word.niche_opt.heapref`) that the LIR
  tier emits as an inline operand of the call, with no register of its
  own: `arg_clears` found no operand register to zero, and the object's
  register is no operand of the call, so it kept the object rooted until
  rewritten (`weak_opt(Some(new Node(3)))`, or an inlined
  `weak_opt(some_node(3))`). `arg_clears` now walks a heap word cast from
  another heap or payload word back to its sources, as it does a payload
  word (`payload_sources`), with the same stops (a live value, a
  constant, a local's or a parameter's word, a register a local's word
  shares). Of the 233 `examples/*.hy` and `examples/perf/*.hy` that
  compile, at `-O2` and `-O0`, only `io_nested_host` and
  `io_nested_write` change against #1791: one straight-line zero each, of
  `open(..)`'s `Result` after the call its `?` passed the stream to
  (`read_to_end(open(..)?)`, `write_all(open(..)?, ..)`).
  Every `examples/perf` archive and listing is byte-identical.
  `tests/memory/gc_weak_niche_option_arg.hy`.
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

# MIR

MIR is typed SSA, and it is what every engine runs. Every function body lowers
from HIR to MIR. Native code ([native.md](native.md)) compiles it with Cranelift
and the interpreter ([mir-interp.md](mir-interp.md)) runs it directly, so a pass
here serves both. A body MIR cannot take is an internal error (`finalize_bodies` panics on a clean compile with one), and so is a call, function value or code pointer naming an entry no body was emitted for (a clone reserved and never compiled, #2580).

## Where it lives

`compiler/src/mir/`. Per body, in order:

1. `codegen/emit_hir.rs` plans the body (layouts, dictionaries, call targets),
   and `mir::lower_from_hir` (`from_hir.rs`) lowers HIR plus that plan to SSA.
2. While a capture is open, `capture_native_bodies` (`native_capture.rs`) copies
   each body and runs `specialize::optimize_mir` on the copy, at the compile's
   `-O` level. A raw capture skips that ([mir-interp.md](mir-interp.md#the-reference-output)).
3. `take_native_capture` hands the bodies to the engine, after `native_expand`
   writes some host calls out as MIR
   ([native.md](native.md#vec-natives-written-out-as-mir)).

Generators, bodies with a cleanup pad, and debugger or coverage builds keep their
MIR as lowered (`optimizable`). A coverage build's bodies get a `Probe` per source
location (`mir::coverage`).

| Piece | Role |
|-------|------|
| `MirTy` | Lattice: `bottom ⊑ {i64, f64, bool, heap-ref, niche Option/Result} ⊑ value` |
| `MirLayout` | Call-edge ABI: `word` / `twoslot` / `heap_niche` |
| `MirBuilder` | Braun SSA (locals are frame slots, explicit φ) |
| `lower_from_hir` | HIR body to SSA, for every body |
| `mir::archive` | MIR on disk ([archive-history.md](archive-history.md)) |
| `mir::frame_words` | What an engine needs about a body's frame words, shared by Cranelift and the interpreter |
| text form (`text.rs`) | Print and parse for round-trip tests; `coil dissect` prints it |

Language `int` / `float` / `bool` map to `i64` / `f64` / `bool`. `heapref`,
`niche_opt` and `niche_res` carry heap and niche `Option` / `Result` words.

**Records** (`{x: 1, y: 2}`) are slotted objects like classes, with their fields
sorted by name, so `{y: 9, x: 8}` and `{x: 8, y: 9}` share one layout. `emit_hir.rs`
plans a field's slot (`record_fields`, `records`, `record_pats`) and
`Checker::record_type_id` gives each shape (names and word kinds) a type id with
precise `ClassWordKinds`. Reads and writes are `ProductLoad` / `HeapFieldStore`:
CSE and LICM treat them as any class field. The host's records
(`io::fs::metadata`, FFI `extern struct` returns) use the same layout. Record type
ids come from their own range (`RECORD_TYPE_ID_BASE`,
`common::is_record_type_id`), so `==` compares two records slot by slot and two
class instances by identity (#1618).

**Allocation and collection.** `Alloc`, `ArrayPush`, `Format`, `Stringify` and
`Cow` are the ops that make or grow objects. A `GcBarrier` follows each as its
safepoint. `fill_live_roots` (`gc.rs`) records the live heap words at those edges
in `GcBarrier.roots` and `MirFunc.gc_roots`. The engines do not read that sidecar:
they work out their roots from `frame_words` ([native.md](native.md#calls-and-collections)).

## Passes by level

`OptimizeOptions` (filled from the [`OptLevel`](pipeline.md#optimization-levels))
reaches `optimize_mir`. Two switches gate the passes:

| Switch | On at | Passes |
|--------|-------|--------|
| `mir_fold` | `-O1` and up, `-Os` | SCCP, InstCombine, DestProp, `thread_bool_phis` |
| `mir_loop` | `-O2`, `-O3`, `-Os` | `loop_opts` (CSE, DestProp, LICM, CSE), `interchange_loops`, strength reduction, CSE, GVN/PRE, `copy_until`, `byte_scan`, `bulk_push`, `vec_loops`, `mark_prefetch_scans` |

`sroa` and `phi_sroa` run at every level. Debug builds verify the MIR after each pass
(`check_after`), so a pass that breaks SSA is named where it does.

## SCCP

`mir::sccp` (`sccp.rs`) is Wegman–Zadeck sparse conditional constant
propagation. Every value starts unknown (⊤) and every block unreached but
the entry; reached blocks evaluate their instructions over ⊤ → constant →
varying (⊥). A φ meets only the values on edges that run, and a branch on
a known condition runs one edge, so a loop φ that only gets its own value
back stays constant and a join whose other arms never run takes the one
that does: what InstCombine (which skips φs) and `tidy` (which only drops
blocks nothing jumps to) miss.

Each value proven constant then becomes a `Const`, a branch on one a
jump, φ edges from blocks that never run go, the blocks nothing reaches
are dropped and DCE runs. Only pure scalar ops fold (`Bin`, `Cmp`,
`Unary`, `Cast`, `Phi`); an op that would trap is never constant
(`instcombine::const_of` refuses it), so it stays and traps at run time.
A body that has not settled after `MAX_SWEEPS` (64) sweeps is left as it
was. Tests: `sccp.tests.rs`.

## CSE and GVN

`mir::cse` numbers values in one block, including `DIV` / `MOD` / `DIVF` / `MODF`
and repeated element, field, length and pure-call reads. `mir::gvn` walks the
dominator tree and hoists a pure expression onto a fork when **every** successor
computes it and the operands already dominate the fork. Integer `Div` / `Rem` stay
in their arms (a zero-trip or untaken path must not trap). There is no join-φ PRE
and no speculative one-arm hoist. Hit benches: `examples/perf/mir_cse_divf.coil`,
`mir_gvn_divf.coil`.

## LICM

Natural-loop LICM (`licm.rs`) hoists invariant constants, arithmetic, compares,
unary ops and casts into a preheader. A trapping op (int `Div` / `Rem` / `Pow`,
overflow-checked int ops, a checked element read) or a pure call that runs on
every iteration moves behind a copy of the loop test instead, so a loop that never
runs never traps. Lengths and element or field reads move only out of loops that
cannot resize or write that kind of heap word (`effects::may_resize` /
`writes_elements` / `writes_fields`). A second CSE run merges what moved. Hit
bench: `examples/perf/mir_licm_divf.coil`.

The heap summary has two kinds (#1559): elements (`StoreIndex`, which applies only
to arrays) and fields (`HeapFieldStore`, only instances; `ProductLoad` reads
slot `k` of a product object: an instance's field, a tuple's item at a
constant index, an enum's payload word; `Index` reads arrays and a tuple at a
runtime index). A `ProductLoad` of a tuple (a dictionary and an existential
pack among them) is in `MirFunc::invariant_loads`, as a `readonly class`
field's is: tuples are immutable. An element store
therefore keeps no field read in its loop, and a field store no element read; CSE
kills a `Field` key only on a field store and an `Index` key only on an element
store. A store still kills every read of its own kind, whatever object it names,
so aliases stay sound; calls, natives and appends that may run effects count as
both. Hit bench: `examples/perf/licm_field_past_element_store.coil`.

Loops and GVN ask dominance of `licm::Dominators` (#1518): the immediate-dominator
tree (Cooper, Harvey and Kennedy, over reverse postorder), with each query answered
from the tree's preorder spans. The roots are the entry and every block nothing
branches to; a block no root reaches is dominated by every block. The unit test
`natural_loops_about_linear_over_join_chain` counts the steps over `n` diamonds in
a row.

## InstCombine

Typed peepholes on `f64` / `i64` SSA. HIR folding (`hir::fold`) only sees literal
operands in the source tree; this pass folds results too.

- const-fold of bin / cmp / unary / cast (refuse int and float ÷0 and `MIN / -1`)
- identities: `x±0`, `x*1`, `x/1`, int `x&-1` / `|0` / `^0` / `<<0`, int `x-x`
  / `x*0` / `x%1` (float `+0.0` / `*1.0` exact bits only; refuse `x*0.0`)
- strength: `x * 2` to `x + x` (int and IEEE `+2.0`)
- const-cond `br` to `jump`

Float rewrites are IEEE-safe only. There is no fast-math, contract or reassociation
flag.

- **No FMA.** `a * b + c` stays mul-then-add (two roundings). A fused `mul_add`
  would change results.
- **Exact reciprocal:** `x / 2^k` to `x * 2^{-k}` when the divisor is a normal power
  of two and `1/c` is finite. `/ 3.0` stays.
- **Known-finite:** `x - x` and `x + (-x)` to `+0.0`, `x / x` to `+1.0` only when
  `x` is a finite const, `int→float`, `fneg` of those, or a φ of those. `x / x` also
  needs a nonzero const. Parameters and mul results stay unfolded (`inf - inf` and
  `0 / 0` are NaN).
- **`x * -1.0` to `fneg`.** Bit-identical for finite, zero and inf.

Hit benches: `examples/perf/mir_instcombine.coil`, `mir_float_pipeline.coil`.

## DestProp

Trivial φ forwarding on typed SSA. Braun already drops `phi(x, x)` at construction;
InstCombine can make both arms the same `ValueId` (`a + 0` / `a * 1` to `a`). Uses
then see the source. Disagreeing φs, type mismatches and self-only φs stay. A
following CSE can share the forwarded uses. Hit bench:
`examples/perf/mir_destprop.coil`.

## IV strength reduction

A lite LSR (`strength.rs`) rewrites `iv * invariant` to an add induction (a new
header φ, latch `+ step*factor`), for floats only: an int recurrence computes the
next product before the exit test, so it could trap on an overflow the loop never
performs ([Int overflow](int-overflow.md)). Float `cast(i) * C` only when `C` is a
finite integer-valued const (IEEE-exact while the product stays in the mantissa).
Non-const float factors and quadratic `i*i` stay. Hit bench:
`examples/perf/mir_iv_sr.coil`.

## Loop rewrites

These turn a loop into an op or a call, and run after the scalar passes.

| Pass | Rewrite |
|------|---------|
| `interchange_loops` | A nest walking memory across its rows walks along them; the inner loop's row base then hoists |
| `copy_until` | A loop that pushes each byte until one stops it: a scan for the stop and one copy of the run |
| `byte_scan` | Early-exit byte loops as `ByteScan`, skipping 64 no-op iterations at a time |
| `bulk_push` | Counted copy and fill loops as one call |
| `vec_loops` | Counted map and reduce loops as one `VecLoop`, 8 lanes at a time ([simd.md](simd.md)) |
| `mark_prefetch_scans` | Fills `prefetch_loads` for forward scans of pointer arrays. No engine reads it yet |

`vectorize.rs` matches the loops `vec_loops` takes: `a[i + k] = value(i)` stores
and `s = s ⊕ term(i)` reductions over stride-1 `i64` / `f64` lanes. A failed guard
runs the loop as written, panic and all. A float reduce left-folds lanes into the
running scalar, and a lane FMA is mul-then-add. Hit benches: `vec_scan`, `vec_axpy`,
`vec_lanes`, `vec_reduce`, `vec_byte_scan`, `vec_bulk` in `examples/perf/`.

## Two-word values

[`MirLayout::from_coil_ty`](../../compiler/src/layout/abi.rs) matches
[`two_word_kind`](../../compiler/src/layout/abi.rs) for builtins:

| Shape | Layout | Edge |
|-------|--------|------|
| `Option<int>` / any immediate inner | `TwoSlot` | `[payload, tag]` on a direct call and return |
| `Result<Ok, E>` with immediate `Ok` | `TwoSlot` | same |
| arity-2 immediate product | `TwoSlot` | `[a, b]` |
| heap `Option<T>` | `HeapNiche` | one word: `None` is `0`, `Some` a pointer |
| heap-heap `Result<T,E>` | `HeapNiche` | `Ok` is an aligned pointer, `Err` is `pointer \| 1` |
| nested / mixed / `CallIndirect` / unsure | `Word` | boxed `ObjEnum` |

Heap niches never overlap two-slot: a niche needs both sides heap, two-slot needs
an immediate `Ok` or `Option` inner.

A two-slot return is `Terminator::Return { lo, hi }` (`lo` is the payload or first
product word, `hi` the tag or second word). One-word niche, boxed and scalar
returns keep `hi = None`. The layout sits on `MirFunc::ret_layout`. A `Call`'s
second word is its `dest_hi`. A function returns at most two words.

Host `Option` / `Result<(), E>` / heap-heap `Result` values pack once at
`HostInvoke` (`host_enum`). MIR adds no second pack.

## Ops only some paths make

| Op | Who makes it |
|----|--------------|
| `Probe` | `mir::coverage`, in coverage builds |
| `ByteScan`, `VecLoop` | Only passes, so raw MIR has none |

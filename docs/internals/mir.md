# Numeric MIR (COI-267 / COI-268) and Result/Option LIR (COI-270)

Typed SSA sidecar for a **numeric subset**, **dense bytecode** for
specialized float/i32 loops (P1), MIR CSE (P2), and **MIR→LIR** for
shipped Result/Option layouts (P3).

**Language islands (I0+)** expand this sidecar beyond pure numeric — they
are not a full-MIR rewrite. Doctrine, I1–I8 ladder, refuse map, and A/B
rules: [mir-islands.md](mir-islands.md) (COI-292). IL stays lowering +
fuse-select ([pipeline.md](pipeline.md)).

## Where it lives

`compiler/src/mir/` — not inside `il/`. Fuse-IL stays the production lowerer
for non-specialized functions. Dense emit replaces a whole function body
before stack-IL opts when the body qualifies. Two-slot leaf helpers replace
the body with stack IL (`emit_lir`) after the same opts when the reconstruct
is no larger than the opted fuse-IL (single-use return/cmp values stay on
the stack).

| Piece | Role |
|-------|------|
| `MirTy` | Lattice: `bottom ⊑ {i32⊑i64, f32⊑f64, bool, heap-ref, niche Option/Result} ⊑ value`. I1 names heap/niche words; dense still uses numeric lanes only ([mir-islands.md](mir-islands.md)). |
| `MirLayout` | Call-edge ABI: `word` / `twoslot` / `heap_niche` |
| `MirBuilder` | Braun SSA (locals = IL slots, explicit φ) |
| `try_lower_numeric` | Pre-fuse `IlOp` → SSA; refuses escaping classes / leftover unmapped heap. One-word `CALL` (Q7), two-slot helper / self / sibling `CALL` / `RETURN` (B3 / B7 / C1), niche / two-slot match (Q8), and mapped `ArrayPush` (B6) lower |
| `try_specialize_body` | Infer + SSA + MIR CSE/GVN + MIR LICM + MIR InstCombine (P11 float peeps) + DestProp + IV SR + saxpy-reduce HostInvoke (P12) or dense emit (HostInvoke box/unbox; cost gate vs fuse) |
| `try_lower_abi_body` | Infer + SSA + MIR CSE + LIR emit when there is no hard refuse; `IlModule` keeps it only if cost ≤ fuse |
| `mir::cse` | Same-block GVN (includes `DIVF`/`DIV` that stack-IL CSE refuses); used on dense and LIR leafs |
| `mir::gvn` | Dominator GVN + fully-anticipated fork PRE; dense specialize only (not ABI LIR) |
| `mir::licm` | Natural-loop hoist of invariant Const/arith/cmp/cast (float `Div` ok; int `Div`/`Rem` stay) |
| text form | Print / parse for round-trip tests |

Language `int` / `float` / `bool` map to `i64` / `f64` / `bool`. I1
([COI-293](https://linear.app/ardax/issue/COI-293/i1-heap-niche-types-in-mir-lattice))
adds `heapref` / `niche_opt` / `niche_res` under `value` so infer/lower can
**carry** shipped heap and niche Option/Result words in SSA. Dense emit and
`DenseAbi` take numeric, `HeapRef`, and niche word lanes; two-slot `RETURN`
and allocating / escaping bodies stay fuse-IL or LIR. I2 ([COI-294](https://linear.app/ardax/issue/COI-294/i2-match-on-niche-two-slot-in-mir)
/ [COI-302](https://linear.app/ardax/issue/COI-302/after-unlock-i2-boxedconstructmatch-cost-gate))
lowers niche / two-slot / boxed-overlap match (`LogNot` / tag `Br` /
`JumpIfMatch` any tag, arity ≤ 1 including arity 0 overlap). **Q8**
([COI-330](https://linear.app/ardax/issue/COI-330)) reconstructs niche /
two-slot match as dense register `Br` (cost gate vs LIR/fuse). Boxed
`JumpIfMatch` stays MIR→LIR.
I3 ([COI-295](https://linear.app/ardax/issue/COI-295/i3-non-escaping-class-fields-in-mir))
lowers field load/store on **non-escaping** named class locals the
local_escape sidecar already unboxed into consecutive slots
(`FieldLoad` / `FieldStore`). Escaping / heap-backed named locals
(`InitTyped` / `GetField` / `LoadField`) stay fuse-IL. Dense emit
refuses the new field ops.
I4 / Q9 ([COI-296](https://linear.app/ardax/issue/COI-296/i4-string-format-mir-subset-or-refuse),
[COI-332](https://linear.app/ardax/issue/COI-332))
names `STRING` / `PRINT` / `FORMAT` / `STRINGIFY` as SSA (R1) and
reconstructs them on MIR→LIR. Dense infer still refuses table ops so
numeric specialize is unchanged. **R2** densifies `from_bytes` /
`to_bytes` as I6 HostInvoke (box at the host edge). **R3** maps
`FORMAT` / `STRINGIFY` like I5 alloc. There is no second
Format lowering. Unicode / regex are out of MIR (R4 leftover). Ladder:
[q9-format-string.md](q9-format-string.md).
I5 ([COI-300](https://linear.app/ardax/issue/COI-300/i5-alloc-gc-barriers-in-mir))
names `Alloc` (`MakeArray` / `MakeTuple` / `MakeEnum` / `InitTyped`) and
`GcBarrier` safepoints. S2a
([COI-305](https://linear.app/ardax/issue/COI-305/s2a-live-root-sidecar-at-mir-gcbarrier-alloc))
fills live-heap `roots` (and IL slots when snapshotted). S2b
([COI-306](https://linear.app/ardax/issue/COI-306/s2b-slot-frame-stack-maps-for-interpreter-gc))
encodes those slots as interpreter frame maps. S2c
([COI-307](https://linear.app/ardax/issue/COI-307/s2c-specialize-lir-across-alloc-when-maps-exist))
lets infer / specialize / `emit_lir` cross alloc **only** when those
maps exist. S2d
([COI-314](https://linear.app/ardax/issue/COI-314/s2d-map-backed-looping-alloc-further-alloc-opts))
extends that to mapped **preheader** `Make*` + index (dense) and
compare-only looping leftovers (LIR). S2e
([COI-316](https://linear.app/ardax/issue/COI-316)) drops per-residual
`Seek` restore; in-loop `Make*` stays off dense (boxing tax).
Post-loop-only heap returns stay fuse-IL (invert+fuse). Unmapped allocating bodies stay fuse-IL. S3
([COI-308](https://linear.app/ardax/issue/COI-308/s3-widen-densemir-coverage-match-call-heap-index))
widens dense coverage: one-word `CALL` beyond the dense map, and I6
HostInvoke including Q9 R2 `from_bytes` / `to_bytes`. Heap-index / `StoreIndex` take dense after V* miss (S3b): unpinned
`Index` / `StoreIndex` / `ArrayLen`. Residual `StorePop` leaves tell at
the prologue frame high-water; S2e does not emit a per-residual `Seek`.
Pin opcodes stay fuse-IL / proven stack-IL only — pin keys do not
survive the dense prologue `Seek`.
Boxed dense+match stays I2 LIR (JumpIfMatch stack protocol vs dense regs).
Niche / two-slot match may dense (**Q8**).
Stack-map note: [mir-stack-maps.md](mir-stack-maps.md).

## P1 — dense exec (COI-268)

Eligible numeric loops (float `+`/`-`/`*`/`/`, counted i64 `+`/`-`/`*`/`/`/`%`,
or `i32`; one or more back-edges) and cost-gated straight-line bodies emit:

- `DenseBin` / `DenseConst` / `DenseMove` / `DenseUnary` / `DenseCast`
- `Seek` to the typed slot high-water mark
- Fuse-select `LOAD`/`LOAD`/`cmp`/`JMPF` (→ `BinSlotSlotJmpf`) and `RETURN`
  at control and **Value ABI** edges

Nested / multi-header float-mul loops (flagship `mandelbrot.hy`) are in
scope for dense. The hit bench `examples/perf/mir_dense_float.hy`
(`escape`) remains the single-header kernel.

CALL still places args as `Value` words in slots `0..arity`. Dense ops
reinterpret those bits as `i64`/`f64`. RETURN loads one word back onto the
stack. Dense emit **refuses** two-word returns.

Float `+`/`-`/`/` loops specialize (COI-287 W1; hit `mir_dense_addf.hy` is
`+`/`-` only — `DIVF` already qualified). Counted i64 loops specialize
(COI-288 W2; hit `mir_dense_i64.hy`). Refuse inventory:
[specialize-refuse.md](specialize-refuse.md).

## P2 — MIR CSE (COI-269)

After SSA lower, **GVN** runs on the numeric function before dense emit
(and before P3 LIR emit). P2 numbered same-block; P10 walks the dominator
tree and adds fully-anticipated fork PRE. Stack-IL `local_cse` / `ssa_gvn`
still refuse `DIV`/`MOD`/`DIVF`/`MODF`; those ops are numbered here.
Fuse-IL InstCombine / CSE / LICM are unchanged for non-MIR bodies. Hit
benches: `examples/perf/mir_cse_divf.hy`, `mir_gvn_divf.hy`.

## P6 — MIR LICM + widen specialize (COI-280)

After CSE, **natural-loop LICM** hoists invariant Const / add/sub/mul /
float div / cmp / unary / cast into a preheader. Integer `Div`/`Rem` stay
in the loop so a skipped trip cannot trap. A second CSE run merges
hoisted consts. Hit bench: `examples/perf/mir_licm_divf.hy`.

Specialize no longer refuses multi-header bodies: nested numeric loops
(including flagship `mandelbrot`) can emit `DenseBin` when infer + lower
succeed. Straight-line bodies lift and keep only when emit cost ≤ fuse-IL
([specialize-refuse.md](specialize-refuse.md)).

## W1 — float arith without requiring `*` (COI-287)

Infer’s dense gate is `has_float_arith` (`ADDF`/`SUBF`/`MULF`/`DIVF`, plus
float `INC`/`DEC`), not `MULF`/`DIVF` only. Add-only and sub-only float
loops emit `DenseBin`. `DIVF` already qualified before W1. Hit bench:
`examples/perf/mir_dense_addf.hy`.

## W2 — counted i64 (COI-288)

Infer’s dense gate also accepts i64 `+`/`-`/`*`/`/`/`%` (and int `INC`/`DEC`)
on a back-edge body that is otherwise numeric (multi-word `RETURN` still
refuses). Heap index and one-word `CALL` may lift (A2 / Q7). Compare-only
stays fuse-IL. Hit bench:
`examples/perf/mir_dense_i64.hy`.

## W3 — cost-gated straight-line (COI-289 / A3)

A no-back-edge numeric body may lift. Keep only when reconstruct cost ≤
opted fuse-IL (`Seek` weighted by frame size). Dense `Seek` + Value ABI is
a per-CALL tax that loops amortize; tiny helpers stay fuse-IL. There is
no `work_ops ≥ 8` infer refuse.
Hit bench: `examples/perf/mir_dense_straight.hy`.

## W4 — HostInvoke inside dense (COI-290 / A3)

Specialize no longer refuses a numeric body solely because it contains
HostInvoke. LICM hoist is **purity bits** (scalar-pure math / axpy; not
`packed_*` heap reads; not IO / clocks / GC). S3 dense emit reconstructs
I6-typed hosts including Q9 R2 `from_bytes` / `to_bytes`. Precise float/heap
types still live on math / packed / axpy specs.

I6
([COI-297](https://linear.app/ardax/issue/COI-297/i6-effects-hostinvoke-as-mir-edges))
types natives as SSA `HostInvoke` edges when `allow_effects` is set. I7
([COI-299](https://linear.app/ardax/issue/COI-299/i7-debugger-deopt-boundaries-on-mir))
names `Deopt` stop / leave edges (`allow_deopt`). **B8:** debugger-attached
and `-Og` may dense + MIR→LIR; emit skips explicit `Deopt`. **C3:**
compiler-internal resume maps, named-let remap, sparse emit locs
(no archive bump; P5 resume leftover).
The VM debugger steps the reconstruct
([mir-deopt.md](mir-deopt.md)). I8
([COI-298](https://linear.app/ardax/issue/COI-298/i8-broaden-mir-emit-entry-post-i1-i3))
lifts leftover bodies through MIR→LIR when there is no hard refuse.
Unmapped I5 alloc and HostInvoke/`CALL` stay fuse-IL on the LIR path.
Q9 R1 string/format may lift. User `CALL` is COI-291 (below). Dense emit keeps
`DenseBin` for the numeric region and at each host edge: `LOAD` args
(Value words) → `CONST` id → `HostInvoke` → `STORE` dest, then more
dense ops. P12 whole-body saxpy pack still runs first when the pattern
matches (no inner host). Hit bench: `examples/perf/mir_dense_host.hy`.

## M1–M3 — typed dense→dense CALL (COI-291)

One-word ABI ([`compiler/src/mir/abi.rs`](../../compiler/src/mir/abi.rs)):

- **Args:** `arity` Value words in callee slots `0..arity` (same bits as
  typed dense slots — `i32`/`i64`/`f32`/`f64`/`bool`).
- **Return:** one word on TOS (`RETURN` width 1). Caller `STORE`s it.
- **Two-slot / niche:** helper two-slot is **B3**; sibling / mutual
  `TailCall` is **B7**; self two-slot `CALL` / `RETURN` is **C1**.
  `CallIndirect` refuses. SSA models dest + `dest_hi`
  (`MAX_MODELED_RET_WORDS` = 2). N>2 needs extra dests + archive encoding.
  **Q7:** one-word self-`CALL` / `TailCall` may dense after convoy fused
  returns unfuse. **B7:** sibling / mutual `TailCall` uses stack args (not
  B2 dest convoy). **C1** may convoy a self-`CALL` pair that only feeds
  `RETURN`. Keep only when emit cost ≤ fuse-IL. **B2** emits self
  CALL args/results on the operand stack and skips prologue `Seek` when
  only param slots are live, so tight `tak` / `fib` can win that gate.

`IlModule` specializes **bottom-up**: a body may `CALL` only after the
callee is already in the dense ABI map. Infer/lower treat that `CALL`
like W4 HostInvoke (typed SSA args). Emit is `LOAD` args → `Entry` →
`STORE` dest. The VM `CALL`/`RETURN` path already restores the caller
frame base and keeps caller slots below `callee_sp`; no new opcode.

Non-dense callees still refuse (W4 HostInvoke allowlist unchanged).
Hit bench: `examples/perf/mir_dense_call.hy`.

## P7 — MIR InstCombine (COI-281)

After LICM + a second CSE, **typed peeps** run on dense SSA (`f64` / `i32` /
`i64`). Fuse-IL `algebraic` only matches Load/Const/ConstPool windows; this
pass folds binop results too.

Proving set (P11 float peeps live in the same pass):

- const-fold of bin / cmp / unary / cast (refuse int/float ÷0 and `MIN / -1`)
- identities: `x±0`, `x*1`, `x/1`, int `x&-1` / `|0` / `^0` / `<<0`, int `x-x`
  / `x*0` / `x%1` (float `+0.0` / `*1.0` exact bits only; refuse `x*0.0`)
- strength: `x * 2` → `x + x` (int and IEEE `+2.0`; flagship `2.0 * zr` hits)
- const-cond `br` → `jump`
- P11 float peeps (same pass): see below

Hit bench: `examples/perf/mir_instcombine.hy`.

## P8 — MIR DestProp / copy-forward (COI-282)

After InstCombine, **trivial φ forwarding** runs on typed SSA. Braun already
drops `phi(x, x)` at construction; InstCombine can make both arms the same
`ValueId` (`a + 0` / `a * 1` → `a`). Uses see the source. Disagreeing φs,
type mismatch, and self-only φs stay. No dead-block rewrite (dense emit
fallthrough) and no new opcodes. A following CSE can share the forwarded
uses. Hit bench: `examples/perf/mir_destprop.hy`.

## P9 — MIR IV strength reduction (COI-283)

After DestProp, a **lite LSR** rewrites `iv * invariant` to an add
induction (new header φ, latch `+ step*factor`). Integer `i32`/`i64` is
wrapping-exact. Float `cast(i) * C` only when `C` is a finite
integer-valued const (IEEE-exact while the product stays in the
mantissa). Non-const float factors stay, so mandelbrot
`(x as float) * (2/size)` is unchanged. Quadratic `i*i` and IL-style
host barriers do not apply (dense numeric subset only). A following CSE
cleans unused casts. Hit bench: `examples/perf/mir_iv_sr.hy`.

## P10 — MIR cross-block GVN / PRE (COI-284)

`mir::cse` numbers expressions along the dominator tree (not only
same-block) and hoists a pure expr onto a fork when **every** successor
computes it and the operands already dominate the fork. Integer
`Div`/`Rem` stay in their arms (zero-trip / untaken-path trap). No
join-φ PRE, no speculative one-arm hoist, no new opcodes. Hit bench:
`examples/perf/mir_gvn_divf.hy`.

## P11 — MIR float pipeline (COI-285)

InstCombine’s typed peeps add **IEEE-safe** float rewrites only. There is
**no** fast-math / contract / reassoc flag — default is the only policy.

- **No FMA.** `a * b + c` stays mul-then-add (two roundings). A fused
  `mul_add` would change flagship `mandelbrot` checksums and needs a
  Dense FMA kind that does not exist. Do not append one for this ticket.
- **Exact reciprocal:** `x / 2^k` → `x * 2^{-k}` when the divisor is a
  normal power of two (mantissa 0) and `1/c` is finite. `/ 3.0` stays.
- **Known-finite:** `x - x` and `x + (-x)` → `+0.0`, `x / x` → `+1.0`
  only when `x` is a finite const, `int→float`, `fneg` of those, or a φ
  of those. `x / x` also needs a nonzero const. Params / mul results stay
  unfolded (`inf - inf` / `0 / 0` are NaN).
- **`x * -1.0` → `fneg`.** Bit-identical for finite / zero / inf.
- **No sqrt / rsqrt opcode.** Numeric MIR has no sqrt op. W4 may call
  allowlisted `math_sqrt` (HostInvoke **105**) at a box/unbox edge.

FMA / recip do **not** fire on mandelbrot (`2.0 * zr * zi + ci` stays
two ops after `*2` → `+`; `2/size` is not a power-of-two after the
outer `2.0 *`). Hit bench: `examples/perf/mir_float_pipeline.hy`.

## P12 — MIR → coil-simd HostInvoke (COI-286)

After GVN/PRE, a **single counted saxpy-reduce** may replace the whole
body with HostInvoke `simd_axpy_reduce` (**136**) instead of dense
bytecode. Pattern (no new Coil syntax):

`s = 0; x = x0; i = 0; while i < n { s = s + a * x + y; x = x + dx; i = i + 1 }`

or the affine form `x = (i as float) * dx + x0`. The kernel lives in the
workspace `coil-simd` crate (AVX2/SSE2/AVX-512/NEON + scalar). Terms use
mul-then-add, no FMA; the sum left-folds `(s + a*x) + y`. Nested /
data-dependent loops (flagship mandelbrot, `mir_dense_float` escape)
stay on `DenseBin`. Const trip count `< 8` refuses. Hit bench:
`examples/perf/mir_simd_axpy.hy`.

`ardax-corp/coil-simd` is not a separate GitHub package; kernels stay
in-tree (same crate already used by `packed_la`).

## S5a V0 — compiler-only SIMD opcodes (COI-310)

After P12, a **single counted stride-1 numeric store** may emit `VLoad` /
`VStore` / `VBin` / `VMove` (archive **minor 8**) instead of fuse-IL
heap-index. Width is eight `i64`/`f64` lanes; handlers call
`coil-simd::lanes`. No userland SIMD syntax.

Hard refuse (same spirit as dense): alloc/GC in the region, `match`,
impure `CALL` / HostInvoke, explicit `Deopt`, heap words in vregs,
gather/scatter, loop-carried reductions (V1). Dense+Index mix stays
fuse-IL. P12 HostInvoke packs are unchanged.

Prove: `fill` in `examples/perf/vec_scan.hy` (`v[i] = i`). Flagships
stay dense / fuse-IL.

## S5b V1 — SIMD reductions + conservative FMA (COI-311)

After V0, a **single counted stride-1 add-reduce**
(`s = s + a[i]` / `s = s + a[i] * b[i]`) may emit `VLoad` / `VBin` /
`VReduce`. A store of `a * x[i] + y[i]` (or zip `a[i] * b[i] + c[i]`)
may emit `VFma`. Archive **minor 9**.

P11 still holds: float `VReduce` left-folds lanes into the running
scalar (`s = (…((s+x0)+x1)…)`); `VFma` is mul-then-add (two
roundings). No fast-math / contract flag. Hardware `fmadd` is not used.

Same refuse map as V0 (alloc/GC, match, impure CALL/HostInvoke,
explicit `Deopt`, heap words in vregs). Reductions without a heap load
stay dense (`s = s + i`). P12 HostInvoke packs stay first.

Prove: `scan` in `examples/perf/vec_scan.hy`; saxpy store in
`examples/perf/vec_axpy.hy`.

## P3 — multi-word / niche as MIR→LIR (COI-270)

LIR here is the existing **fuse-IL / stack IL**, not a new ISA. Review Board
kept host-edge pack/unpack; this cut does **not** revive pair/niche opcodes
or a nursery.

### Layouts (align with shipped codegen)

[`MirLayout::from_coil_ty`](../../compiler/src/mir/layout.rs) matches
[`return_layout`](../../compiler/src/typechecking/return_layout.rs) for
builtins (user arity-≤1 enums stay classified only in codegen):

| Shape | Layout | Edge |
|-------|--------|------|
| `Option<int>` / any immediate inner | `TwoSlot` | `[payload, tag]` on direct `CALL`/`RETURN` (`CALL` bit 31, `RETURN` operand `2`) |
| `Result<Ok, E>` with immediate `Ok` | `TwoSlot` | same |
| arity-2 immediate product | `TwoSlot` | `[a, b]` (second on top) |
| heap `Option<T>` | `HeapNiche` | one `Value`: `None` = `0`, `Some` = pointer |
| heap-heap `Result<T,E>` | `HeapNiche` | `Ok` = aligned pointer, `Err` = `pointer \| 1` |
| nested / mixed / `CallIndirect` / unsure | `Word` | boxed `ObjEnum` |

Heap niches never overlap two-slot (niche needs both sides heap; two-slot
needs immediate `Ok` / immediate Option inner).

### SSA

A two-slot return is `Terminator::Return { lo, hi }` (`lo` = payload or first
product word, `hi` = tag / second word). One-word niche / boxed / scalar
returns keep `hi = None`. Numeric ops stay `MirTy::{I64,F64,Bool}`; the
layout sits on `MirFunc::ret_layout`.

`try_lower_numeric` accepts `ret_words` in `1..=MAX_MODELED_RET_WORDS`
(payload then tag on the IL stack when width is 2). Dense infer accepts
that shape (B3 / C1); keep/refuse is the cost gate. N>2 refuses.

### LIR emit

`emit_lir` writes stack IL only:

- `Seek` + slot `LOAD`/`STORE` / `BinSlotSlot` / fused cmp+`JMPF`
- `RETURN` width 2 for `TwoSlot`, width 1 for `Word` / `HeapNiche`
- niche bits as existing `BITAND` / `BITOR` / `CONST 0` / `LogNot`
- no `MakeEnum`, no `ReturnPair` / `PairToHeap` / niche ISA tombstones

`try_lower_abi_body` + `emit_lir` implement that mapping (eligible IL →
SSA → fuse-IL). I8 widens eligibility to any leftover body without an
I4–I7 refuse (`compiler/src/mir/entry.rs`). Production replace is **ON**
after stack-IL opts: `emit_lir` keeps single-use return/cmp values on the
stack and `DUP`s a TOS that is also the first word of `k, k+1`. Int
`slot ⊕ imm` bins emit `BinSlotImm` so pre-fuse cost matches opted
fuse-IL. A body is kept only when emitting cost does not grow.
Stack-IL opts are **not** re-run on the reconstruct (`local_cse`
refuses `MOD` and rematerialized `pair_int_churn`). Callers
(`match f()`, `?`, I/O) stay on fuse-IL.
Hit bench: `examples/perf/result_int_churn.hy`.

Host Option / `Result<(),E>` / heap-heap Result still pack once at
`HostInvoke` (`host_enum`). MIR does not add a second pack.

## Out of scope (later tickets)

- P5 — optional Cranelift

## Acceptance

`mir::mandelbrot_inner_loop` is typed SSA. `tests/positive/mir_dense_float.hy`
and `examples/perf/mir_dense_float.hy` (`escape`) execute via `DenseBin`.
Flagship `mandelbrot.hy` (three nested loops) is eligible for dense.
`Result<int,int>` / `Option<int>` leafs lower through MIR→LIR with the
shipped two-slot ABI (`tests/positive/mir_result_int.hy`).

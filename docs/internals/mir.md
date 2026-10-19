# MIR

Every function body lowers from HIR to typed SSA (MIR) and is emitted from
it in one of two tiers: **dense** 3-address bytecode, or **LIR**, the VM's
stack ops written from the SSA. Since M4 and M5 (2026-10) there is no
stack-IL lift, no fuse-IL tier and no stack-lowering fallback; a body MIR
cannot take is an internal error. The P*, W*, I* and S* sections below are
the history of how MIR coverage grew to every body; their notes about
"fuse-IL" describe the stack-lowered bodies that no longer exist.

## Where it lives

`compiler/src/mir/`. Per body, in order:

1. `codegen/emit_hir.rs` plans the body (layouts, dictionaries, call
   targets) and `mir::lower_from_hir` (`from_hir.rs`) lowers HIR plus that
   plan to SSA, with GC roots and frame maps.
2. `IlModule::optimize_and_flatten` (`il/module.rs`) tries **dense** leaf
   first: MIR passes, then dense emit, kept when its cost beats the body's
   LIR form and every callee it `CALL`s is dense.
3. Otherwise the body takes **LIR**: SROA (allocating bodies) or CSE, then
   InstCombine's folds (`instcombine_lir`), then `emit_lir`. A `defer` body takes LIR with its cleanup pad.
4. All bodies are concatenated and lowered once by `il::lower`
   (fuse-select, PCs, encode).

| Piece | Role |
|-------|------|
| `MirTy` | Lattice: `bottom ⊑ {i64, f64, bool, heap-ref, niche Option/Result} ⊑ value` |
| `MirLayout` | Call-edge ABI: `word` / `twoslot` / `heap_niche` |
| `MirBuilder` | Braun SSA (locals = frame slots, explicit φ) |
| `lower_from_hir` | HIR body → SSA, for every body |
| `try_specialize_body_side` | MIR CSE/GVN + LICM + InstCombine (P11 float peeps) + DestProp + IV SR + saxpy-reduce HostInvoke (P12) or dense emit (HostInvoke box/unbox; cost gate vs the body's LIR form) |
| `try_lower_abi_body_side` | SROA or CSE + LIR emit (`emit_lir`) |
| `mir::cse` | Same-block GVN (includes `DIVF`/`DIV`); used on dense and LIR |
| `mir::gvn` | Dominator GVN + fully-anticipated fork PRE; dense only |
| `mir::licm` | Natural-loop hoist of invariant arith/cmp/cast/length/field reads; trapping ops (int `Div`/`Rem`/`Pow`, checked `Index`) and pure calls move behind a copy of the loop test; dense and LIR bodies (LIR keeps literals in the loop) |
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
lowers field load/store on **non-escaping** named class locals that
lowering unboxed into consecutive slots
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
encoded those slots as interpreter frame maps (no longer emitted: precise
frame maps of the final bytecode cover every body,
[mir-stack-maps.md](mir-stack-maps.md)). S2c
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
Only the prologue `ArrayPin` is still emitted; `IndexPin*` / `StoreIndexPin*` are not ([array-pin.md](array-pin.md)).
Boxed dense+match stays I2 LIR (JumpIfMatch stack protocol vs dense regs).
Niche / two-slot match may dense (**Q8**).
Stack-map note: [mir-stack-maps.md](mir-stack-maps.md).

## P1 — dense exec (COI-268)

Eligible numeric loops (float `+`/`-`/`*`/`/`, counted i64 `+`/`-`/`*`/`/`/`%`;
one or more back-edges) and cost-gated straight-line bodies emit:

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
tree and adds fully-anticipated fork PRE. It is the only CSE (`hir::cse` was folded in
2026-10): it numbers values lowering introduces, `DIV`/`MOD`/`DIVF`/`MODF`
across blocks, and in one block repeated element / field / length /
pure-call reads. LIR bodies run it as `cse_lir`, which keeps literals in
place, plus MIR LICM (the stack-IL passes, InstCombine included, were
removed 2026-10). Hit
benches: `examples/perf/mir_cse_divf.hy`, `mir_gvn_divf.hy`.

## P6 — MIR LICM + widen specialize (COI-280)

After CSE, **natural-loop LICM** hoists invariant Const / add/sub/mul /
float div / cmp / unary / cast into a preheader. A trapping op (int
`Div`/`Rem`/`Pow`, overflow-checked int ops, a checked element read) or a
pure call that runs on every iteration moves behind a copy of the loop
test instead, so a loop that never runs never traps. Lengths and
element or field reads move only out of loops that cannot resize or
write the heap (`effects::may_resize` / `writes_heap`). A second CSE run
merges hoisted consts. Hit bench: `examples/perf/mir_licm_divf.hy`.

Loops and GVN/PRE ask dominance of `licm::Dominators` (#1518): the
immediate-dominator tree (Cooper, Harvey and Kennedy, over reverse
postorder), with each query answered from the tree's preorder spans.
The roots are the entry and every block nothing branches to; a block no
root reaches is dominated by every block, as the set fixpoint it
replaced had it. That fixpoint kept every block's full dominator set, so
a chain of `n` joins held ~n²/2 elements, and `loop_opts` built it for
every body, loop or not. The unit test
`natural_loops_about_linear_over_join_chain` counts the steps over `n`
diamonds in a row:

| n | steps before (set elements) | steps after (preds + intersect) |
|---|---|---|
| 100 | 30,500 | 802 |
| 800 | 1,924,000 | 6,402 |

`coil compile`, release, 2026-10-19, `f(int x)` of N
`if x == k { s = s + "a"; }` (main, without #1502's LIR emission fix):

| N | Ir before | Ir after | wall before | wall after |
|---|---|---|---|---|
| 800 | 4,671 M | 3,946 M | 0.76 s | 0.74 s |
| 1600 | 15,409 M | 12,431 M | 2.66 s | 2.36 s |

The 725 M and 2,979 M saved match what callgrind put in
`licm::dominators` (#1518's table); emitted archives are byte-identical.

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
  typed dense slots — `i64`/`f64`/`bool`).
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

After LICM + a second CSE, **typed peeps** run on dense SSA (`f64` /
`i64`). HIR folding (`hir::fold`) only sees literal operands in the source
tree; this pass folds binop results too.

Proving set (P11 float peeps live in the same pass):

- const-fold of bin / cmp / unary / cast (refuse int/float ÷0 and `MIN / -1`)
- identities: `x±0`, `x*1`, `x/1`, int `x&-1` / `|0` / `^0` / `<<0`, int `x-x`
  / `x*0` / `x%1` (float `+0.0` / `*1.0` exact bits only; refuse `x*0.0`)
- strength: `x * 2` → `x + x` (int and IEEE `+2.0`; flagship `2.0 * zr` hits)
- const-cond `br` → `jump`
- P11 float peeps (same pass): see below

Hit bench: `examples/perf/mir_instcombine.hy`.

**LIR bodies (#1434).** The dense attempt is gated on `has_numeric_arith`
(`+ - * / %` only), so a body whose only arithmetic is bitwise ops or
shifts never reached InstCombine: constants that meet only in MIR (after
inlining, copy forwarding) were computed at run time. `lir_tail` now runs
`instcombine_lir` on every LIR body, at every level like `hir::fold`: the
constant folds, identities and constant branches, without the strength
rewrites (`x * 2` is one op with an immediate on the stack machine; `x + x`
reads `x` twice). When it changes the body, `from_hir::tidy` drops the
blocks a folded branch leaves unreachable (emit_lir's liveness is slow
over them: 800 dead blocks took 2.8 s). Folding puts literal operands on
trapping ops, so emit_lir keeps a may-trap op in order against memory
writes (#856; `tests/codegen/overflow_trap_order_lir.hy`). A coverage
compile (`Pipeline::set_keep_fns_in`) skips the LIR fold
(`MirFunc::keep_lines`): coverage counts a line by the code it emits, and a
line whose value folds into a later one would read as uncoverable, so
`coil mutate` would skip its mutants. For the same reason a coverage
compile refuses dense (`dense: coverage`, #1494): its MIR passes fold a
line's constants into the next line just the same, so every body takes
LIR there. Coverage runs pay for that in speed, not in what they count.

Hit bench `examples/perf/mir_lir_fold.hy` (`mix`: bit ops of `mask(-1)`
called 2M times from a dense loop; release, `COIL_AUTO_PAR=0`, 15
interleaved runs, 2026-10-19; auto-par on gives the same, the loop stays
sequential):

| | median | min | `.hyc` bytes |
|---|---|---|---|
| before (main 8ee3aae4) | 1.884 s | 1.699 s | 7084 |
| after | 1.124 s | 0.944 s | 5804 |

`check` in the issue's repro (`mask(-1) >> 16`) goes from
`CONST; CONST; BITAND; CONST; BinReturn SHR` to one `ConstReturnImm 65535`.
Guard: `lir_body_folds_constants_after_inlining` (perf_metrics).

## P8 — MIR DestProp / copy-forward (COI-282)

After InstCombine, **trivial φ forwarding** runs on typed SSA. Braun already
drops `phi(x, x)` at construction; InstCombine can make both arms the same
`ValueId` (`a + 0` / `a * 1` → `a`). Uses see the source. Disagreeing φs,
type mismatch, and self-only φs stay. No dead-block rewrite (dense emit
fallthrough) and no new opcodes. A following CSE can share the forwarded
uses. Hit bench: `examples/perf/mir_destprop.hy`.

Disagreeing latch copies are not DestProp. COI-383 **S6** sinks the
incoming def past the last use of the φ dest so dense emit’s latch
overwrite can drop the copy. No new opcode; LOAD/STORE cost 2 vs
`DenseMove` 1 is unchanged.

## P9 — MIR IV strength reduction (COI-283)

After DestProp, a **lite LSR** rewrites `iv * invariant` to an add
induction (new header φ, latch `+ step*factor`), for floats only: an int
recurrence computes the next product before the exit test, so it could trap
on an overflow the loop never performs ([Int overflow](int-overflow.md)).
Float `cast(i) * C` only when `C` is a finite
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

Refuse is the loop region, not the whole function: a call or
allocation before or after the loop is left scalar. The index may
start at a loop-invariant value other than 0, and loads/stores may be
`a[i + k]` for a constant `k`. An add-reduction does not need a heap
load (`s = s + i`). Integer division of two lanes is `VBin` `IDIV64`.
A multiply reduction (`s = s * a[i]`) uses the same `VReduce` with
the low byte set to mul; add stays `0`, so older bytecode is unchanged.
Min and max are not MIR operators, so they have no fold.

Several counted loops in one function are vectorized when each exits
straight into the next, or into `return`. A branch between the loops
is still refused: the lowering has no φ. A step other than `+ 1`, a
call inside a body, `match`, and `Deopt` are still refused.
P12 HostInvoke packs stay first.

Prove: `scan` in `examples/perf/vec_scan.hy`; saxpy store in
`examples/perf/vec_axpy.hy`.

## P3 — multi-word / niche as MIR→LIR (COI-270)

LIR here is the VM's stack ops, not a new ISA. Review Board
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

A function returns up to two words (`MAX_MODELED_RET_WORDS`). Dense
accepts that shape (B3 / C1); keep/refuse is the cost gate. N>2 refuses.

### LIR emit

`emit_lir` writes stack ops only:

- `Seek` + slot `LOAD`/`STORE` / `BinSlotSlot` / fused cmp+`JMPF`
- `RETURN` width 2 for `TwoSlot`, width 1 for `Word` / `HeapNiche`
- niche bits as existing `BITAND` / `BITOR` / `CONST 0` / `LogNot`
- no `MakeEnum`, no `ReturnPair` / `PairToHeap` / niche ISA tombstones

`try_lower_abi_body_side` + `emit_lir` implement that mapping for every
body dense does not keep. `emit_lir` keeps single-use return/cmp values on
the stack and `DUP`s a TOS that is also the first word of `k, k+1`. Int
`slot ⊕ imm` bins emit `BinSlotImm`. LIR runs MIR CSE (`cse_lir`) and
LICM, then SROA when the body allocates.
Hit bench: `examples/perf/result_int_churn.hy`.

Emit is linear in a body's size (#1393): each block's fused compare
(`term_cmp_dests`) and preds are computed once per function, and the GC
root pass computes the defined-SSA set once, not per allocation. Release
build, 2026-10-16, one `main` of N `if x == k { s = s + "a"; }`:

| N | before | after |
|---|---|---|
| 200 | 202 ms | 74 ms |
| 1600 | 10.8 s | 0.62 s |

N `assert!(x == 1);` (macro expansion included): 400 went 1.14 s → 0.39 s,
1600 went 15.2 s → 2.0 s. What remains superlinear there is macro
expansion (each generated snippet is parsed behind padding as long as all
earlier ones, and each call is looked up by a walk of the whole AST).

Blocks nothing reaches (#1493) cost emission no more than live ones. The GC
clears (`entry_clears`, `source_clears`) ask whether a collection can run
before a register is rewritten (`RewriteScan`); each question used to walk
every block reachable from its join, once per copy source of the join's φ.
A chain of joins whose rebinding arms are dead (a folded `if x == k` with
nothing dropping the arms, as #1434's LIR folding left before `tidy`) has
no collection on the live path, so every walk ran to the end: ~n³ block
scans. Now the answer is one backward pass over the CFG per register asked,
and each register is asked once per join however long its chain of copy
sources. Register sharing (`assign_shared`, which the issue first blamed)
was never the cost: 3 ms at N = 800. The unit test
`lir_clears_work_about_linear_in_unreachable_blocks` counts scans over
`N` dead arms:

| N | scans before | scans after |
|---|---|---|
| 100 | 696,498 | 89,496 |
| 800 | 43,548,198 | 4,803 |

(At 100 every value still has its own register, so more registers are
asked; at 800 they share four.) Release build, 2026-10-19, `main` of N
`if x == k { s = s + "a"; }` with #1434's folding and no `tidy`:

| N | before | after | with `tidy` |
|---|---|---|---|
| 400 | 0.63 s | 0.22 s | |
| 800 | 3.87 s | 0.70–0.83 s | 0.33 s |

`entry_clears` went 3.48 s → 0.13 s at 800. What remained superlinear
walked copy sources (`CopySources::of`) back through the whole φ chain, once
per join in `entry_clears` and once per `ClearWord` in `source_clears` and
`keep_memory_order` (~60 ms each at 800): ~n² values, reachable arms or not.

Those walks are linear now (#1502). A clear only cares which registers a
walk meets first, and in what order, so `SourceRegs` answers that once per
value in definition order (a value's sources' registers, then each
source's own answer, last source first, as the walk's stack pops them)
wherever the sources form no cycle. A walk still runs in a loop, past a
heap word fused into a branch, or when a live value defined earlier may
stop it, and then it stops following a source once every register past
it has been asked. `keep_memory_order` walks a clear's sources only when
an op from before the clear is emitted after it. `EmitPlan::new` runs once
per body (the sidecars and the cleanup pad's frame read emission's own
plan, `LirFrame`), and a barrier's paired allocation is found from one
pass over the body, not by a scan of the body per barrier. The emitted
code is unchanged: 1,532 corpus archives (`-O0` and the default level)
compile byte-identical. The unit test
`lir_copy_sources_about_linear_over_rebinding_joins` counts the values
the walks step through over `N` reachable arms:

| N | steps before | steps after |
|---|---|---|
| 100 | 29,900 | 2 |
| 800 | 1,919,200 | 2 |

(Pruning the walk alone took that to 19,801 and 6,389: the first `s`'s
register is held nowhere else, so every walk still ran to the chain's
start. `SourceRegs` itself is one pass in definition order, each value's
list at most one entry per register.)

LIR emission alone (`emit_lir` plus its sidecars, release, the same MIR
chain):

| N | before | after |
|---|---|---|
| 400 | 68 ms | 9 ms |
| 800 | 273 ms | 22 ms |
| 1600 | 1,059 ms | 44 ms |
| 3200 | 4,669 ms | 181 ms |

A whole `coil compile` of `fn f(int x) -> string` with N
`if x == k { s = s + "a"; }` (release, 2026-10-19, best of 3, two runs)
went 0.31–0.32 s → 0.22–0.25 s at 400, 0.79–0.94 s → 0.55–0.58 s at 800,
and 2.72–2.87 s → 1.55–1.61 s at 1600.
LIR emission is ~13% of that compile at 1600; what stays superlinear there
is `licm::dominators` (dominator sets per block, ~43% at 1600, #1518), and
parsing, `precise_frames` and HIR id hashing. While such a body still has a
register per value (below ~127 arms) its clears were themselves quadratic
code (#1519).

They are linear now (#1519). Each taken arm zeroed every register an
earlier value of the chain lived in, twice: once beside its `ClearWord`
(`source_clears`) and again past the edge's φ copies (`entry_clears`).
`prune_clears` runs a forward dataflow, after the three clear plans, of
the registers that may hold a word this frame wrote since its entry, and
drops a clear of a register that holds none on any path in: zeroed by an
earlier clear with no write after, or never written. (The precise frame
maps count a word the frame never wrote as no root, as they do a zero, so
neither changes what a collection scans.) Writes count wherever emission
may put them: a block's defined registers at its top and again before its
terminator, besides where each op stands; an edge's φs and match payloads
before its clears; parameters and words no op defines are never zero. That
alone left each arm zeroing every earlier join's register (the path that
skipped the arms after it still held the old word there): `n²/2`. So a
branch edge into a rebound join now zeroes its own copy's source, as a
jump edge did: a skipped arm leaves its join's previous register zero once,
on the edge, and later arms need no clear of it. The unit test
`lir_clears_about_linear_over_rebinding_joins` counts `CONST 0; STORE`
pairs over `N` reachable arms:

| N | before | dataflow only | with branch-edge clears |
|---|---|---|---|
| 25 | 1,177 | 325 | 73 |
| 100 | 19,702 | 5,050 | 298 |

`fn f(int x) -> string` with N `if x == k { s = s + "a"; }`,
`coil dissect --fn f` (release, 2026-10-19):

| N | frame words | ops in `f` before | after | clears before | after |
|---|---|---|---|---|---|
| 25 | 52 | 2,659 | 451 | 1,177 | 73 |
| 50 | 102 | 10,309 | 901 | 4,852 | 148 |
| 100 | 202 | 40,609 | 1,801 | 19,702 | 298 |
| 200 | 5 | 4,793 | 4,793 | 994 | 994 |

(At 200 the registers are shared and the code was already linear.) A path
now zeroes each register at most once: an arm zeroes the join's register it
read, and its own result after the copy; a skipped arm costs one clear on
its branch edge, which no arm paid before.

Host Option / `Result<(),E>` / heap-heap Result still pack once at
`HostInvoke` (`host_enum`). MIR does not add a second pack.

## Out of scope (later tickets)

- P5 — optional Cranelift

## Acceptance

`mir::mandelbrot_inner_loop` is typed SSA. `tests/codegen/dense_float.hy`
and `examples/perf/mir_dense_float.hy` (`escape`) execute via `DenseBin`.
Flagship `mandelbrot.hy` (three nested loops) is eligible for dense.
`Result<int,int>` / `Option<int>` leafs lower through MIR→LIR with the
shipped two-slot ABI (`tests/errors/result_int_two_slot.hy`).

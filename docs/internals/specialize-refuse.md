# Dense specialize refuse inventory

`try_specialize_body` runs after stack-IL opts. Infer + SSA lower + dense emit
(or P12 saxpy-reduce) replace a whole function. Leftovers try IL→MIR→LIR.
Keep/refuse is **checksum + cost vs fuse-IL**. Everything else stays fuse-IL.

There is no in-repo stdlib hot path (collections / HTTP live in other repos).
This table is `examples/perf/` plus a few numeric demos.

Doctrine: [opt-generalization.md](opt-generalization.md). Islands:
[mir-islands.md](mir-islands.md). Quirks Q1–Q9:
[language-quirks.md](language-quirks.md).

## Hard walls (today)

True barriers (unsound or missing ABI / maps / debugger). Q6–Q9 first
rungs are **not** in this table — see ladders below and
[opt-generalization.md](opt-generalization.md) B0.

| Wall | Today | Commit |
|------|-------|--------|
| Unicode / regex in SSA | out of MIR | Q9 **R4** leftover after **B9** / R3 maps |
| User `Iterator` / coro / dict / heap-field range `for` | fuse-IL | later Q6 rung |
| Dense+match boxed `JumpIfMatch` (heap enum) | MIR→LIR (I2) | later island |
| Multi-payload `Unpack` / `JumpIfMatch` arity > 1 | fuse-IL | later island |
| Native deopt resume maps | compiler sidecar (`DraftDeoptMap`); not archived; emit skips `Deopt` | **I7** / **C3** — maps exist; P5 resume leftover |
| Incomplete deopt maps (stack-only / convoy TOS) | native must refuse | **C3** leftover |
| Unmapped alloc / GC safepoint | fuse-IL unless S2b draft binds | **B6** maps `ArrayPush` / CALL+`Make*`; leftover unmapped edges stay fuse-IL |
| Residual `Byte` / `Pow` / `AND`/`OR` | fuse-IL | later island |
| LIR one-word `CALL` / HostInvoke reconstruct | fuse-IL (dense may still emit) | I6 |
| LIR one-word sibling `TailCall` | fuse-IL (dense may still emit) | I6 |
| `CALL` / `RETURN` width > `MAX_MODELED_RET_WORDS` (2) | fuse-IL | C1 leftover — extra dests + archive encoding |

## Ladders / cost-gated (were A3 walls)

| Shape | After Q6–Q9 | Keep |
|-------|-------------|------|
| Counted `for` (array / Vec / `[T; N]` / literal range) | **Q6** dense helpers | Cost gate; `main` + format / grow still fuse-IL |
| One-word self-`CALL` / `TailCall` | **Q7** eligible | Cost gate; **B2** convoy reconstruct (no Seek tax on param-only leafs) |
| Sibling / mutual `TailCall` | **B7** eligible | Cost gate; stack-arg + reserved callee entry labels |
| Self two-slot `CALL` / `RETURN` | **C1** eligible | Cost gate; dest + `dest_hi`; N>2 stays refuse |
| Two-slot helper `CALL` / `RETURN` | **B3** eligible | Cost gate; LIR reconstructs width-2 `CALL`; dense may keep |
| Niche / two-slot match | **Q8** dense register `Br` | Cost gate vs LIR / fuse |
| `STRING` / `PRINT` / `FORMAT` / `STRINGIFY` | **Q9** R1 MIR→LIR; **R3** maps `FORMAT` / `STRINGIFY` | Cost gate; dense infer still refuses |
| `from_bytes` / `to_bytes` | **Q9** R2 I6 dense HostInvoke | Cost gate; box at the host edge; LIR still cannot reconstruct HostInvoke |
| Compare-only (no float/i64/i32 arith) | I8 LIR or fuse-IL | Cost gate |

Post-loop-only `return [x]` stays fuse-IL so invert+fuse (COI-87) stays
observable. Boxed LOAD/STORE heap residuals lose the cost gate
([s2d-inloop-make-tax.md](s2d-inloop-make-tax.md)).

## Cost gate (not a feature checklist)

Lift when infer can type the body. Opt on SSA. Emit dense-native (A2) or
LIR. Keep only when reconstruct cost ≤ opted fuse-IL.

Folded former floors:

- **W3 `work_ops ≥ 8`** — tiny helpers (`i + j * 2`) lose on measured
  `Seek` / LOAD / STORE tax. `eval_a` / `mir_dense_straight` still win.
- **Seek ≤ 64** — frame size weights `Seek` in emit cost; last-arm writes
  are a soundness check, not a prove-frame cap.
- **W4 HostInvoke id allowlists** — LICM hoists when purity bits say
  scalar-pure (`classify_host_name`) and the native is not heap-reading
  `packed_*`. Dense emit reconstructs I6-typed hosts, including Q9 R2
  `from_bytes` / `to_bytes`. Q9 R1 reconstructs `STRING` / `PRINT` /
  `FORMAT` / `STRINGIFY` on MIR→LIR.

Loops amortize prologue `Seek`; they skip the static straight-line compare
unless the reconstruct is a select diamond or leftover in-loop `Make*`.

## `examples/perf` bodies

| Body | File | Outcome | Reason |
|------|------|---------|--------|
| `mandelbrot` | `mandelbrot.hy` | dense | nested `*` |
| `escape` | `mir_dense_float.hy` | dense | `*` kernel |
| `hot` | `mir_dense_addf.hy` / `*_divf` / `mir_instcombine` / `mir_destprop` / `mir_iv_sr` / `mir_float_pipeline` | dense | float arith |
| `pack` | `mir_simd_axpy.hy` | HostInvoke 136 | P12 pack |
| `hot` | `mir_dense_i64.hy` | dense | counted i64 |
| `hot` | `mir_dense_straight.hy` | dense | straight-line wins cost |
| `hot` | `mir_dense_host.hy` | dense + HostInvoke | pure `math_sin` in loop |
| `hot` / `kernel` | `mir_dense_call.hy` | dense + dense `CALL` | one-word CALL |
| `main` / `iv_mul` / `nested` | `numeric.hy` / `iv_mul_sr.hy` / `licm_nested_chains.hy` | dense | counted i64 |
| `eval_a` | `nbody.hy` | dense | straight-line wins cost |
| `times_a` / `times_at` | `nbody.hy` | dense + open CALL + Index | S3 / A2 |
| `sum` / `fill` / `scan` / `axpy` | `indexed_sum.hy` / `vec_scan.hy` / `vec_axpy.hy` | `V*` / dense Index | S5 |
| `sum` | `for_in_sum.hy` | `VReduce` / dense Index | **Q6** counted array |
| `main` | `for_in_sum.hy` | fuse-IL | format + `Vec.push` (cost / grow; Q9 R3 maps format; dense still refuses) |
| `range_sum` | `for_in_range.hy` | dense counted i64 | **Q6** literal range |
| `range_sum` | `for_in_range_value.hy` | dense counted i64 | **B5** first-class range local |
| `range_sum` | `for_in_range_param.hy` | dense counted i64 | **C2** Range parameter |
| `range_sum` | `for_in_range_ret.hy` | dense counted i64 | **C2** returned Range |
| `main` | `operators_loop.hy` | fuse-IL | `Pow` / bitwise |
| `main` | `field_hot.hy` | fuse-IL | escaping class / `CALL` |
| `tak` / `fib` | `tak.hy` / `fib.hy` | dense or fuse-IL | **Q7** + **B2** convoy; keep when cost ≤ fuse |
| sibling `TailCall` (even/odd) | `tail_sibling.hy` | dense or fuse-IL | **B7** stack-arg `TailCall` + cost gate |
| self two-slot `CALL` / `RETURN` | `self_two_slot.hy` / `option_self_call.hy` | dense, LIR, or fuse-IL | **C1** dest + `dest_hi`; cost gate vs fuse |
| `nsieve` | `nsieve.hy` | dense or fuse-IL | **B6** mapped `Vec.push`; keep when cost ≤ fuse |
| `binary_trees` | `binary_trees.hy` | fuse-IL | heap / classes / recursion |
| `option_local_match` / in-frame two-slot match + arith | `option_local_match.hy` | dense or fuse-IL | **Q8** register `Br`; cost gate vs LIR/fuse |
| `*_churn` / `option_int_churn` / `result_int_churn` | several | dense, LIR, or fuse-IL | **B3** two-slot helper `CALL` / `RETURN`; cost gate vs fuse |
| `hot` / match+call | `option_match_call.hy` | dense or LIR | **B3** two-slot CALL + Q8 `Br` |
| `match_*` boxed enum | several | fuse-IL or LIR | boxed `JumpIfMatch` stays I2 LIR |
| `array_mut` | `array_mut.hy` | fuse-IL | `main` + write / format (Q9 R3 maps format; does not densify) |
| `bump` | `looping_makearray.hy` | dense or SROA | mapped preheader or slot SROA |
| `pack` | `s2d_inloop_pack_store.hy` | dense SROA | computed-index select |
| `pack` | `s2d_inloop_escape.hy` | dense-native or fuse-IL | A2 + cost gate |
| `dict_*` / `gc_churn` / `coro_ping` | several | fuse-IL | heap / host / `new` |

S2f scalarizes `[T; N]` when the index is proven (`i % N`; **Q4**).
S2g boxes once at a named escape (**Q1**). Grow on `[T; N]` is a type
error (**Q3**). A2 emits `DenseIndex` / `DenseMake` / `DensePush`.

A/B: prefer `coil-embed`; flagships flat (±5%) or identical archives; no
vanity microbenches. Refuse tables shrink toward hard walls
([COI-336](https://linear.app/ardax/issue/COI-336/a3-broaden-mir-entry-shrink-refuse-tables)).
Post-Q6–Q9 ranked revisit: [opt-generalization.md](opt-generalization.md) B0
([COI-338](https://linear.app/ardax/issue/COI-338/b0-post-quirks-refuse-audit-ranked-revisit-plan)).
**B1** ([COI-339](https://linear.app/ardax/issue/COI-339)) is entry hygiene
for the Q6–Q8 first rungs (tables + `lir_eligible` / infer). **B2**
([COI-340](https://linear.app/ardax/issue/COI-340)) is the Seek / frame
parking reconstruct so tight `fib` / `tak` can win the cost gate.
**B3** ([COI-341](https://linear.app/ardax/issue/COI-341)) opens two-slot
helper `CALL` / `RETURN` on dense and LIR; keep/refuse is still cost.
**B6** ([COI-344](https://linear.app/ardax/issue/COI-344)) maps
`ArrayPush` grow sites and CALL+alloc drafts; cost gate still refuses
boxed reconstruct.
**B7** ([COI-345](https://linear.app/ardax/issue/COI-345)) opens sibling /
mutual `TailCall` (one-word and two-slot); keep/refuse is still cost.
**C1** ([COI-349](https://linear.app/ardax/issue/COI-349)) opens self
two-slot `CALL` / `RETURN` on the same dest + `dest_hi` reconstruct.
N>2 stays refuse (`MAX_MODELED_RET_WORDS`). LIR still cannot reconstruct
one-word `CALL` / `TailCall`.
**B8** ([COI-346](https://linear.app/ardax/issue/COI-346)) drops the
debugger-attached / `-Og` specialize refuse. Majority bodies may dense
or LIR; the VM debugger steps the reconstruct. **C3** records resume
maps and remaps named lets; leftover: P5 native resume, incomplete
convoy maps, per-PC locals, codegen-unknown line locs.
**B9** ([COI-347](https://linear.app/ardax/issue/COI-347)) maps
`FORMAT` / `STRINGIFY` (Q9 R3). Dense infer still refuses table ops.
Leftover: unicode / regex in SSA (R4).

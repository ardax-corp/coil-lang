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
| I4 `from_bytes` / `to_bytes` (dense HostInvoke) | fuse-IL / I6 typed, off dense | **Q9** R2 |
| Unicode / regex in SSA | out of MIR | later Q9 rung |
| Mutual / two-slot recursive `CALL` | fuse-IL | later Q7 rung |
| User `Iterator` / coro / dict / first-class range `for` | fuse-IL | later Q6 rung |
| Dense+match boxed `JumpIfMatch` (heap enum) | MIR→LIR (I2) | later island |
| Multi-payload `Unpack` / `JumpIfMatch` arity > 1 | fuse-IL | later island |
| Debugger-attached / `-Og` | fuse-IL | **I7** (stays) |
| Unmapped alloc / GC safepoint | fuse-IL | maps (I5 / S2b) |
| Residual `Byte` / `Pow` / `AND`/`OR` | fuse-IL | later island |
| Two-slot `CALL` / `RETURN` (LIR reconstruct) | LIR / fuse-IL | B3 |
| LIR `CALL` / HostInvoke reconstruct | fuse-IL (dense may still emit) | B3 / I6 |

## Ladders / cost-gated (were A3 walls)

| Shape | After Q6–Q9 | Keep |
|-------|-------------|------|
| Counted `for` (array / Vec / `[T; N]` / literal range) | **Q6** dense helpers | Cost gate; `main` + format / grow still fuse-IL |
| One-word self-`CALL` / `TailCall` | **Q7** eligible | Cost gate; `tak` / `fib` lose (Seek tax) |
| Niche / two-slot match | **Q8** dense register `Br` | Cost gate vs LIR / fuse |
| `STRING` / `PRINT` / `FORMAT` / `STRINGIFY` | **Q9** R1 MIR→LIR | Cost gate; dense infer still refuses |
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
  `packed_*`. Dense emit already reconstructs other I6-typed hosts except
  I4 string bytes (`from_bytes` / `to_bytes`). Q9 R1 reconstructs
  `STRING` / `PRINT` / `FORMAT` / `STRINGIFY` on MIR→LIR.

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
| `main` | `for_in_sum.hy` | fuse-IL | format + `Vec.push` (cost / grow; Q9 R1 can lift format alone) |
| `range_sum` | `for_in_range.hy` | dense counted i64 | **Q6** literal range |
| `main` | `operators_loop.hy` | fuse-IL | `Pow` / bitwise |
| `main` | `field_hot.hy` | fuse-IL | escaping class / `CALL` |
| `tak` / `fib` | `tak.hy` / `fib.hy` | fuse-IL | **Q7** eligible; cost gate loses (Seek tax) |
| sibling `TailCall` (even/odd) | — | fuse-IL or dense | open one-word `TailCall` + cost gate |
| `nsieve` | `nsieve.hy` | fuse-IL | `Vec.push` (no `Make*`) |
| `binary_trees` | `binary_trees.hy` | fuse-IL | heap / classes / recursion |
| `option_local_match` / in-frame two-slot match + arith | `option_local_match.hy` | dense or fuse-IL | **Q8** register `Br`; cost gate vs LIR/fuse |
| `*_churn` / `option_int_churn` / `result_int_churn` | several | fuse-IL or LIR | two-slot `CALL` / `RETURN` still LIR; match diamond may dense |
| `match_*` boxed enum | several | fuse-IL or LIR | boxed `JumpIfMatch` stays I2 LIR |
| `array_mut` | `array_mut.hy` | fuse-IL | `main` + write / format (Q9 R1 does not densify) |
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
for the Q6–Q8 first rungs (tables + `lir_eligible` / infer). Cost gate
unchanged — do not force dense on `fib` / `tak` Seek loses (B2).

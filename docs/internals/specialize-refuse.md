# Dense specialize refuse inventory (COI-287 W0)

`try_specialize_body` runs after stack-IL opts. Infer + SSA lower + dense emit
(or P12 saxpy-reduce) replace a whole function. Everything else stays fuse-IL
(or P3 MIR→LIR for two-slot leafs).

There is no in-repo stdlib hot path (collections / HTTP live in other repos).
This table is `examples/perf/` plus a few numeric demos.

## Gates (after W1)

| # | Refuse | Typical IL | Next cut |
|---|--------|------------|----------|
| 1 | No back-edge | straight-line kernel (`eval_a`) | W3 (optional, cost-gated) |
| 2 | Need float `+`/`-`/`*`/`/` or `i32` | i64-only counted loops; float compare-only | W2 (i64 counted) |
| 3 | Non-numeric IL | `CALL` / `HostInvoke` / heap index / class field / match / string | W4 (limited inward edges) |
| 4 | Multi-word `RETURN` | two-slot Option/Result | P3 LIR (already on) |
| 5 | Residual `Byte` / `Pow` / `AND`/`OR` | `operators_loop` | stay fuse-IL |

`has_i32` is effectively unused: language `int` is `i64`, and infer paints
integer bins as `I64`. Integer loops therefore hit gate 2 today (W2).

`DIVF` already set the old `has_fmul` flag; W1 widens that flag to `ADDF` /
`SUBF` / `MULF` / `DIVF` (and float `INC`/`DEC`).

## `examples/perf` bodies

| Body | File | Outcome | Reason / phase |
|------|------|---------|----------------|
| `mandelbrot` | `mandelbrot.hy` | dense | nested `*` — P6 |
| `escape` | `mir_dense_float.hy` | dense | `*` kernel — P1 |
| `hot` | `mir_dense_addf.hy` | dense | `+`/`-` no `*`/`/` — **W1** |
| `hot` | `mir_cse_divf.hy` | dense | `DIVF` — P2 |
| `hot` | `mir_gvn_divf.hy` | dense | `DIVF` — P10 |
| `hot` | `mir_licm_divf.hy` | dense | `DIVF` — P6 |
| `hot` | `mir_instcombine.hy` | dense | `*` — P7 |
| `hot` | `mir_destprop.hy` | dense | `*` — P8 |
| `hot` | `mir_iv_sr.hy` | dense | `*` — P9 |
| `hot` | `mir_float_pipeline.hy` | dense | `*` `/` — P11 |
| `pack` | `mir_simd_axpy.hy` | HostInvoke 136 | P12 (eligible, then pack) |
| `main` | `numeric.hy` | fuse-IL | i64 add — **W2** |
| `iv_mul` | `iv_mul_sr.hy` | fuse-IL | i64 mul — **W2** |
| `nested` | `licm_nested_chains.hy` | fuse-IL | i64 add — **W2** |
| `eval_a` | `nbody.hy` | fuse-IL | no back-edge — W3 |
| `times_a` / `times_at` | `nbody.hy` | fuse-IL | `CALL` + heap/index — W4 |
| `sum` | `indexed_sum.hy` | fuse-IL | heap/index |
| `fill` / `scan` | `vec_scan.hy` | fuse-IL | heap/index |
| `main` | `for_in_sum.hy` | fuse-IL | heap + `for` iterator |
| `main` | `operators_loop.hy` | fuse-IL | `Pow` / bitwise |
| `main` | `field_hot.hy` | fuse-IL | class/field + `CALL` |
| `tak` / `fib` | `tak.hy` / `fib.hy` | fuse-IL | `CALL` (recursion) |
| `nsieve` | `nsieve.hy` | fuse-IL | heap/index |
| `binary_trees` | `binary_trees.hy` | fuse-IL | heap / classes |
| `*_churn` / `option_*` / `result_*` | several | fuse-IL or LIR | heap / match / two-slot — P3 |
| `match_*` / `dict_*` / `gc_churn` / `coro_ping` | several | fuse-IL | match / heap / host |

Intentional fuse-IL hit benches (`numeric`, `iv_mul_sr`, `licm_nested_chains`,
stack-IL `cse_*`, `dest_prop_field_alias`) stay off dense: they are i64 or
heap, not float add-only. W1 does not flip them.

## W2 / W3 / W4 (not this PR)

- **W2** — drop the i32 quirk so safe i64 counted loops specialize (`numeric`,
  `iv_mul_sr`, `licm_nested_chains`). Separate kick.
- **W3** — cost-gated straight-line numeric (`eval_a`). Only if demand.
- **W4** — inward `CALL` to known numeric leafs (`times_a` → `eval_a`). Later.

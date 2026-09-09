# Dense specialize refuse inventory (COI-287 W0)

`try_specialize_body` runs after stack-IL opts. Infer + SSA lower + dense emit
(or P12 saxpy-reduce) replace a whole function. Everything else stays fuse-IL
(or P3 MIR→LIR for two-slot leafs).

There is no in-repo stdlib hot path (collections / HTTP live in other repos).
This table is `examples/perf/` plus a few numeric demos.

## Gates (after COI-291)

| # | Refuse | Typical IL | Next cut |
|---|--------|------------|----------|
| 1 | Straight-line below cost gate | `i + j * 2` (2 work ops) | stay fuse-IL |
| 2 | Need float or i64 arith (or `i32`) | float compare-only | stay fuse-IL |
| 3 | Non-numeric IL | `TailCall` / two-slot `CALL` / I4 string HostInvoke / class field / match / string / `FORMAT` | I4 barrier (string/format stay fuse-IL). S3: one-word `CALL`, I6 HostInvoke, heap index |
| 4 | Multi-word `RETURN` | two-slot Option/Result | P3 LIR (already on) |
| 5 | Residual `Byte` / `Pow` / `AND`/`OR` | `operators_loop` | stay fuse-IL |

**W4 allowlisted HostInvoke (inside dense):** LICM still hoists only these
ids. S3 dense emit also reconstructs other I6-typed HostInvoke edges
(box → call → unbox) except I4 `from_bytes` / `to_bytes`. User `CALL` is
one-word: dense ABI map **or** open fuse-IL / LIR callee (S3).

| Ids | Names |
|-----|--------|
| **87–91** | `packed_dot`, `packed_matmul`, `packed_matrix_zip`, `packed_matrix_neg`, `packed_vec_arith` |
| **102–110** | `math_sin` … `math_pow` (frozen prelude math) |
| **125–135** | `math_atan` … `math_tanh` (M1 prelude math) |
| **136** | `simd_axpy_reduce` (`coil-simd`; also P12 whole-body pack) |

At those edges, dense emit **boxes** typed slots onto the Value stack,
`HostInvoke`s, **unboxes** the result into a typed slot, and continues
dense. Packed LA args stay `i64` Value words (heap pointers); math / axpy
are scalar `f64` (axpy `n` is `i64`). No open-ended user methods.

**W3 cost gate (no back-edge):** infer still requires numeric IL (S3
allows one-word `CALL`, I6 HostInvoke except I4 string bytes, and heap
index; still refuse class field / match / string / multi-word `RETURN` /
residual `Byte` / `Pow` / `AND`/`OR`) and float
`+`/`-`/`*`/`/` or i64 `+`/`-`/`*`/`/`/`%` (or int/`float` `INC`/`DEC`).
Compare-only still refuses. A body **without** a back-edge also needs
`numeric_work_ops >= STRAIGHT_LINE_MIN_WORK_OPS` (**8**).

Work ops are `Bin` / `BinSlotImm` / `BinSlotSlot` plus residual
`INC`/`DEC`/`NEG`/`NEGF`/`CastIntToFloat`. Load / Store / Const / control
are free. Rationale: dense `Seek` + Value ABI at CALL/RETURN is a fixed
per-invocation tax. Loops amortize it over trips; a 2–4 op helper does not.
Eight is past that handful. Loops do **not** use this count (back-edge is
enough, same as W1/W2).

**W2 safety (counted i64):** infer already requires a back-edge **or** the W3
gate. The body must be numeric IL. Qualifying arith is i64 `+`/`-`/`*`/`/`/`%`
or int `INC`/`DEC` — compare-only still refuses. Nested / multi-header loops
are eligible, same as float. `has_i32` stays in the gate but language `int`
is `i64`, so infer still paints integer bins as `I64`.

W1: `DIVF` already set the old `has_fmul` flag; that flag is `ADDF` / `SUBF` /
`MULF` / `DIVF` (and float `INC`/`DEC`).

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
| `hot` | `mir_dense_i64.hy` | dense | i64 +/− counted — **W2** |
| `hot` | `mir_dense_straight.hy` | dense | no back-edge, ≥8 work ops — **W3** |
| `hot` | `mir_dense_host.hy` | dense + HostInvoke | allowlisted `math_sin` in loop — **W4** |
| `hot` / `kernel` | `mir_dense_call.hy` | dense + dense `CALL` | leaf-first typed CALL — **COI-291**; S3 also open one-word CALL |
| `main` | `numeric.hy` | dense | i64 add — **W2** (side effect) |
| `iv_mul` | `iv_mul_sr.hy` | dense | i64 mul — **W2** (side effect) |
| `nested` | `licm_nested_chains.hy` | dense | i64 add — **W2** (side effect) |
| `eval_a` | `nbody.hy` | dense | no back-edge, ≥8 work ops — **W3** |
| `times_a` / `times_at` | `nbody.hy` | dense + open CALL + unpinned Index | S3b sound residuals |
| `sum` | `indexed_sum.hy` | V1 `VReduce` (stride-1); gather stays dense Index | S5b / S3b |
| `fill` / `scan` | `vec_scan.hy` | `fill`: V0 SIMD; `scan`: V1 `VReduce` | stride-1 store / add-reduce |
| `axpy` / `checksum` | `vec_axpy.hy` | `axpy`: V1 `VFma`; `checksum`: V1 `VReduce` | conservative saxpy store |
| `main` | `for_in_sum.hy` | fuse-IL | heap + `for` iterator |
| `main` | `operators_loop.hy` | fuse-IL | `Pow` / bitwise |
| `main` | `field_hot.hy` | fuse-IL | class/field + `CALL` |
| `tak` / `fib` | `tak.hy` / `fib.hy` | fuse-IL | `CALL` (recursion) |
| `nsieve` | `nsieve.hy` | fuse-IL | heap-index + `Vec.push` (no `Make*`; S2d does not fire) |
| `binary_trees` | `binary_trees.hy` | fuse-IL | heap / classes / recursion |
| `*_churn` / `option_*` / `result_*` | several | fuse-IL or LIR | heap / match / two-slot — P3 |
| `array_mut` | `array_mut.hy` | fuse-IL | `main` + I4 write; S2d does not fire |
| `bump` | `looping_makearray.hy` | dense | S2d preheader `MakeArray` + computed-index store |
| `match_*` / `dict_*` / `gc_churn` / `coro_ping` | several | fuse-IL | match / heap / host / class `new` |

Stack-IL `cse_*` / `dest_prop_field_alias` stay fuse-IL (heap / field). W2
does not rewrite those sources; `numeric` / `iv_mul_sr` / `licm_nested_chains`
now meet the counted-i64 gate and emit dense. The W3 prove bench is
`mir_dense_straight.hy`. The W4 prove bench is `mir_dense_host.hy`
(`sin` inside an otherwise dense loop). The COI-291 prove bench is
`mir_dense_call.hy` (`hot` loops a dense `kernel`). S3 open CALL lets
`times_a` call `eval_a` with unpinned dense Index residuals (S3b).
Recursion (`tak` / `fib`) stays fuse-IL on the callee. Mapped **preheader**
`MakeArray` plus an index loop may take dense (S2d). S2e dropped
per-residual `Seek` restore; in-loop `Make*` still stays off dense
(LOAD/STORE boxing; [s2d-inloop-make-tax.md](s2d-inloop-make-tax.md)).
Compare-only leftovers may take LIR when maps exist and the cost gate holds.
Const-index `s += xs[0]` usually mem_fwd+DCE's the `MakeArray` before MIR. A live heap return (`return [i]`) plus a counted
loop and **no** earlier alloc stays fuse-IL so invert+fuse remains
observable. Unmapped alloc, CALL+alloc (map lift refuses `CALL`), and
`Vec.push` / class `new` loops stay fuse-IL.

## Language refuse → island (COI-292 I0)

Dense gates above are the numeric inventory. This table is the **language**
refuse map for MIR islands. Full doctrine: [mir-islands.md](mir-islands.md).

| Feature | Today | Island |
|---------|-------|--------|
| Heap-ref / niche Option/Result *types* in SSA | layout exists (`HeapNiche`); SSA paints `i64`/`value` | **I1** — name + carry; no alloc specialize |
| `match` / `JumpIfMatch` on niche / two-slot / boxed unary (any tag, arity ≤ 1 incl. overlap 0) | MIR→LIR (I2); **dense+match stays refuse** (stack match vs dense regs) | **I2** / S3 leftover |
| Non-escaping class fields (local-escape sidecar) | MIR→LIR `FieldLoad` / `FieldStore` (unboxed slots); dense refuse | **I3** |
| `FORMAT` / `STRING` / `STRINGIFY` / `PRINT` | fuse-IL (dense + MIR→LIR refuse) | **I4 barrier** — no subset |
| `MakeArray` / alloc / GC safepoints | SSA `Alloc` + `GcBarrier`; S2a roots; S2b maps; S2c dense / LIR **only when maps exist**; S2d mapped preheader Make*; S2e Seek-less residuals (in-loop Make* still refuse); unmapped fuse-IL; post-loop-only `return [x]` fuse-IL | **I5** / **S2a** / **S2b** / **S2c** / **S2d** / **S2e** |
| HostInvoke outside W4; purity-driven barriers | SSA `HostInvoke` + effect bits (`allow_effects`); LICM never hoists impure; S3 dense emit reconstructs I6-typed hosts except I4 string bytes | **I6** / **S3** |
| Debugger / deopt edges | SSA `Deopt` + implicit leave; debugger-attached / `-Og` refuse specialize | **I7** |
| Broader MIR emit entry | IL→MIR→LIR when `lir_eligible` (I1–I3 / two-slot / inferable leftover: if/compare, store-only, tiny let; I4–I7 refuse) | **I8** |
| Escaping classes, boxed nested enums, recursion | fuse-IL | stay refuse unless a later island says otherwise |
| Compiler SIMD (`V*`) | stride-1 store + add-reduce + conservative FMA | **S5a V0** / **S5b V1** — refuse alloc/GC, match, impure CALL/host, debugger/`-Og`, heap in vregs |
| Cranelift | parked (P5) | not an island |

A/B: prefer `coil-embed`; flagships flat (±5%) or identical archives; no
vanity microbenches. Identical flagship `.hyc` is expected while an island
does not fire on those bodies.

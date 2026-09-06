# Numeric MIR (COI-267 / COI-268)

Typed SSA sidecar for a **numeric subset**, plus **dense bytecode** for
specialized float/i32 loops (P1).

## Where it lives

`compiler/src/mir/` — not inside `il/`. Fuse-IL stays the production lowerer
for non-specialized functions. Dense emit replaces a whole function body
before stack-IL opts when the body qualifies.

| Piece | Role |
|-------|------|
| `MirTy` | Lattice: `bottom ⊑ {i32⊑i64, f32⊑f64, bool} ⊑ value` |
| `MirBuilder` | Braun SSA (locals = IL slots, explicit φ) |
| `try_lower_numeric` | Pre-fuse `IlOp` → SSA; refuses classes / heap / calls |
| `try_specialize_body` | Infer + SSA + MIR CSE + dense emit for float-mul / i32 loops |
| `mir::cse` | Same-block GVN (includes `DIVF`/`DIV` that stack-IL CSE refuses) |
| text form | Print / parse for round-trip tests |

Language `int` / `float` / `bool` map to `i64` / `f64` / `bool`.

## P1 — dense exec (COI-268)

Eligible **leaf, single-header** numeric loops (float `*`/`/` or `i32`, no
heap/calls, one back-edge) emit:

- `DenseBin` / `DenseConst` / `DenseMove` / `DenseUnary` / `DenseCast`
- `Seek` to the typed slot high-water mark
- Fuse-select `LOAD`/`LOAD`/`cmp`/`JMPF` (→ `BinSlotSlotJmpf`) and `RETURN`
  at control and **Value ABI** edges

Nested loops (flagship `mandelbrot.hy`) stay on fuse-IL. The hit bench
`examples/perf/mir_dense_float.hy` (`escape`) is the dense kernel.

CALL still places args as `Value` words in slots `0..arity`. Dense ops
reinterpret those bits as `i64`/`f64`. RETURN loads one word back onto the
stack. Multi-word / niche layouts stay on the fuse-IL path (P3).

Int-only and add-only float loops stay on fuse-select so existing CSE/LICM
hit benches are unchanged.

## P2 — MIR CSE (COI-269)

After SSA lower, **local GVN** runs on the numeric function before dense emit.
Stack-IL `local_cse` / `ssa_gvn` still refuse `DIV`/`MOD`/`DIVF`/`MODF`; those
ops are numbered here. Fuse-IL InstCombine / CSE / LICM are unchanged for
non-dense bodies. Hit bench: `examples/perf/mir_cse_divf.hy`.

## Out of scope (later tickets)

- P3 — multi-word / niche as MIR→LIR
- P4 — native SIMD package
- P5 — optional Cranelift

## Acceptance

`mir::mandelbrot_inner_loop` is typed SSA. `tests/positive/mir_dense_float.hy`
and `examples/perf/mir_dense_float.hy` (`escape`) execute via `DenseBin`.
Flagship `mandelbrot.hy` (three nested loops) remains fuse-IL.

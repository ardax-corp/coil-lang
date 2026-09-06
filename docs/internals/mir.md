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
| `try_specialize_body` | Infer + SSA + dense emit for float-mul / i32 loops |
| text form | Print / parse for round-trip tests |

Language `int` / `float` / `bool` map to `i64` / `f64` / `bool`.

## P1 — dense exec (COI-268)

Eligible **leaf** numeric loops (float `*`/`/` or `i32`, no heap/calls) emit:

- `DenseBin` / `DenseCmp` / `DenseConst` / `DenseMove` / `DenseUnary` / `DenseCast`
- `Seek` to the typed slot high-water mark
- Existing `LOAD` + `JMPF` / `RETURN` at control and **Value ABI** edges

CALL still places args as `Value` words in slots `0..arity`. Dense ops
reinterpret those bits as `i64`/`f64`. RETURN loads one word back onto the
stack. Multi-word / niche layouts stay on the fuse-IL path (P3).

Int-only and add-only float loops stay on fuse-select so existing CSE/LICM
hit benches are unchanged.

## Out of scope (later tickets)

- P2 — move InstCombine / CSE / LICM onto MIR
- P3 — multi-word / niche as MIR→LIR
- P4 — native SIMD package
- P5 — optional Cranelift

## Acceptance

`mir::mandelbrot_inner_loop` is typed SSA. Production `examples/perf/mandelbrot.hy`
and `tests/positive/mir_dense_float.hy` execute the inner float kernel via
`DenseBin` / `DenseCmp`.

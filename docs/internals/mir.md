# Numeric MIR (COI-267 P0)

Typed SSA sidecar for a **numeric subset**. Production execution is unchanged:
stack IL → fuse-select → bytecode → VM `Value`. Dense MIR exec is P1.

## Where it lives

`compiler/src/mir/` — not inside `il/`. Fuse-IL stays instruction lowering
into bytecode (and, optionally, into this MIR). It is not a full LLVM.

| Piece | Role |
|-------|------|
| `MirTy` | Lattice: `bottom ⊑ {i32⊑i64, f32⊑f64, bool} ⊑ value` |
| `MirBuilder` | Braun SSA (locals = IL slots, explicit φ) |
| `try_lower_numeric` | Pre-fuse `IlOp` → SSA; refuses classes / heap / calls |
| text form | Print / parse for round-trip tests |

Language `int` / `float` / `bool` map to `i64` / `f64` / `bool`. `i32` and
`f32` are lattice lanes for later dense / SIMD cuts.

## Out of scope (later tickets)

- P1 — MIR → dense bytecode (`Value` ABI at edges)
- P2 — move InstCombine / CSE / LICM onto MIR
- P3 — multi-word / niche as MIR→LIR
- P4 — native SIMD package
- P5 — optional Cranelift

## Acceptance

`mir::mandelbrot_inner_loop` (and IL lowering of a Mandelbrot-shaped fragment)
represent the inner escape iteration in typed SSA. No classes.

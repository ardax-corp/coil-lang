# Numeric MIR (COI-267 / COI-268) and Result/Option LIR (COI-270)

Typed SSA sidecar for a **numeric subset**, **dense bytecode** for
specialized float/i32 loops (P1), MIR CSE (P2), and **MIR→LIR** for
shipped Result/Option layouts (P3).

## Where it lives

`compiler/src/mir/` — not inside `il/`. Fuse-IL stays the production lowerer
for non-specialized functions. Dense emit replaces a whole function body
before stack-IL opts when the body qualifies. Two-slot leaf helpers replace
the body with stack IL (`emit_lir`) after the same opts when the reconstruct
is no larger than the opted fuse-IL (single-use return/cmp values stay on
the stack).

| Piece | Role |
|-------|------|
| `MirTy` | Lattice: `bottom ⊑ {i32⊑i64, f32⊑f64, bool} ⊑ value` |
| `MirLayout` | Call-edge ABI: `word` / `twoslot` / `heap_niche` |
| `MirBuilder` | Braun SSA (locals = IL slots, explicit φ) |
| `try_lower_numeric` | Pre-fuse `IlOp` → SSA; refuses classes / heap / calls |
| `try_specialize_body` | Infer + SSA + MIR CSE + MIR LICM + MIR InstCombine + dense emit for float-mul / i32 loops |
| `try_lower_abi_body` | Infer + SSA + MIR CSE + LIR emit for two-slot leafs |
| `mir::cse` | Same-block GVN (includes `DIVF`/`DIV` that stack-IL CSE refuses) |
| `mir::licm` | Natural-loop hoist of invariant Const/arith/cmp/cast (float `Div` ok; int `Div`/`Rem` stay) |
| text form | Print / parse for round-trip tests |

Language `int` / `float` / `bool` map to `i64` / `f64` / `bool`.

## P1 — dense exec (COI-268)

Eligible numeric loops (float `*`/`/` or `i32`, no heap/calls, one or more
back-edges) emit:

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

Int-only and add-only float loops stay on fuse-select so existing CSE/LICM
hit benches are unchanged.

## P2 — MIR CSE (COI-269)

After SSA lower, **local GVN** runs on the numeric function before dense emit
(and before P3 LIR emit). Stack-IL `local_cse` / `ssa_gvn` still refuse
`DIV`/`MOD`/`DIVF`/`MODF`; those ops are numbered here. Fuse-IL InstCombine /
CSE / LICM are unchanged for non-MIR bodies. Hit bench:
`examples/perf/mir_cse_divf.hy`.

## P6 — MIR LICM + widen specialize (COI-280)

After CSE, **natural-loop LICM** hoists invariant Const / add/sub/mul /
float div / cmp / unary / cast into a preheader. Integer `Div`/`Rem` stay
in the loop so a skipped trip cannot trap. A second CSE run merges
hoisted consts. Hit bench: `examples/perf/mir_licm_divf.hy`.

Specialize no longer refuses multi-header bodies: nested float-mul loops
(including flagship `mandelbrot`) can emit `DenseBin` when infer + lower
succeed. Int-only and add-only float loops still stay on fuse-IL.

## P7 — MIR InstCombine (COI-281)

After LICM + a second CSE, **typed peeps** run on dense SSA (`f64` / `i32` /
`i64`). Fuse-IL `algebraic` only matches Load/Const/ConstPool windows; this
pass folds binop results too.

Proving set (no FMA / reciprocal — P11):

- const-fold of bin / cmp / unary / cast (refuse int/float ÷0 and `MIN / -1`)
- identities: `x±0`, `x*1`, `x/1`, int `x&-1` / `|0` / `^0` / `<<0`, int `x-x`
  / `x*0` / `x%1` (float `+0.0` / `*1.0` exact bits only; refuse `x*0.0` and
  float `x-x`)
- strength: `x * 2` → `x + x` (int and IEEE `+2.0`; flagship `2.0 * zr` hits)
- const-cond `br` → `jump`

Hit bench: `examples/perf/mir_instcombine.hy`.

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

`try_lower_numeric` accepts `ret_words == 2` (payload then tag on the IL
stack). Dense `infer_numeric` still refuses that shape.

### LIR emit

`emit_lir` writes stack IL only:

- `Seek` + slot `LOAD`/`STORE` / `BinSlotSlot` / fused cmp+`JMPF`
- `RETURN` width 2 for `TwoSlot`, width 1 for `Word` / `HeapNiche`
- niche bits as existing `BITAND` / `BITOR` / `CONST 0` / `LogNot`
- no `MakeEnum`, no `ReturnPair` / `PairToHeap` / niche ISA tombstones

`try_lower_abi_body` + `emit_lir` implement that mapping (leaf two-slot IL →
SSA → fuse-IL, `RETURN` width 2). Production replace is **ON** after
stack-IL opts: `emit_lir` keeps single-use return/cmp values on the stack
and `DUP`s a TOS that is also the first word of `k, k+1`. Int
`slot ⊕ imm` bins emit `BinSlotImm` so pre-fuse cost matches opted
fuse-IL. A body is kept only when emitting cost does not grow.
Stack-IL opts are **not** re-run on the reconstruct (`local_cse`
refuses `MOD` and rematerialized `pair_int_churn`). Callers
(`match f()`, `?`, I/O) stay on fuse-IL.
Hit bench: `examples/perf/result_int_churn.hy`.

Host Option / `Result<(),E>` / heap-heap Result still pack once at
`HostInvoke` (`host_enum`). MIR does not add a second pack.

## Out of scope (later tickets)

- P4 — native SIMD package
- P5 — optional Cranelift

## Acceptance

`mir::mandelbrot_inner_loop` is typed SSA. `tests/positive/mir_dense_float.hy`
and `examples/perf/mir_dense_float.hy` (`escape`) execute via `DenseBin`.
Flagship `mandelbrot.hy` (three nested loops) is eligible for dense.
`Result<int,int>` / `Option<int>` leafs lower through MIR→LIR with the
shipped two-slot ABI (`tests/positive/mir_result_int.hy`).

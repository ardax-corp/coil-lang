# Optimization on the stack IL

No optimization pass runs on the stack IL any more. The last ones were
removed in 2026-10; their work moved to the HIR passes and to HIR lowering
(`codegen/emit_hir.rs`), where the types, locals and control flow are still
explicit. What is left of the stack IL is:

- **lift to MIR** (`IlModule::optimize_and_flatten`): each body tries dense
  MIR, then MIR→LIR reconstruct, else stays fuse-IL;
- **fuse-select** (`il::lower::fuse_select`, below), once after the bodies
  are concatenated, then one PC assignment.

This directory keeps the [`OptimizeOptions`] switches, the [`OptLevel`]
presets that set them, and the `--opt-stats` counters ([`OptStats`]: body
tiers, HIR lowering and inlining).

**Hit-bench prove:** a change that is sound but leaves the flagship `.hyc`
unchanged gets focused `examples/perf` benches, and is proved on those; do not
skip a merge because `mandelbrot` / `tak` / `nsieve` / `binary_trees` / `fib`
are identical. Skip only on a hit-bench wash or regression. See
[optimization-roadmap.md](../../../../docs/internals/optimization-roadmap.md#hit-bench-prove-rule).

## Where each former IL pass went

| Former IL pass | Now | Flag |
|----------------|-----|------|
| constant folding, algebraic identities, `instcombine`, `canon` | `hir::fold` (an `int` literal goes right of a commutative op or a flipped compare) | `algebraic` (every level) |
| local CSE / GVN | `hir::cse` | `local_cse` |
| loop-invariant code motion | `hir::licm` | `licm` |
| counted-loop bounds proofs | `hir::bounds` | `loop_bounds` |
| full unroll of short counted loops | `hir::unroll` | `loop_unroll` (not at Size) |
| escape analysis / scalar replacement | `hir::enum_sroa`, `hir::tuple_sroa` | `escape_analysis` |
| `clone_shared_return` | `hir::sink_return`; lowering returns at a kept value `match`'s join (`hir_return_at_joins`) | `sink_return` (not at Size) |
| `slot_promote`, `dead_store_at` | lowering reads one-word locals in place; `hir::fold` drops stores to locals nothing reads | — |
| `stack_dce` | `CodeBuf::push_pop` drops a pure value pushed just before; `hir::fold` drops `x = x` | — |
| `dead_block` | lowering emits no jump or closing return after an exit (`CodeBuf::ends_in_exit`); the MIR lift skips blocks nothing reaches | — |
| `branch_optimization`, `block_reordering` | early exits laid out after the body (`CodeBuf::move_exits_to_end`) | `branch_optimization` |
| `jump_thread` | an `if` ending a loop body jumps straight to the back edge (`HirEmit::next_jump`) | — |

Removed with no replacement, because measurement showed no effect:
`copy_prop`, `dest_prop`, `mem_fwd` + `dead_store`, `strength_reduce`,
`invariant_store_elim`, `tos_carry`, the return / bin-join / multi-op
convoys, `invert_guard_branch`, `slot_promote_tell`, `ssa_gvn` and per-body
`cfg_gvn`. MIR instcombine / strength reduction / GVN are separate.

The HIR passes run in `emit_hir` after inlining: unroll, fold, cse, licm,
bounds, sink_return.

## Cursor facts: `sp` vs `tell`

| Analysis | Module | Quantity |
|----------|--------|----------|
| **`sp`** | [`crate::il::sp`] | Eval-stack *height* (`stack_delta` feeds `tell`; the whole-buffer analysis backs tests only). Nested `CALL`/`MakeCoro` reset to 1 (return value). `STORE` does **not** floor height. |
| **`tell`** | [`crate::il::tell`] | Shared operand/local *cursor*. `STORE` raises the cursor to `slot + 1` even when height is lower. |

Do not substitute one for the other (COI-81). Height is a per-op delta; the
tier choice and `Seek` normalization need the cursor. `Tell::Unknown` at a join
is often the correct answer (a raising loop header), not a gap.

## Residual `IlOp::Byte`

Hot-path ops are typed variants (`Load`, `Const`, `Bin`, `BinSlot*`, `*Return`,
`HostInvoke`, …). `IlOp::Byte` is the long-tail escape hatch (FORMAT, FFI,
`Seek`, some unaries still waiting on typed lift, packed forms, tests): an
opaque barrier to fuse-select and the MIR lift unless decoded with
`as_encode_byte()`. Absolute `JMP`/`JMPF`/`JMPT` as `Byte` is forbidden before
fuse (`assert_no_residual_abs_jumps`).

## fuse-select (D4, in `lower.rs`)

**Fn:** `il::lower::fuse_select` called from `lower_optimized`. Runs **once**
after concat. Not gated by `OptimizeOptions`. Not a second lowering: PC assign
and encode stay in `lower_optimized`. No post-lower `adjust_target`.

- **Input:** **Typed** [`IlOp`](../op.rs). `Jump`/`Entry` stay symbolic.
  Incoming [`Label`] / [`JoinLabel`] binds and `FuseHint` / `JoinClass` (D3) are
  hard barriers — no dummy `NOOP` / `DUP;POP`. Residual [`IlOp::Byte`] is the
  **cold set** (`FORMAT`, FFI, packed multi-slot LOAD/STORE, unmatched
  `from_plain_byte`) and is **refused** in any multi-op window.
- **Output:** Superinstructions (const fold, `BinSlotImm`/`BinSlotSlot`,
  including the const-left commute `CONST; LOAD; int-bin` (COI-384),
  `*Jmpf`/`*Jmpt`, `*Store`, packed LOAD/STORE n≤3, `*Return`).
  `FloatChainStore` / `BinSlotSlotConstJmpf` are tombstones (not selected). Then one PC
  assignment. `Vec<Byte>` for the archive. Label ids map to PCs; they do not
  survive as IL.
- **Refusals:** Window that would pull a **label** or **abs-jump target** onto a
  non-first op; window that contains residual **`Byte`**; `*Return` fusion when
  window[0] is an **unconditional join** (stacked arm value must be popped).
  **`Entry` CALL / TailCall** is never a fuse window member.
  **`FuseHint`** on the cond-jump (`nofuse` / `ValueUnderJmp`) refuses
  `*Jmpf`/`*Jmpt` fusion (pair-`?` / pair-match keep `EQ;JMPF`). A
  **`JoinLabel`** bind is a value join: same window-break as a label, including
  `CONST;RETURN`. Residual abs JMP as `Byte` panics. Per-function fuse-select
  is not production (measured no win).
- **Tests:** `il/lower.rs` `lower_fuses_bin_slot_slot`, `lower_fuses_bin_slot_imm`,
  `lower_fuses_const_return_imm`, `lower_fuses_load_const_add_store_to_bin_slot_imm_store`,
  `lower_fuses_two_stage_float_chain_store`, `lower_refuses_cmp_jmpf_when_jump_is_nofuse`,
  `lower_refuses_const_return_across_value_join`,
  `fuse_select_refuses_residual_byte_in_window`. Retired float chain:
  `cast_float_chain_is_not_fused`.

Run (from repo root):

```bash
cargo test -p compiler --lib -- il::
```

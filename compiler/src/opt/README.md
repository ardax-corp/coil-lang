# Optimization switches

Optimization passes run on HIR and MIR; none run on a stack IL. This
directory keeps the [`OptimizeOptions`] switches, the [`OptLevel`] presets
that set them, and the `--opt-stats` counters ([`OptStats`]: HIR lowering
and inlining).

**Hit-bench prove:** a change that is sound but leaves the flagship `.ccoil`
unchanged gets focused `examples/perf` benches, and is proved on those; do not
skip a merge because `mandelbrot` / `tak` / `nsieve` / `binary_trees` / `fib`
are identical. Skip only on a hit-bench wash or regression. See
[optimization-roadmap.md](../../../../docs/internals/optimization-roadmap.md#hit-bench-prove-rule).

## Where each former IL pass went

| Former IL pass | Now | Flag |
|----------------|-----|------|
| constant folding, algebraic identities, `instcombine`, `canon` | `hir::fold` (an `int` literal goes right of a commutative op or a flipped compare) | `algebraic` (every level) |
| local CSE / GVN | `mir::cse` / `mir::gvn` (`hir::cse` folded in 2026-10) | — |
| loop-invariant code motion | `mir::licm` (`hir::licm` folded in 2026-10) | — |
| counted-loop bounds proofs | `hir::bounds` | `loop_bounds` |
| full unroll of short counted loops | `hir::unroll` | `loop_unroll` (not at Size) |
| escape analysis / scalar replacement | `hir::enum_sroa`, `hir::tuple_sroa` | `escape_analysis` |
| `clone_shared_return` | `hir::sink_return`; lowering returns at a kept value `match`'s join (`hir_return_at_joins`) | `sink_return` (not at Size) |
| `slot_promote`, `dead_store_at` | MIR keeps one-word locals in registers; `hir::fold` drops stores to locals nothing reads | — |
| `stack_dce` | MIR emits no unused pure value; `hir::fold` drops `x = x` | — |
| `dead_block` | MIR lowering builds no block nothing reaches | — |
| `branch_optimization`, `block_reordering` | early exits laid out after the body (direct MIR's block layout, `HirPlan::cold_ok`) | `branch_optimization` |
| `jump_thread` | an `if` ending a loop body jumps straight to the back edge (direct MIR's control flow) | — |

Removed with no replacement, because measurement showed no effect:
`copy_prop`, `dest_prop`, `mem_fwd` + `dead_store`, `strength_reduce`,
`invariant_store_elim`, `tos_carry`, the return / bin-join / multi-op
convoys, `invert_guard_branch`, `slot_promote_tell`, `ssa_gvn` and per-body
`cfg_gvn`. MIR instcombine / strength reduction / GVN are separate.

The HIR passes run in `emit_hir` after inlining: unroll, fold,
bounds, sink_return. MIR runs its own passes (CSE in `mir/cse.rs`, LICM in `mir/licm.rs`) ([mir.md](../../../../docs/internals/mir.md)).

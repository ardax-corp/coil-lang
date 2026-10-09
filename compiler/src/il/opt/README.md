# IL optimization pass contracts (D1)

Stack IL is **instruction lowering + fuse-select**, not SSA/HIR. Passes rewrite
a `Vec<IlOp>` in place. They must **not** invent a new IR. Labels stay
symbolic until [`crate::il::lower`] assigns PCs once.

This page inventories every **production** step that actually runs from
`optimize_once_at` (gated by an [`OptimizeOptions`] flag) plus the named
post-opt step that lives next to lower: **fuse-select**. Driver knobs are
listed once below and are **not** passes.

**Removed 2026-10** (measurement showed no bench effect): `copy_prop`,
`dest_prop`, `mem_fwd` + `dead_store`, `instcombine`, `strength_reduce`,
`invariant_store_elim`, `tos_carry`, `return_convoy`, `bin_join_convoy`,
`multi_op_join_convoy`, `invert_guard_branch`, `slot_promote_tell`,
`ssa_gvn`, `cfg_gvn`, and the IL `escape_analysis` pass. MIR instcombine /
strength reduction / GVN are separate and unaffected. The `escape_analysis`
option survives: it now only gates HIR enum / tuple scalar replacement
(`hir::enum_sroa`, `hir::tuple_sroa`) in `emit_hir`.

**Hit-bench prove:** if a pass is sound but flagship `.hyc` do not change, add
focused `examples/perf` benches and prove those — do not skip merge because
`mandelbrot` / `tak` / `nsieve` / `binary_trees` / `fib` are identical.
Skip only on hit-bench wash or regress. Flagships stay controls. See
[optimization-roadmap.md](../../../../docs/internals/optimization-roadmap.md#hit-bench-prove-rule).
**PGO is gone** ([#301](https://github.com/ardax-corp/coil-lang/pull/301));
branch layout is heuristic (`BranchProfile` is not a thing).

Codegen (not this pipeline): try flatten ([#307](https://github.com/ardax-corp/coil-lang/pull/307)),
sibling/self `TailCall` ([#316](https://github.com/ardax-corp/coil-lang/pull/316)).

Solo tests already exist for every production pass. D1 documents them; it does
not change pass behavior.

## Cursor facts: `sp` vs `tell`

| Analysis | Module | Quantity |
|----------|--------|----------|
| **`sp`** | [`crate::il::sp`] | Eval-stack *height*. Nested `CALL`/`MakeCoro` reset to 1 (return value). `STORE` does **not** floor height. |
| **`tell`** | [`crate::il::tell`] | Shared operand/local *cursor*. `STORE` raises the cursor to `slot + 1` even when height is lower. |

Do not substitute one for the other (COI-81). Fuse/canon/branch layout need
height; slot promotion / `dead_store_at` need the cursor. `Tell::Unknown`
at a join is often the correct answer (a raising loop header), not a gap.

Entry seed: `optimize` / `optimize_at` use `entry_sp` (usually `0` in unit
tests, `arity` on a real function). `entry_tell = entry_sp.max(0) as u32`.

## Residual `IlOp::Byte`

Hot-path ops are typed variants (`Load`, `Const`, `Bin`, `BinSlot*`, `*Return`,
`HostInvoke`, …). `IlOp::Byte` is the long-tail escape hatch (FORMAT, FFI,
`Seek`, some unaries still waiting on typed lift, packed forms, tests).

Unless a pass **decodes** a byte via `as_encode_byte()`, residual `Byte` is an
opaque barrier: unknown stack/cursor effect, no CSE, no promotion. Absolute
`JMP`/`JMPF`/`JMPT` as `Byte` is forbidden before opts/fuse
(`assert_no_residual_abs_jumps`).

## Driver knobs (not passes)

These flags do not have their own rewrite; they wrap or parameterize the
pipeline. No solo “pass” tests.

| Knob | Default | Role |
|------|---------|------|
| `collect_stats` | off | Record per-pass counters into `OptStats`. |
| `pure_call_ctx` | `None` | Sidecar-proven pure user `fn` names + entries for COI-99 length-proof / LICM barriers (`$mono$` clones match the source bind). |
| `loop_unroll_factor` | 8 | Trip cap for `loop_unroll` (clamped to 8). Parameter of that pass. |
| `escape_analysis` | on at Standard+ / Size | Gates HIR enum / tuple scalar replacement in `emit_hir`. Not an IL pass. |

## Pipeline order

`optimize_once_at` = cleanup then decision.

**Cleanup** (`cleanup_once_at`), in order:

1. `jump_thread` → 2. `dead_block` → 3. `stack_dce` → 4. `canon` →
5. `algebraic` → 6. `local_cse`

**Decision** (`decision_once_at`), in order:

7. `loop_bounds` → 8. `loop_unroll` → 9. `slot_promote`
(+ `dead_store_at`) → 10. `clone_shared_return` → 11. `branch_optimization`
→ 12. `block_reordering`

**Production** (`IlModule::optimize_and_flatten`, non-empty `funcs`): the
table runs per body, then the bodies are concatenated. Bare-buffer
`optimize()` (empty `funcs` / unit tests) runs the same table on the whole
buffer.

**After opt:** a single `lower_optimized` fuse-select + PC assign.

Invariants every pass must preserve unless its section says otherwise:

- Labels remain symbolic; jump targets still name the same (or freshly minted)
  ids.
- Net stack height at each terminator / join is unchanged (or the pass refuses).
- Residual abs-jump `Byte` is never introduced.

---

## `jump_thread`

**Flag:** `jump_thread` (default on). **Fn:** `cfg::jump_thread`.

- **Input:** Symbolic `Jump` / `Label` IL. No cursor analysis.
- **Output:** Unconditional `JMP L` whose target begins with `JMP L2` (skipping
  labels) becomes `JMP L2`. One hop per jump per round. Stack height and label
  ids unchanged.
- **Refusals:** Conditional jumps, missing label, target that is not an
  unconditional jump.
- **Tests:** `opt/convoy.tests.rs` `jump_thread_collapses_goto_goto` (calls the
  pass directly). Chain convergence: `opt/mod.tests.rs`
  `jmp_chain_needs_two_rounds_to_thread_to_the_return`.

## `dead_block`

**Flag:** `dead_block` (default on). **Fn:** `cfg::eliminate_dead_blocks`.

- **Input:** Same labeled IL. Linear sweep; labels re-open reachability.
- **Output:** Drops ops after unconditional `JMP` / `RETURN` / `HALT` /
  `*Return` until the next `Label`. Fall-through height at live labels is
  unchanged because dead ops never executed.
- **Refusals:** Does not delete labeled ops (even if the label is unused).
  `CALL` continuations must be labeled so they are not treated as
  fall-through-after-terminator.
- **Tests:** `opt/convoy.tests.rs` `dead_block_drops_after_unconditional_jmp`,
  `dead_block_drops_after_return_until_label`.

## `stack_dce`

**Flag:** `stack_dce` (default on). **Fn:** `dce::stack_dce` (fixpoint of
`stack_dce_once`).

- **Input:** Straight-line adjacent pairs. No `sp`/`tell` required.
- **Output:** Drops `Dup; Pop`, `Load s; StorePop s`, pure producer + `Pop`
  (`Const`/`ConstPool`/`String`/`Load`), `MakeEnum; Pop` (replaced by `arity`
  pops), unary-enum `LoadField 0` / `Unpack` unwrap, constructor+`JumpIfMatch`
  of the same tag → unconditional jump. Residual `Byte` DUP/POP and LOAD/STORE
  same-slot pairs also drop. Net height of the remaining stream is preserved
  (pairs are height-neutral).
- **Refusals:** Different slots, non-droppable producers, intervening ops,
  typed forms that are not the listed pairs.
- **Tests:** `opt/convoy.tests.rs` `stack_dce_removes_dup_pop`,
  `stack_dce_removes_typed_dup_pop`.

## `canon`

**Flag:** `canon` (default on). **Fn:** `il::canon::canonicalize_operand_order`.
Uses **`sp`**.

- **Input:** `Const; Load; op` (any SP), demote-able `ConstPool; Load;
  int-op`, or Known-SP `Load a; Load b; op` with `a > b`.
- **Output:** Const on RHS; low-then-high load order; ordered-cmp polarity flip
  (`LE`↔`GT`, `LEQ`↔`GEQ`). Int `ConstPool` may demote to inline `Const`. Stack
  height and labels unchanged.
- **Refusals:** Unknown SP on `Load; Load; op` only (counted in
  `CanonStats::refused_unknown_sp`); float ops; residual `Byte`; non-commutative
  `SUB`/`DIV`/`MOD`/`SHL`/`SHR`/`Pow`. No float reassoc. `Const; Load; op` is
  stack-relative and does not consult SP (COI-384).
- **Tests:** `il/canon.rs` `const_load_add_swaps_to_load_const_add`,
  `const_load_add_swaps_after_unknown_sp`, `load_load_unknown_sp_still_refused`,
  `const_load_sub_refused`.

## `algebraic`

**Flag:** `algebraic` (default on; **only** pass at `-O0`). **Fn:**
`il::algebraic::algebraic_simplify`. Uses **`sp`**. Needs the const `pool` for
float identities / pool fold.

- **Input:** Known-SP typed windows (`Const`/`ConstPool`/`Load`/`Bin`/`BinSlot*`,
  `LogNot` pairs, …).
- **Output:** Strength peeps (`x+0`, `x*1`, `x*0`, `x-x`, `x&-1`, float `+0.0`/
  `+1.0` exact bits, `pow 2` → `Dup; MUL`, const-fold of scalar int/float bins
  that encode as inline `CONST` or a new pool entry). Height of the rewritten
  window matches the original.
- **Refusals:** Unknown SP-in mid-window; residual `Byte` (not matched); `DIV`/
  `MOD`/`DIVF`/`MODF` by zero; negative int fold (bit 31 is `POOL_FLAG`); float
  NaN / −0.0 identities; host/calls are not folded (they are not these windows).
- **Tests:** `il/algebraic.rs` `add_zero_folds_to_load`, `refuses_when_sp_unknown`.
  Isolated flag: `float_const_pool_add_via_optimize_pipeline`.

## `local_cse`

**Flag:** `local_cse` (default on at Standard). **Fn:**
`opt::early_cse::early_cse`. Cleanup, after `algebraic`.

- **Input:** One basic block at a time (`analysis::build_blocks` leaders). Available map of
  pure expressions whose result was stored (`BinSlot*`, stack `Bin` of loads,
  `CastIntToFloat`, `ArrayLen`, `Index` / `IndexPin*`, `LoadField`).
- **Output:** Second identical compute → `Load` of the slot that still holds
  the first result. Height of each rewrite matches the original window.
- **Refusals:** `DIV`/`MOD`/`DIVF`/`MODF`; store to an operand or to the
  holding slot; `StoreIndex` / `ArrayPush` kill memory exprs; `HostInvoke` /
  `CALL` / residual effectful `Byte` / jumps clear the map. Does not cross
  labels. Does not replace cheap `Const`/`Load` with a slot load (fuse).
- **Tests:** `opt/early_cse.rs` `binslot_store_reused_as_load`,
  `store_to_operand_kills_expr`, `host_invoke_is_barrier`,
  `does_not_cross_basic_block`. Isolated flag:
  `isolated_optimize_flag_runs_pass`. Hit benches: `examples/perf/cse_index_recompute.hy`,
  `cse_cast_recompute.hy` ([#317](https://github.com/ardax-corp/coil-lang/pull/317)).

## `licm` (moved to the HIR)

Loop-invariant code motion runs on the HIR (`hir::licm`, after inlining and
scalar replacement in `emit_hir`), still under the `licm` flag. The stack-IL
pass was removed 2026-10; the invariant `len(a)` hoist the bounds proofs need
(`bounds::hoist_loop_invariants`) now runs at the start of `loop_bounds`.
Hit benches: `examples/perf/licm_nested_chains.hy`, `tail_sibling.hy`.

## `loop_bounds`

**Flag:** `loop_bounds` (default on). **Fn:** `il::bounds::loop_bounds` (after `bounds::hoist_loop_invariants`). Uses
**`sp`**. Reads `pure_call_ctx` for length-proof barriers.

- **Input:** Counted / `0..len` natural loops with an invariant array.
- **Output:** Hoists `LOAD a; ArrayLen; STORE t` (and fill-loop `CONST; STORE`)
  to the preheader (store floors `tell` at `t+1`). Proven unit-stride or
  invariant-stride `Index` / `StoreIndex` rewrite to `*Unchecked` / pin forms.
  Unproven sites stay checked. Height unchanged except for the hoisted triple.
- **Refusals:** `ArrayPush` / `MakeArray` / impure call / host / FFI in the loop
  (length not invariant); `LEQ`/`GEQ` headers are not proofs (`LE`/`GT` only);
  pure user helpers on `b[i]` are not a length barrier; unknown paths stay
  checked. Residual `Byte` `StoreIndex` is rewritten only when decoded.
- **Tests:** `il/bounds.rs` `hoists_array_len_out_of_counted_loop` (calls
  `loop_bounds` directly), plus refuse tests for push / make-array.
  Sidecar length/index facts (`typechecking/index_facts.rs`) feed codegen
  `IndexUnchecked` / `ArrayPin` for helpers, for-in, and stride; pipeline
  tests in `compiler/src/pipeline.rs`.

## `loop_unroll`

**Flag:** `loop_unroll` (default on; off at `-Os`). **Fn:**
`loop_unroll::unroll_loops`. Honors `loop_unroll_factor`.

- **Input:** Innermost counted natural loop, induction from 0 step +1, trip
  count ≤ `min(factor, 8)`, header `LE`/`LEQ`/`GT` + `JMPF`.
- **Output:** Body cloned `trips` times; header/latch dropped. Inner labels
  reminted. Straight-line height is the sequential composition of the original
  body.
- **Refusals:** Nested loops; `Entry` / `HostInvoke` / `Print` / residual
  CALL/FFI/FORMAT/`TailCall`; `break` / extra exits / foreign jumps into the
  header; trip 0 or > 8; non-zero induction init; bound stored in the loop.
- **Tests:** `opt/loop_unroll.tests.rs` `unrolls_simple_const_bound_while`,
  `call_disables_unroll`, `break_disables_unroll`, `nested_loops_are_not_unrolled`.

## `slot_promote`

**Flag:** `slot_promote` (default on). **Fn:** `slot_promote::slot_promote`.
Uses **`tell`**. Cleanup `dead_store_at` runs immediately after.

- **Input:** Straight-line and same-def-join aliases (`LOAD a; STORE b`),
  tell-safe producer bindings, store-destination coalescing, copy-only latch
  shuffles.
- **Output:** Rewrites later `LOAD` / `BinSlot*` uses to the source; elides
  unused alias stores when tell or a higher store covers the floor. Peel param
  copies may raise the producer into a dead high slot then elide. Labels
  unchanged.
- **Refusals:** Unknown tell; `CALL`/host without a raise proof; residual
  `Byte` between copy-shuffle ops; overlapping live ranges (mandelbrot
  `tr`/`zr`); multi-pred φ merges; address-taken / aggregate promotion.
- **Tests:** `opt/slot_promote.rs` `forwards_alias_load_through_store_load`,
  `rewrites_bin_slot_through_alias`,   `same_def_join_forwards_alias_across_diamond`.

## `clone_shared_return`

**Flag:** `clone_shared_return` (default on; off at `-Os`). **Fn:**
`convoy::clone_shared_return`.

- **Input:** Return-label cluster targeted by jump-only unconditional preds
  *and* a fall-through (or other) producer arm.
- **Output:** Replaces those `JMP`s with a cloned `RETURN`. If the cluster then
  has no jump preds, fuses a lone fall-through `CONST`/`LOAD` into `*Return`.
  Each arm’s height at return is unchanged (the jump-only arm already had the
  value on stack).
- **Refusals:** No jump-only preds; not a mixed join (jump-only only).
- **Tests:** `opt/convoy.tests.rs`
  `clone_shared_return_fuses_const_arm_after_jump_only_clone`.

## `branch_optimization`

**Flag:** `branch_optimization` (default on). **Fn:**
`branch_opt::optimize_branches_at`. Uses **`sp`**. Last among IL consumers
except block reorder / seek. Heuristic only (no profile).

- **Input:** `JMPF`/`JMPT` whose fall-through is a terminating then-arm
  (no internal jumps/labels) with Known SP at the jump and along the moved arm.
- **Output:** Invert polarity and move the cold arm after a freshly minted
  module-wide-unique label. Semantics identical; layout only.
- **Refusals:** Unknown SP / empty stack at the cond; then-arm with an internal
  jump or label; suffix that could fall into the moved region;
  `ValueUnderJmp` / `nofuse` pair-`?` tag jumps (cold invert would turn the
  shared fail `RETURN` into a join).
- **Tests:** `opt/branch_opt.rs` `heuristic_moves_return_off_jmpf_fallthrough`,
  `value_under_jmp_try_refuses_cold_invert`,
  `refuses_when_cond_jump_has_empty_stack`.

## `block_reordering`

**Flag:** `block_reordering` (default on). **Fn:**
`block_order::reorder_basic_blocks`.

- **Input:** Basic blocks split on labels and terminators.
- **Output:** Detached jump-only terminating blocks sink to the end. Fall-through
  chains stay adjacent. Label ids and branch polarity **are not rewritten**.
- **Refusals:** Fall-through successor; block that is not a terminator; back-edge
  successor; unconditional-jump join target.
- **Tests:** `opt/block_order.rs` `cold_return_block_moves_past_join`,
  `linear_code_unchanged`, `branch_targets_keep_the_same_label_ids`.

---

## fuse-select (D4, named pass in `lower.rs`, not in `opt/` driver)

**Fn:** `il::lower::fuse_select` called from `lower_optimized`. Runs **once**
after concat. Not gated by `OptimizeOptions`. Not a second lowering: PC assign
and encode stay in `lower_optimized`. No post-lower `adjust_target`.

- **Input:** Post-opt **typed** [`IlOp`](../op.rs). `Jump`/`Entry` stay symbolic.
  Incoming [`Label`] / [`JoinLabel`] binds and `FuseHint` / `JoinClass` (D3) are
  hard barriers — no dummy `NOOP` / `DUP;POP`. Residual [`IlOp::Byte`] is the
  **cold set** (`FORMAT`, FFI, packed multi-slot LOAD/STORE, unmatched
  `from_plain_byte`) and is **refused** in any multi-op window.
- **Output:** Superinstructions (const fold, `BinSlotImm`/`BinSlotSlot`,
  `*Jmpf`/`*Jmpt`, `*Store`, packed LOAD/STORE n≤3, `*Return`).
  `FloatChainStore` / `BinSlotSlotConstJmpf` are tombstones (not selected). Then one PC
  assignment. `Vec<Byte>` for the archive. Label ids map to PCs; they do not
  survive as IL.
- **Refusals:** Window that would pull a **label** or **abs-jump target** onto a
  non-first op; window that contains residual **`Byte`**; `*Return` fusion when
  window[0] is an **unconditional join** (stacked arm value must be popped).
  **`Entry` CALL / TailCall** is never a fuse window member — LICM / bounds
  may see through that `CALL` only when [`PureCallCtx`](../pure_call.rs)
  (sidecar purity) proves the callee; impure `CALL` is a hoist barrier.
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

---

## Solo-test map

Every production `OptimizeOptions` pass has at least one test that either
calls the pass function directly or runs `optimize` with only that flag true.

| Pass | Solo test already existed | Newly added in D1 |
|------|---------------------------|-------------------|
| jump_thread | `convoy.tests.rs` | no |
| dead_block | `convoy.tests.rs` | no |
| stack_dce | `convoy.tests.rs` | no |
| canon | `canon.rs` | no |
| algebraic | `algebraic.rs` | no |
| local_cse | `early_cse.rs` | yes |
| loop_bounds | `bounds.rs` | no |
| loop_unroll | `loop_unroll.tests.rs` | no |
| slot_promote | `slot_promote.rs` | no |
| clone_shared_return | `convoy.tests.rs` | no |
| branch_optimization | `branch_opt.rs` | no |
| block_reordering | `block_order.rs` | no |
| fuse-select (D4) | `lower.rs` | no |

Run (from repo root):

```bash
cargo test -p compiler --lib -- il::
```

# IL optimization pass contracts (D1)

Stack IL is **instruction lowering + fuse-select**, not SSA/HIR. Passes rewrite
a `Vec<IlOp>` in place. They must **not** invent a new IR. Labels stay
symbolic until [`crate::il::lower`] assigns PCs once.

This page inventories every **production** step that actually runs from
`optimize_once_at` (gated by an [`OptimizeOptions`] flag) plus the two named
post-opt steps that live next to lower / module flatten: **`cfg_gvn`** and
**fuse-select**. Driver knobs are listed once below and are **not** passes.

Solo tests already exist for every production pass. D1 documents them; it does
not change pass behavior.

## Cursor facts: `sp` vs `tell`

| Analysis | Module | Quantity |
|----------|--------|----------|
| **`sp`** | [`crate::il::sp`] | Eval-stack *height*. Nested `CALL`/`MakeCoro` reset to 1 (return value). `STORE` does **not** floor height. |
| **`tell`** | [`crate::il::tell`] | Shared operand/local *cursor*. `STORE` raises the cursor to `slot + 1` even when height is lower. |

Do not substitute one for the other (COI-81). Fuse/canon/convoy/mem_fwd need
height; slot promotion / dead-store / copy-prop need the cursor. `Tell::Unknown`
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
| `iterative_optimization` | off | Re-run `optimize_once_at` until a no-op round or the cap. |
| `max_optimization_iterations` | 10 | Cap, clamped to `1..=10`. |
| `collect_stats` | off | Record per-pass counters into `OptStats`. |
| `pure_call_ctx` | `None` | Pure user `fn` names + entries for COI-99 length-proof barriers. |
| `pgo_prioritize_hot_loops` | on | Heat-order LICM / unroll / escape when a PGO profile is loaded. |
| `loop_unroll_factor` | 8 | Trip cap for `loop_unroll` (clamped to 8). Parameter of that pass. |

PGO instrument compile (`pgo_instrumenting`) runs **cleanup only** and skips
decision opts + `cfg_gvn`.

## Pipeline order

`optimize_once_at` = cleanup (profile-agnostic) then decision (layout/heat).

**Cleanup** (`cleanup_once_at`), in order:

1. `jump_thread` → 2. `dead_block` → 3. `stack_dce` → 4. `mem_fwd` →
5. `copy_prop` → 6. `dead_store` (same flag as `mem_fwd`) → 7. `canon` →
8. `algebraic` → 9. `cast_spill`

**Decision** (`decision_once_at`), in order:

10. `licm` → 11. `loop_bounds` → 12. `loop_unroll` → 13. `invariant_store_elim`
→ 14. `ssa_gvn` → 15. `escape_analysis` → 16. `slot_promote` (+ `dead_store`)
→ 17. `clone_shared_return` → 18. `return_convoy` → 19. `bin_join_convoy` →
20. `multi_op_join_convoy` → 21. `invert_guard_branch` →
22. `branch_optimization` → 23. `block_reordering` → 24. `seek_back_edge` →
25. `slot_promote_tell`

**Production** (`IlModule::optimize_and_flatten`, non-empty `funcs`): per-body
opts run with `multi_op_join_convoy`, `invert_guard_branch`, `seek_back_edge`,
and `slot_promote_tell` **deferred**. Then per-body **`cfg_gvn`**, then seek +
slot_promote_tell, then concat, then whole-buffer multi_op + invert. Bare-buffer
`optimize()` (empty `funcs` / unit tests) does **not** run `cfg_gvn`.

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

## `mem_fwd`

**Flag:** `mem_fwd` (default on). **Fn:** `dce::mem_fwd`. Uses **`sp`**.

- **Input:** Adjacent `StorePop s; Load s`. SP-in at the store must be Known
  and `h > s + 1` (TOS after store is not the stored slot).
- **Output:** `Dup; StorePop s`. Height unchanged (`StorePop; Load` and
  `Dup; StorePop` are both net 0).
- **Refusals:** Unknown SP; `h <= s + 1` (shared-stack / post-CALL return
  height); load that feeds `Index`; mismatched slots. Residual `Byte` store/load
  is not rewritten here (typed only).
- **Tests:** `opt/convoy.tests.rs` `mem_fwd_store_pop_load_becomes_dup_store`,
  `mem_fwd_refuses_when_load_feeds_index`.

### Companion: `dead_store` (same flag)

Not an `OptimizeOptions` field. Gated by `mem_fwd` in cleanup; run again after
`slot_promote`. **Fn:** `dce::dead_store_at`. Uses **`tell`**.

- **Input:** `StorePop s` whose slot is unread to the next barrier, with a
  cursor proof that dropping the store does not lower a later floor.
- **Output:** Removes the store (and a feeding `Dup` when the slot is unused).
- **Refusals:** Unknown cursor; opaque `Byte` / host / call / FFI; slot still
  loaded or used by `BinSlot*`; loop-carried store before a jump when a later
  load exists.
- **Tests:** `opt/convoy.tests.rs` `dead_store_drops_dup_store_when_slot_unused`,
  `dead_store_keeps_store_before_opaque_byte_barrier`.

## `copy_prop`

**Flag:** `copy_prop` (default on). **Fn:** `dce::copy_prop`. Uses **`tell`**.

- **Input:** Straight-line `producer; StorePop s` then later `Load s` with Known
  tell-in. Producers: `Const` / `ConstPool` / `String` / `Load` / `BinSlotImm` /
  `BinSlotSlot`.
- **Output:** Replaces the `Load` with a clone of the producer. Does not itself
  drop the store (that is `dead_store`). Height at each remaining op is
  unchanged (clone has the same delta as `Load`).
- **Refusals:** Unknown tell; labels / jumps / `Entry` / `HostInvoke` / `Print`
  / fields / `Make*` / box / returns / residual `Byte` (binding map cleared);
  shape-sensitive `Load` before `GetField` or `MakeArray`/`MakeTuple`/`MakeEnum`;
  store that aliases a producer dependency; self-alias `Load s; StorePop s`.
- **Tests:** `opt/convoy.tests.rs` `copy_prop_replaces_load_and_cursor_safe_dead_store`,
  `copy_prop_refuses_control_flow_boundaries`, `copy_prop_clears_bindings_across_host_invoke`.

## `canon`

**Flag:** `canon` (default on). **Fn:** `il::canon::canonicalize_operand_order`.
Uses **`sp`**.

- **Input:** Known-SP windows `Const; Load; op`, demote-able `ConstPool; Load;
  int-op`, or `Load a; Load b; op` with `a > b`.
- **Output:** Const on RHS; low-then-high load order; ordered-cmp polarity flip
  (`LE`↔`GT`, `LEQ`↔`GEQ`). Int `ConstPool` may demote to inline `Const`. Stack
  height and labels unchanged.
- **Refusals:** Unknown SP (counted in `CanonStats::refused_unknown_sp`); float
  ops; residual `Byte`; non-commutative `SUB`/`DIV`/`MOD`/`SHL`/`SHR`/`Pow`. No
  float reassoc.
- **Tests:** `il/canon.rs` `const_load_add_swaps_to_load_const_add`,
  `unknown_sp_refused`, `const_load_sub_refused`.

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

## `cast_spill`

**Flag:** `cast_spill` (default on). **Fn:**
`il::cast_spill::spill_cast_before_float_chain`.

- **Input:** `CastIntToFloat` inside a float-arith → `StorePop` window
  (mandelbrot `CONST; LOAD; Cast; …; STORE`).
- **Output:** Hoists `LOAD; Cast` into a prefix `LOAD; Cast; STORE t` and rewrites
  the body to `LOAD t` so fuse-select can match `LOAD; CONST` / const-under
  `FloatChainStore`. New temps are fresh high slots. Labels unchanged; extra
  stores raise `tell` on purpose.
- **Refusals:** No float-chain-store after the cast; jump interrupting the
  window; already-hoisted `Cast; STORE`. Residual `Byte` casts are recognized
  via `as_encode_byte` (`CastIntToFloat`).
- **Tests:** `il/cast_spill.rs` `spills_cast_inside_float_arith_store_window`,
  `refuses_cast_without_float_chain_store_window`. Isolated off:
  `optimize_with_cast_spill_disabled_keeps_inline_cast`.

## `licm`

**Flag:** `licm` (default on). **Fn:** `il::licm::licm`. Uses **`sp`**. Honors
`pgo_prioritize_hot_loops`.

- **Input:** Natural loops (back-edge JMP to a header label) whose header SP is
  Known. Also runs `bounds::hoist_loop_invariants` first.
- **Output:** Hoists invariant `Const`/`Load`, `BinSlot*`, tuple/array/dict
  construction, non-trapping int arith, FORMAT concat, `len`, and invariant
  float chains into a preheader (may mint a preheader and retarget the external
  entry). Sinks table-indexed `STRING` field keys. CSE of `LOAD; CastIntToFloat`.
  Loop body height at the header/latch is preserved (moved ops are height-neutral
  or rewritten to loads of the hoisted temp).
- **Refusals:** Unknown SP; `HostInvoke` in the loop; `JumpIfMatch` in the loop;
  `Load` of a slot stored in the loop; load still needed as a stack producer;
  effectful / residual `Byte` that is not a recognized hoist form.
- **Tests:** `il/licm.rs` `hoists_const_out_of_while_shaped_loop`,
  `refuses_when_host_invoke_in_loop`, `refuses_when_jump_if_match_in_loop`.

## `loop_bounds`

**Flag:** `loop_bounds` (default on). **Fn:** `il::bounds::loop_bounds`. Uses
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

## `loop_unroll`

**Flag:** `loop_unroll` (default on; off at `-Os`). **Fn:**
`loop_unroll::unroll_loops_pgo`. Honors `loop_unroll_factor` and
`pgo_prioritize_hot_loops`.

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

## `invariant_store_elim`

**Flag:** `invariant_store_elim` (default on). **Fn:**
`invariant_store_elim::eliminate_invariant_stores`.

- **Input:** Natural loop containing `producer; StorePop s` where the producer
  is loop-invariant and `s` is not loaded in the body.
- **Output:** Drop the pair if `s` is never loaded anywhere; otherwise sink it
  to the unique forward exit label. Loop-carried height at the header is
  unchanged (the store no longer runs per trip).
- **Refusals:** Variant producer; slot loaded in the loop; extra exits when the
  store is live after the loop (no unique sink); no structured exit and the
  slot is loaded later. Residual `Byte` stores are not matched (`StorePop`
  only).
- **Tests:** `opt/invariant_store_elim.tests.rs`
  `eliminates_unused_invariant_store`, `sinks_live_invariant_store_out_of_loop`,
  `keeps_variant_store`. Isolated pipeline:
  `optimize_pipeline_eliminates_unused_invariant_store`.

## `ssa_gvn`

**Flag:** `ssa_gvn` (default on). **Fn:** `il::gvn_ssa::ssa_gvn`. Also invoked
from `cfg_gvn_with` when the flag is on.

- **Input:** Stack IL with labels. Virtual `Phi(block, slot)` VNs at joins
  (no φ opcode, no slot rename).
- **Output:** Redundant pure binop whose result already lives in a slot becomes
  `Load`. Height: `Bin` (−1) is replaced by `Load` (+1) only when the original
  operands are already consumed / the value is in a slot — the rewrite is the
  CSE of a recompute, so join height matches the stored value.
- **Refusals:** `DIV`/`MOD`/`DIVF`/`MODF`; pred disagreement on a slot (φ);
  effectful ops. Residual `Byte` is not numbered as a binop.
- **Tests:** `il/gvn.tests.rs` `ssa_gvn_cse_across_basic_blocks`,
  `ssa_gvn_preserves_div`, `ssa_gvn_skips_join_when_operand_phi_disagrees`.

## `escape_analysis`

**Flag:** `escape_analysis` (default on). **Fn:**
`escape_analysis::escape_analysis_pgo`. Honors `pgo_prioritize_hot_loops`.

- **Input:** `MakeArray { arity: 1..=32 }; StorePop s` whose elements are
  immediates (`Const` / pool / string).
- **Output:** Explodes the array into consecutive high frame slots; rewrites
  local `Index` / `len` / `StoreIndex` of that local. Heap object gone. Slot
  ids for other locals unchanged; new slots are GC roots.
- **Refusals:** Return / call / `HostInvoke` / field store / `ArrayPush` /
  computed elements / second store to `s` / opaque use / residual `Byte` use
  that is not a local element op; arity 0 or > 32; frame would exceed slot 256.
  Not named-local class SROA.
- **Tests:** `opt/escape_analysis.tests.rs` `scalarizes_non_escaping_index`,
  `keeps_heap_when_array_is_returned`, `keeps_heap_when_passed_to_call`.
  Isolated flag: `isolated_optimize_flag_runs_pass`.

## `slot_promote`

**Flag:** `slot_promote` (default on). **Fn:** `slot_promote::slot_promote`.
Uses **`tell`**. Cleanup `dead_store_at` runs immediately after.

- **Input:** Straight-line and same-def-join aliases (`LOAD a; STORE b`),
  tell-safe producer bindings, store-destination coalescing, copy-only latch
  shuffles.
- **Output:** Rewrites later `LOAD` / `BinSlot*` uses to the source; elides
  unused alias stores when tell or a higher store covers the floor. Peel param
  copies may raise the producer into a dead high slot then elide. Labels
  unchanged. TOS-at-`t+1` `STORE t` is *not* this pass — that is
  `slot_promote_tell`.
- **Refusals:** Unknown tell; `CALL`/host without a raise proof; residual
  `Byte` between copy-shuffle ops; overlapping live ranges (mandelbrot
  `tr`/`zr`); multi-pred φ merges; address-taken / aggregate promotion.
- **Tests:** `opt/slot_promote.rs` `forwards_alias_load_through_store_load`,
  `rewrites_bin_slot_through_alias`, `same_def_join_forwards_alias_across_diamond`.

## `clone_shared_return`

**Flag:** `clone_shared_return` (default on; off at `-Os`). **Fn:**
`convoy::clone_shared_return`.

- **Input:** Return-label cluster targeted by jump-only unconditional preds
  *and* a fall-through (or other) producer arm.
- **Output:** Replaces those `JMP`s with a cloned `RETURN`. If the cluster then
  has no jump preds, fuses a lone fall-through `CONST`/`LOAD` into `*Return`.
  Each arm’s height at return is unchanged (the jump-only arm already had the
  value on stack).
- **Refusals:** No jump-only preds; not a mixed join (jump-only only). Convoy
  mixed-class joins stay refused until this clone runs.
- **Tests:** `opt/convoy.tests.rs`
  `clone_shared_return_fuses_const_arm_after_jump_only_clone`.

## `return_convoy`

**Flag:** `return_convoy` (default on). **Fn:** `convoy::return_convoy`. Uses
**`sp`** at the join.

- **Input:** Identical immediate `LOAD s` (`s ≤ 255`) or inline `CONST` on every
  pred of a return-label cluster (`JMP`, or all-`JMPF`/`JMPT` with value under
  the cond, or all-`JumpIfMatch`).
- **Output:** Sink the producer and fuse `LoadReturnSlot` / `ConstReturnImm`.
  Join height becomes the terminator’s.
- **Refusals:** Disagreeing consts/slots; mixed jump classes; jump-only arm
  without a producer; Unknown join SP on cond/match/jump-only preds; pool
  `CONST`; `LOAD` slot > 255; residual `Byte` that is not a return producer.
- **Tests:** `opt/convoy.tests.rs` `return_convoy_fuses_agreeing_const_join`,
  `return_convoy_skips_disagreeing_consts`,
  `return_convoy_skips_jump_if_match_unknown_join_sp`.

## `bin_join_convoy`

**Flag:** `bin_join_convoy` (default on). **Fn:** `convoy::bin_join_convoy`.
Uses **`sp`**.

- **Input:** Identical plain binop or `BinSlot*` tail on every pred of a return
  cluster (same pred/SP gates as return convoy).
- **Output:** `BinReturn`, or one shared `BinSlot*` immediately before `RETURN`.
- **Refusals:** Disagreeing ops; mixed jump classes; Unknown join SP on
  jump-pred-only templates; conditional jump into the cluster when SP-in is not
  proven. Residual `Byte` tails are accepted when `as_encode_byte` is a listed
  binop / `BinSlot*`.
- **Tests:** `opt/convoy.tests.rs` `bin_join_convoy_fuses_agreeing_binop_to_bin_return`,
  `bin_join_convoy_skips_disagreeing_binops`.

## `multi_op_join_convoy`

**Flag:** `multi_op_join_convoy` (default on). **Fn:**
`convoy::multi_op_join_convoy`. Uses **`sp`**. Production runs this on the
**concatenated** module (scoped run can mis-sink `JMPF`/fall-through diamonds).

- **Input:** Identical 2..=4-op suffixes at return or non-return joins.
  Preds: `JMP` / `JMPF` / `JMPT` / `JumpIfMatch`.
- **Output:** Suffix sunk once after the join labels. Preds lose the suffix;
  join height matches the sunk ops.
- **Refusals:** Disagreeing suffixes; Unknown SP (including mixed
  `JMPF`+`JMP` without a known join height); `EQ`-fed `JMPF` suffix (keeps the
  compare on the pred); residual `Byte` that is not a suffix op (unary
  NOT/NEG/NEGF `Byte` *is* allowed as compute).
- **Tests:** `opt/convoy.tests.rs` `multi_op_join_convoy_sinks_identical_suffix`,
  `multi_op_join_convoy_skips_jmpf_fallthrough_unknown_sp`.

## `invert_guard_branch`

**Flag:** `invert_guard_branch` (default on). **Fn:**
`cfg::invert_branch_over_jump` (re-exported as `invert_guard_branch`).
Production runs this on the concatenated buffer **after** multi_op.

- **Input:** `JMPF A; JMP B; A:` (labels may cluster). No cursor.
- **Output:** `JMPT B`, dropping the trailing `JMP`. Fusable guards invert too
  (`*Jmpt` twins at fuse-select, COI-87). Height: both shapes pop the cond.
- **Refusals:** False target not bound at the next real instruction; not the
  `JMPF; JMP` pair.
- **Tests:** `opt/convoy.tests.rs` `inverts_guard_branch_over_unconditional_jump`,
  `refuses_guard_inversion_when_false_target_is_not_next`.

## `branch_optimization`

**Flag:** `branch_optimization` (default on). **Fn:**
`branch_opt::optimize_branches_at`. Uses **`sp`**. Last among IL consumers
except block reorder / seek / tell-promote. Optional `BranchProfile`.

- **Input:** `JMPF`/`JMPT` whose fall-through is a terminating then-arm
  (no internal jumps/labels) with Known SP at the jump and along the moved arm.
- **Output:** Invert polarity and move the cold arm after a freshly minted
  module-wide-unique label. Semantics identical; layout only.
- **Refusals:** Unknown SP / empty stack at the cond; then-arm with an internal
  jump or label; suffix that could fall into the moved region; profile saying
  the fall-through is hot (`not_taken >= taken`).
- **Tests:** `opt/branch_opt.rs` `heuristic_moves_return_off_jmpf_fallthrough`,
  `refuses_when_cond_jump_has_empty_stack`, `profile_hot_fallthrough_keeps_layout`.

## `block_reordering`

**Flag:** `block_reordering` (default on). **Fn:**
`block_order::reorder_basic_blocks`. Optional `BranchProfile`.

- **Input:** Basic blocks split on labels and terminators.
- **Output:** Detached jump-only terminating blocks sink to the end. Fall-through
  chains stay adjacent. Label ids and branch polarity **are not rewritten**.
- **Refusals:** Fall-through successor; block that is not a terminator; back-edge
  successor; unconditional-jump join target; profile-hot entry.
- **Tests:** `opt/block_order.rs` `cold_return_block_moves_past_join`,
  `linear_code_unchanged`, `branch_targets_keep_the_same_label_ids`.

## `seek_back_edge`

**Flag:** `seek_back_edge` (default **off**; on at `-O3` Aggressive only).
**Fn:** `slot_promote::seek_normalize_back_edges`. Uses **`tell`**.

- **Input:** Innermost natural loop whose forward-edge cursor is Known and whose
  body has self-stores the header join currently hides (COI-97).
- **Output:** Inserts `Seek` (residual `Byte`) at the latch to the forward-edge
  cursor so the header becomes Known; later `slot_promote_tell` can drop in-loop
  self-stores. `Seek` is `tell::Set` and does not change eval-stack height.
- **Refusals:** Outer (non-innermost) loops — outer Seek splits
  `FloatChainStore` (mandelbrot `cr`); no profitable self-store; latch already
  has `Seek`. Off on Standard because innermost mandelbrot has no such stores.
- **Tests:** `opt/slot_promote.rs` `seek_on_back_edge_elides_loop_self_store`,
  `optimize_at_default_does_not_seek_normalize`,
  `optimize_at_seek_back_edge_elides_raising_loop_store`.

## `slot_promote_tell`

**Flag:** `slot_promote_tell` (default on). **Fn:** `slot_promote::slot_promote_at`.
Uses **`tell`**. Runs last after every slot-tracking pass; production runs it
**after** `cfg_gvn`.

- **Input:** Known-cursor `LOAD` of a slot that already *is* TOS (`tell == slot
  + 1`), and `STORE t` reached with the cursor at `t + 1` (TOS already is slot
  `t`), including TailCall reload runs and (after Seek) in-loop self-stores.
- **Output:** Drops those `LOAD`/`STORE` words when **every** remaining
  reference to the slot is also dropped. Packed `LOAD` of n≤3 is dropped as one
  word. Height: a redundant TOS load is +1 that must not happen — dropping it
  keeps TOS as the local.
- **Refusals:** Unknown cursor (whole body refused if any slot operand is
  unresolvable); surviving reader of a self-store; CALL reload run; return
  reload; store nobody reads (left to `dead_store`).
- **Tests:** `opt/slot_promote.rs` `tail_call_argument_temps_leave_the_frame`,
  `unknown_cursor_refuses_the_promotion`, `self_store_stays_when_a_reader_survives`.

---

## `cfg_gvn` (production, not an `OptimizeOptions` flag)

**Fn:** `il::gvn::cfg_gvn_with(ops, ssa_gvn_flag)`. Always run per body in
`IlModule::optimize_and_flatten` after per-body opts (skipped while PGO
instrumenting). Inner `ssa_gvn` follows the `ssa_gvn` flag.

- **Input:** One function body. Intra-block + join-sink CSE of pure producers
  (`Const`/`Load`/`Bin`/`BinSlot*`/`Index`/`LoadField`/`Dup`). Join sink
  requires agreeing pred tails and agreeing SP-in.
- **Output:** Second identical producer → `Dup`; join-sunk redundant tail.
  `Load; Dup` is re-expanded to `Load; Load` so fuse-select still sees both
  binop operands (COI-82). No slot rename. Height preserved (`Dup` vs second
  `Const`/`Load` is the same +1).
- **Refusals:** `StorePop`, calls, `HostInvoke`, `SetField`, `Make*`, box,
  residual effectful `Byte` — barriers. Does not replace convoy refuse rules.
- **Tests:** `il/gvn.rs` `within_block_dup_replaces_second_identical_const`
  (calls `cfg_gvn` directly). Dup-expand: `expands_dup_after_load_so_binop_can_fuse`.

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
  `*Jmpf`/`*Jmpt`, `*Store`, packed LOAD/STORE n≤3, `*Return`,
  `FloatChainStore` up to 3 stages, `BinSlotSlotConstJmpf`, …) then one PC
  assignment. `Vec<Byte>` for the archive. Label ids map to PCs; they do not
  survive as IL.
- **Refusals:** Window that would pull a **label** or **abs-jump target** onto a
  non-first op; window that contains residual **`Byte`**; `*Return` fusion when
  window[0] is an **unconditional join** (stacked arm value must be popped).
  **`FuseHint`** on the cond-jump (`nofuse` / `ValueUnderJmp`) refuses
  `*Jmpf`/`*Jmpt` fusion (pair-`?` / pair-match keep `EQ;JMPF`). A
  **`JoinLabel`** bind is a value join: same window-break as a label, including
  `CONST;RETURN`. Residual abs JMP as `Byte` panics. Per-function fuse-select
  is not production (measured no win).
- **Tests:** `il/lower.rs` `lower_fuses_bin_slot_slot`, `lower_fuses_bin_slot_imm`,
  `lower_fuses_const_return_imm`, `lower_fuses_load_const_add_store_to_bin_slot_imm_store`,
  `lower_fuses_two_stage_float_chain_store`, `lower_refuses_cmp_jmpf_when_jump_is_nofuse`,
  `lower_refuses_const_return_across_value_join`,
  `fuse_select_refuses_residual_byte_in_window`. Cast-spill → fuse:
  `cast_spill_feeds_float_chain_store`. Invert-guard: `opt/convoy.tests.rs`
  `invert_guard_refuses_value_under_jmp_hint`.

---

## Solo-test map

Every production `OptimizeOptions` pass has at least one test that either
calls the pass function directly or runs `optimize` with only that flag true.

| Pass | Solo test already existed | Newly added in D1 |
|------|---------------------------|-------------------|
| jump_thread | `convoy.tests.rs` | no |
| dead_block | `convoy.tests.rs` | no |
| stack_dce | `convoy.tests.rs` | no |
| mem_fwd (+ dead_store) | `convoy.tests.rs` | no |
| copy_prop | `convoy.tests.rs` | no |
| canon | `canon.rs` | no |
| algebraic | `algebraic.rs` | no |
| cast_spill | `cast_spill.rs` | no |
| licm | `licm.rs` | no |
| loop_bounds | `bounds.rs` | no |
| loop_unroll | `loop_unroll.tests.rs` | no |
| invariant_store_elim | `invariant_store_elim.tests.rs` | no |
| ssa_gvn | `gvn.tests.rs` | no |
| escape_analysis | `escape_analysis.tests.rs` | no |
| slot_promote | `slot_promote.rs` | no |
| clone_shared_return | `convoy.tests.rs` | no |
| return_convoy | `convoy.tests.rs` | no |
| bin_join_convoy | `convoy.tests.rs` | no |
| multi_op_join_convoy | `convoy.tests.rs` | no |
| invert_guard_branch | `convoy.tests.rs` | no |
| branch_optimization | `branch_opt.rs` | no |
| block_reordering | `block_order.rs` | no |
| seek_back_edge | `slot_promote.rs` | no |
| slot_promote_tell | `slot_promote.rs` | no |
| cfg_gvn | `gvn.rs` | no |
| fuse-select (D4) | `lower.rs` | no |

Run (from repo root):

```bash
cargo test -p compiler --lib -- il::
```

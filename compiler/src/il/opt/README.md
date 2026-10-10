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
| `loop_unroll_factor` | 8 | Trip cap passed to `hir::unroll` (which caps it at 8). |
| `escape_analysis` | on at Standard+ / Size | Gates HIR enum / tuple scalar replacement in `emit_hir`. Not an IL pass. |

## Pipeline order

`optimize_once_at` = cleanup then decision.

**Cleanup** (`cleanup_once_at`), in order:

1. `dead_block` → 2. `canon`

**Decision** (`decision_once_at`), in order:

3. `slot_promote` (+ `dead_store_at`) → 4. `clone_shared_return`

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

A value pushed and then popped right away (`CONST 0` from an inlined unit
return, a statement-position literal or local read) is dropped as it is
emitted (`CodeBuf::push_pop`), and the HIR fold drops `x = x`, so there is no
IL `stack_dce` pass (removed 2026-10).

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

## `local_cse` (moved to the HIR)

Local CSE runs on the HIR (`hir::cse`, after inlining and scalar
replacement, before `hir::licm`), still under the `local_cse` flag. The
stack-IL EarlyCSE pass was removed 2026-10. Hit benches:
`examples/perf/cse_index_recompute.hy`, `for_in_iter.hy`.

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

## `loop_unroll` (moved to the HIR)

Full unroll of short counted loops runs on the HIR (`hir::unroll`, after
inlining, before `hir::fold`), still under the `loop_unroll` flag (off at
`-Os`). The stack-IL pass was removed 2026-10. Hit benches:
`examples/perf/vec_scan_pure.hy`, `vec_scan_impure.hy`.

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

## `branch_optimization` (moved to HIR lowering)

Early-exit layout happens as `emit_hir` lowers a body, still under the
`branch_optimization` flag (Standard, Aggressive, Size). An `if` arm (then
or else), a two-arm niche `match` arm, or the last arm of a pair `match`
(`?`'s `Err(e) => return Err(e)`) that always returns is marked cold; after the
body's fall-through return, `CodeBuf::move_exits_to_end` moves each marked
region to the end and inverts the jump that skipped it, so the code after the
exit falls through. Refused: `ValueUnderJmp` / `nofuse` jumps, a region that
can fall through, and a body with a closure or thunk entry bound after the
region (its offset would move). The stack-IL pass was removed 2026-10. Hit
benches: `examples/perf/fib.hy`, `pair_fib.hy`, `triple_fib.hy`,
`result_try_churn.hy`. This also replaces the stack-IL `block_reordering`
pass (removed 2026-10), which sank such exits.

## Jump threading (in HIR lowering)

At every level, an `if` / `else` whose `end` would be followed by an
unconditional jump (a loop body's back edge) ends its then-arm with a jump
straight there; nested `if`s and a block's last statement pass the target
down (`HirEmit::next_jump`). This replaces the stack-IL `jump_thread` pass
(removed 2026-10). Hit bench: `examples/perf/tak_iter.hy`. Test:
`codegen/lib.tests.rs` `if_else_ending_a_loop_body_jumps_to_the_back_edge`.

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
| dead_block | `convoy.tests.rs` | no |
| canon | `canon.rs` | no |
| algebraic | `algebraic.rs` | no |
| loop_bounds | `bounds.rs` | no |
| slot_promote | `slot_promote.rs` | no |
| clone_shared_return | `convoy.tests.rs` | no |
| fuse-select (D4) | `lower.rs` | no |

Run (from repo root):

```bash
cargo test -p compiler --lib -- il::
```

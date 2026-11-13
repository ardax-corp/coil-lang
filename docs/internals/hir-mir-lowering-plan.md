# HIR → MIR lowering consolidation (plan)

Status: **done** (2026-11-10; see [Execution record](#execution-record)). Proposed 2026-11-09. Tier D of the 2026-11 cleanup audit. Base: `feature/native` at 20ab4b172.

Every body goes HIR → MIR in two stages that walk the same HIR:

1. **The plan**, in `compiler/src/codegen/`: `plan_hir_body`, `hir_mir_tables`, and `hir_mir_input`, which builds `HirMirInput`.
2. **The lowering**, in `compiler/src/mir/from_hir.rs`.

The plan stage is left over from the deleted stack-IL emitter:

- **AST frame numbering.** Parameters, `self`, `__dictN` and unbox tags get their frame slots by interning AST argument names into `context.variables`.
- **An emit cursor** that nothing needs any more.
- **A phased-emit pass** that reorders emission. It is redundant.
- **A bag of side tables:** `HirMirInput` has 88 fields, 16 of them boxed closures over the checker. `HirMirTables` has 29 maps.
- **Repeated re-planning.** After each HIR pass, the plan is redone to decide whether to keep that pass's rewrite.

Goal: one lowering that asks the checker and compiler what it needs as it reaches each node, frames numbered from HIR, and one plan per body. Measurable targets are in [Success criteria](#success-criteria).

The facts below were measured on 20ab4b172. Each line says what it was taken from.

| Fact | Source |
|---|---|
| The "emit-cursor" refusal fires 0 times in the corpus and in spool | instrumented run |
| Phased emit triggers in 186 corpus programs; forcing it off still passes 5527/5527 on native and mir | probe |
| `hir_mir_input` is side-effect free. `hir_mir_walk` is the only place that interns strings, requests method clones, allocates adapter labels, reserves eq glue, records gated host calls and allocates dict-op statics. It does these once per body, in HIR pre-order | read |
| Planning still writes `method_clone_plans`, `planned_method_clones`, `resolved_plans`, `outlined`, `pair_return_kinds` and lambda labels through RefCells, even for plans it throws away. `hir_plan_bodies_for_clones` plans every function only for that side effect | read |
| Corpus fallbacks: early_free 693 of 2,900 attempts (local-type 534, free-shape 153); inline 112 of 4,502; fold 6 (1 body); SROA 3 (1 body); outlined-plan, lambda twin and emit-cursor 0 | instrumented run |
| Re-planning is about 8% of lowering time (spool: ~82 ms of ~1 s). Building inline candidates costs ~166 ms | timing |
| `mir` and `codegen` live in the same crate. No other crate or test builds a `HirMirInput` | read |

## Success criteria

- **Code:** −3,500 lines or more across `compiler/src/codegen/`, `hir/refusal.rs` and `mir/from_hir.rs`. `HirMirInput`, `HirMirTables`, `HirMirFrame`, the emit cursor, phased emit and `context.variables` slot interning are all gone.
- **Behaviour:** the corpus passes on `--engine native` and `--engine mir` (5527+), as do `COIL_NO_FIBERS=1 --engine mir`, `COIL_CODE_IMAGE_CHECK=1`, `tests/debug_cli.rs`, `coil-debug/tests/dap_integration.rs` and `compiler/tests/dissect.rs`. Compiling the same program twice gives byte-identical archives.
- **Run-time perf:** `instructions:u` on the flagships (`--fold-calls off`) and the `examples/perf` hit benches within ±1% at every step, or better.
- **Compile time:** spool `coil compile`, uncached, no slower than 20ab4b172 at any step. After D4, at least 5% faster.

## Phases

The order is chosen for low risk and the most deletion first. D3 only deletes. D2 is a prerequisite for the frame part of D1. D4 comes last, because it changes which rewrites are kept.

### Phase D3: drop the emit cursor and phased emit

Verified when: the corpus passes on both engines, the three ordering tests pass, and archives are byte-identical to the base. D3a is the exception: where phased emit fired, bodies move to source order, so those archives only need to be canonically equivalent (see the decision log).

#### Step D3a: delete phased emit (Size: S)

- **Acceptance:**
  - Deleted: `program_needs_phased_emit`, the phase loop, `reserve_phased_free_fn_entries` (a duplicate of `reserve_program_callable_entries`), `impl_calls_later_free_fn`, and `walk_expr_calls` with its helpers.
- **Verification:** the corpus on both engines, `tests/data/impl_after_caller.coil`, `tests/data/impl_call_later_helper.coil`, `tests/functions/forward_enum_calls.coil`, and spool compiled twice with `cmp` against the base archive.
- **Files:** `codegen/compiler.rs`.
- **Test seams:** the corpus; the archive bytes.
- **Blocked by:** none.

#### Step D3b: delete the emit cursor (Size: S)

- **Acceptance:**
  - Deleted: `emit_idx`, `next_emit_id`, `skip_emit_ids_in`, the `walk_id` / `body_pos` check and the "emit-cursor" refusal.
  - `MethodCloneSrc.emit_idx` becomes a `compiled: bool`, and the replay save/restore goes.
- **Verification:** as in D3a.
- **Files:** `codegen/mod.rs`, `codegen/compiler.rs`, `codegen/emit_hir.rs`.
- **Test seams:** the corpus.
- **Blocked by:** D3a.

#### Step D3c: one source for two-word return kinds (Size: S)

- **Acceptance:**
  - A `debug_assert` fires if a `pair_return_kinds` pin ever contradicts an answer already cached.
  - The for-in `next` pin moves from `hir_mir_walk` to instance registration.
- **Verification:** the corpus in a debug build, which enables the assert, on both engines.
- **Files:** `codegen/emit_hir.rs`, `codegen/emit_hir_mir.rs`.
- **Test seams:** the debug-build corpus.
- **Blocked by:** D3b.

### Phase D2: number frames from HIR

The slot numbers do not change: they are the calling convention (entry slot k is incoming word k). Only where they come from changes.

Verified when: frames built from HIR match the interned frames for every body; the corpus, `debug_cli` and the DAP tests pass; the archive is byte-identical to the base.

#### Step D2a: add `BodyFrame` and check it against the interned frame (expand) (Size: S)

- **Acceptance:**
  - `BodyFrame` is computed from `hir.params`, `callee_param_pairs`, the dict counts (`dict_arity_for`, own dicts, context dicts) and lambda captures. It yields parameter slots, dict slots, `entry_sp` and `frame_base`.
  - In debug builds and tests, every body asserts that it equals today's interned numbering.
- **Verification:** the debug-build corpus on both engines with the assert on, plus `cargo test -p compiler`.
- **Files:** a new `codegen/frame.rs`, plus `codegen/emit_hir.rs` (calls only).
- **Test seams:** the equality assert over the whole corpus.
- **Blocked by:** D3c.

#### Step D2b: read slots from `BodyFrame` (migrate) (Size: M)

- **Acceptance:**
  - The plan's `params` / `frame_base`, every `lookup_slot("__dictN")` (emit_hir ×10, emit_hir_mir ×5, eq_glue ×5, compiler ×2), the name-shadow checks and `hir_lambda_frame` all read `BodyFrame`.
  - `lower_synth_hir` uses it too.
- **Verification:** the corpus on both engines, `tests/debug_cli.rs`, `coil-debug/tests/dap_integration.rs`, `COIL_CODE_IMAGE_CHECK=1` and the archive `cmp`.
- **Files:** `codegen/emit_hir.rs`, `codegen/emit_hir_mir.rs`, `codegen/eq_glue.rs`, `codegen/compiler.rs`.
- **Test seams:** the corpus, the debugger CLI tests, the DAP tests.
- **Blocked by:** D2a.

#### Step D2c: stop interning AST argument names (contract) (Size: S)

- **Acceptance:**
  - The `do_compile` Argument arm no longer interns names. `context.variables`, `lookup_slot`, `alloc_*_slot` and `hir_entry_sp` are deleted.
  - Debug parameters are recorded from HIR.
  - Parameter-shadows-const handling (`const_env`) is kept, or proven to be resolved in HIR already.
- **Verification:** as in D2b, plus `debug_locals_are_exact` and `debug_reads_a_body_with_hundreds_of_locals`.
- **Files:** `codegen/compiler.rs`, `codegen/mod.rs`.
- **Test seams:** the debugger CLI tests.
- **Blocked by:** D2b.

### Phase D1: lowering asks for what it needs

Verified when: `HirMirInput`, `HirMirTables` and `HirMirFrame` no longer exist, and from_hir queries a `LowerCx` (the checker plus a compiler sink) as it lowers each node. The corpus passes on both engines and archives stay deterministic.

#### Step D1a: replace the 16 checker closures with methods (Size: S, two PRs)

- **Acceptance:**
  - `lower_from_hir` takes a `&Checker`-backed context.
  - `is_class`, `pair_class`, `pinned`, `scalar_enum`, `boxed_enum`, `enum_drop`, `word_kind`, `array_stamp`, `pair_variants`, `unit_tags`, `payload_names`, `boxed_view`, `niche` and `scalar_variant` become methods.
  - The archive is byte-identical.
- **Verification:** the corpus on both engines and the archive `cmp`.
- **Files:** `mir/from_hir.rs`, `codegen/emit_hir.rs`.
- **Test seams:** the corpus; the archive bytes.
- **Blocked by:** none. This can run in parallel with D3/D2; see the lanes below.

#### Step D1b: compute per-body facts lazily (Size: M, two PRs)

- **Acceptance:**
  - The 28 per-body fact tables become methods on `Lower`, memoized by `HirId`:
    - PR 1: pair, lane and heap facts.
    - PR 2: records, news, hosts and the rest.
  - The fields copied from `HirPlan` are read from the plan directly.
  - The archive is byte-identical.
- **Verification:** as in D1a.
- **Files:** `mir/from_hir.rs`, `codegen/emit_hir.rs` (`hir_mir_input` shrinks).
- **Test seams:** the corpus; the archive bytes.
- **Blocked by:** D1a.

#### Step D1c: lowering builds the frame (Size: M)

- **Acceptance:**
  - from_hir builds `BodyFrame` itself.
  - `HirMirFrame`, `hir_mir_frame`, `ctx_dicts`, `entry_words` and `lambda_dicts` are deleted.
- **Verification:** as in D2b.
- **Files:** `mir/from_hir.rs`, `codegen/emit_hir_mir.rs`, `codegen/frame.rs`.
- **Test seams:** the corpus and the debugger tests.
- **Blocked by:** D1b, D2c.

#### Step D1d: side effects through a sink trait; delete the tables (Size: M, two PRs)

- **Acceptance:**
  - A `LowerSink` trait (implemented by `Compiler`) covers interning, clone requests, adapters, eq glue, gated host calls and dict-op statics. Lowering calls it where it emits the op.
  - `hir_mir_walk` and `HirMirTables` are deleted.
  - **First PR:** the determinism test is extended. Compiling spool and the corpus `main` programs twice must give byte-identical archives (archives change versus the base, but stay deterministic).
  - **Second PR:** the move itself.
  - The capability check still matches every gated host call.
- **Verification:** the extended determinism test, the corpus on both engines, `COIL_CODE_IMAGE_CHECK=1`, `compiler/tests/dissect.rs`, the capability tests (`macro_host_denied` and the sandbox tests), perf ±1%.
- **Files:** `codegen/emit_hir_mir.rs` (deleted), `mir/from_hir.rs`, `codegen/compiler.rs`, `compiler/tests/pipeline.rs`.
- **Test seams:** the determinism test; the capability tests.
- **Blocked by:** D1c.

### Phase D4: plan once

Verified when: each body is planned at most twice (base plus final); no silent fallback remains; compile time is 5% better; perf ±1%.

#### Step D4a: fallbacks that never fire become errors (Size: S)

- **Acceptance:**
  - outlined-plan, the lambda twin and the old cursor path become internal errors.
  - The one SROA body and the one fold body that fall back get fixed at the source (test first), and then those stages become errors too.
- **Verification:** the corpus on both engines, spool, and the coil-stdlib harness.
- **Files:** `codegen/emit_hir.rs`, the SROA/fold passes in `hir/`.
- **Test seams:** the corpus.
- **Blocked by:** D1d.

#### Step D4b: early_free produces plannable output (Size: M)

- **Acceptance:**
  - The 534 local-type and 153 free-shape fallbacks are fixed where the rewrite is produced, so `classify` can place every local it leaves behind.
  - The fallback count is under 20, with each remaining one listed and explained.
  - Perf follows the hit-bench rule.
- **Verification:** the fallback counter (`--opt-stats` refusal counters) on the corpus, the corpus on both engines, perf.
- **Files:** `hir/early_free.rs`, `hir/classify.rs`.
- **Test seams:** the opt-stats counters; `.coil` tests for each fixed shape.
- **Blocked by:** D4a.

#### Step D4c: resolve calls before planning, run passes, then plan once (Size: M)

- **Acceptance:**
  - Call resolution (what inline and early_free need from `plan.calls` and the host ids) is split out of the plan.
  - Passes run on HIR, then the body is planned once. One bisect-style fallback is kept, behind a counter.
  - Thrown-away plans no longer spend the clone budget or lambda labels.
  - `hir_plan_bodies_for_clones` becomes a cheap clone-discovery walk.
- **Verification:** the corpus on both engines, perf ±1% (the clone set may change; prove it on hit benches), spool compile ≥5% faster.
- **Files:** `codegen/emit_hir.rs`, `codegen/compiler.rs`, `hir/inline.rs`.
- **Test seams:** the corpus, perf, compile timing.
- **Blocked by:** D4b.

#### Step D4d: one validator per concern (Size: M)

- **Acceptance:**
  - `hir::refusal::Walk` owns the shape checks. `hir_check_value` / `hir_check_effect` keep only the representation-edge checks.
  - The duplicated arms are deleted.
- **Verification:** the corpus on both engines, `refusal.tests.rs`, `hir_corpus.rs`.
- **Files:** `codegen/emit_hir.rs`, `hir/refusal.rs`.
- **Test seams:** `refusal.tests.rs`.
- **Blocked by:** D4c.

## Lanes (what may run in parallel)

| Lane | Steps | Notes |
|---|---|---|
| A | D3a → D3b → D3c → D2a → D2b → D2c | Frame and ordering work, mostly in `compiler.rs` / `emit_hir.rs` |
| B | D1a → D1b | from_hir and `hir_mir_input` only |
| — | D1c → D1d → D4a → D4b → D4c → D4d | Starts once A and B are both done; one at a time |

One PR at a time per lane, merged on green into `feature/native`, rebased before each merge. A lane that conflicts twice in a row pauses until the other lane's open PR lands.

## Decision log

| Decision | Chosen option | Why |
|---|---|---|
| Where the work lands | Step PRs straight into `feature/native` | It is already the long-lived branch for #2265, and full CI runs there |
| Slot numbers | Unchanged, only computed from HIR | They are the calling convention and what the debugger shows; D2a's equality assert proves it |
| Archive bytes | Identical through D3–D1c; allowed to change (deterministically) from D1d on | Moving side effects into lowering reorders string, label and static ids. The format is unchanged, so no version bump |
| Fallbacks | Remove those that never fire; fix early_free's at the source; keep one counted bisect fallback | Measured: most fallbacks never fire; early_free's are real |
| Order | D3 → D2 → D1 → D4, with D1a/D1b in parallel | D3 only deletes; D2 is a prerequisite for D1c; D4 changes which rewrites are kept, so it goes last |
| Execution (user, 2026-11-09) | Two lanes (A: D3+D2, B: D1a+D1b), then D1c onward serially | Faster, and the lanes touch mostly separate code |
| D1d archive bytes (user, 2026-11-09) | Deterministic is enough; byte identity with the base is not required from D1d on | The format is unchanged; replaying in today's order would add code for no behaviour gain |
| Archive check when emission order changes (2026-11-09, at D3a) | Byte-identical, or canonically equivalent: same bodies, same string-pool multiset, same metadata, deterministic. Every differing archive is listed in the PR | Deleting phased emit puts bodies in source order in the 7 programs where it fired, spool included, so string indices move. Applies the user's D1d rule (deterministic is enough) to the one step whose premise "no ids move" was wrong |
| D4 scope (user, 2026-11-09) | All of D4a–D4d, gated by the hit-bench rule (±1%) | early_free's 693 fallbacks are real lost rewrites |

## Not yet specified

- **D1d:** settled: `LowerSink` calls happen in lowering's own order (user, 2026-11-09). At pickup, still report code size and perf on spool against the base.
- **D4b:** which early_free rewrites `classify` cannot place. 534 events, not yet bucketed by shape. Researched at D4b pickup.
- **D4c:** whether fewer thrown-away plans change which method clones exist. This is fog until D4c, and is measured with the perf hit benches there.

## Always / Never

- **Always:** both engines agree; each PR is green on its own; test-first for any bug found; perf numbers in every PR that can affect generated code; rebase and re-verify before merging.
- **Never:**
  - change the archive format or bump the archive version (none of this needs it);
  - renumber frame slots or HostInvoke ids;
  - case-specific optimizations;
  - touch `typechecking/` beyond the checker queries D1 needs;
  - retire `purity.rs`, `par_*` or `index_facts` (a separate plan);
  - stack PRs.

## Progress

| Step | Status | PR |
|---|---|---|
| D3a | done | #2365 |
| D3b | done | #2368 |
| D3c | done | #2371 |
| D2a | done | #2372 |
| D2b | done | #2373 |
| D2c | done | #2374 |
| D1a | done | #2366, #2367 |
| D1b | done | #2369, #2370 |
| D1c | done | #2375 |
| D1d | done | #2376, #2377 |
| D4a | done | #2378 |
| D4b | done | #2379 |
| D4c | done | #2381 |
| D4d | done | #2385 |

## Execution record

Base 20ab4b172, end aaf121ec9 (2026-11-10). Every step merged on local verification: the corpus on native and mir (5527 tests at the start, 5546 at the end), the archive comparison, clippy and fmt.

### Against the success criteria

- **Code: missed.** The target was −3,500 lines or more. The files in scope changed by +3,510 / −3,401 (net +109). The named structures are gone: `HirMirInput`, `HirMirTables`, `HirMirFrame`, `hir_mir_walk`, the emit cursor, phased emit and `context.variables` slot interning. But the per-node work they did moved behind `LowerCx`; it was not deleted.
- **Behaviour: met.** Archives were byte-identical at every step except these:
  - D3a: 7 archives differ in body order and string ids, and are canonically equivalent.
  - D1d: 360 corpus archives differ in string ids and stay deterministic. Spool's archive is 68 bytes smaller.
  - D4a–D4c: the kept rewrites change.
- **Run-time perf: met.** Everything is within ±1%. Kept early frees gave one large win: `s2d_inloop_escape` −97%.
- **Compile time: met.** Spool compiles with 13.7% fewer instructions after D4c (wall −11.8%). Most of that came from building inline candidates once per call site, not from removing re-plans.

### What the plan got wrong

- **D3a, "no ids move".** Deleting phased emit puts bodies in source order where it used to fire.
- **D1c, "lowering builds the frame from `BodyFrame`".** `BodyFrame` covers only entry words. Which body-local bindings get a slot depends on dispatch decisions in codegen, so `LowerCx::bound_locals` and `dict_params` stay.
- **D1d, "`emit_hir_mir.rs` is deleted".** It stays, as the per-node site planner behind `LowerCx::site`. The reach set, the reads-dictionaries refusal and the existential packs with their lint stay whole-body.
- **D4a, "one SROA body and one fold body".** There were 3 and 6, plus two fold shapes that only show at `-O0`.
- **D4d, "delete the duplicated arms".** Most of the overlap between the two validators is traversal. Only 7 real duplicate checks were deleted, and 5 shape checks moved into `refusal::Walk`.

### Fixed along the way

- D2b: clone probing planned against a leftover frame of unrelated trait signatures.
- D4b: early_free rewrote an orphaned node that shared operands with a live one. The silent fallback had been hiding this miscompile. early_free fallbacks went from 707 to 0.
- D4b: a free call that does nothing, once per loop pass, on a match over literals (`option_result` +62% before the fix).

### Left open

- `outlined_plan` / `flush_outlined` and the clone probe plan a body without running `refusal` first.
- The two validators disagree on lane arrays: `query::stack_arrays` against `plan_hir_local_layouts`.
- Call resolution still walks the whole body on each inlining round.
- Unplanned trait-instance clone requests do not count against the clone budget. A debug assert guards this.

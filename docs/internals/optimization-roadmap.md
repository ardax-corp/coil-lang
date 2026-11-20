# AOT and JIT optimization roadmap

What the optimizer does today, how a change is proved, and what is open.
Optimizations go in HIR or MIR passes; native code (JIT and AOT) takes the
result. How native code is generated: [native.md](native.md).

## Baseline

Run the repeatable matrix with:

```bash
./scripts/perf_matrix.sh
```

The script builds the release binary, checks each benchmark checksum, compares
the four cross-language benchmarks (plus restored `fib`) against Lua and Node, runs the Coil-only
examples, and writes raw `poop` output plus metadata under
`/tmp/coil_perf_matrix/`. Set `OUT_DIR` for another location, `DURATION_MS`
for longer samples, or `RUN_MASSIF=1` to collect optional Valgrind Massif files.

The matrix runs each cross-language row twice, with `--auto-par off` and with
auto-par at its default. These are directional comparisons, not language
rankings: the ports have different runtime startup, library and allocation
behavior. `numeric`, `operators_loop` and `match_sum` are Coil-only: the
matrix keeps their results, but they have no Lua or Node ports.

Extra benches: `examples/perf/gc_churn.coil`
([#286](https://github.com/ardax-corp/coil-lang/pull/286)),
integer-payload Option/Result churn
([#289](https://github.com/ardax-corp/coil-lang/pull/289)), and the hit benches
listed under the prove rule below.

## MIR passes

`specialize::optimize_mir` (`compiler/src/mir/specialize.rs`) runs these in
order on each body native code takes (`native_capture`). A generator, a body
with a cleanup pad, a debugger's or a coverage build's body keeps its MIR as
lowered, and the MIR interpreter's reference capture runs no pass.

| Pass | Module | Runs when |
|------|--------|-----------|
| SCCP | `mir/sccp.rs` | `mir_fold` |
| CSE, copy forwarding, LICM, CSE (`loop_opts`) | `mir/cse.rs`, `mir/destprop.rs`, `mir/licm.rs` | `mir_loop` |
| Loop interchange, then `loop_opts` again | `mir/interchange.rs` | `mir_loop` |
| InstCombine, DestProp | `mir/instcombine.rs`, `mir/destprop.rs` | `mir_fold` |
| IV strength reduction, CSE, GVN | `mir/strength.rs`, `mir/cse.rs` | `mir_loop` |
| SROA, `phi_sroa` | `mir/sroa.rs`, `mir/phi_sroa.rs` | always |
| `thread_bool_phis` | `mir/bool_thread.rs` | `mir_fold` |
| `copy_until`, `byte_scan`, `bulk_push`, `vec_loops` | `mir/copy_until.rs`, `mir/byte_scan.rs`, `mir/bulk.rs`, `mir/vec_loop.rs` | `mir_loop` |
| `mark_prefetch_scans` | `mir/prefetch.rs` | `mir_loop` |

`mir_fold` is on from `-O1` except at `-Og`; `mir_loop` is on at `-O2`, `-O3`
and `-Os`. IV strength reduction is
float-only, because int overflow traps ([int-overflow.md](int-overflow.md)).

HIR passes run first, in `codegen/emit_hir.rs`: `hir::fold`, `hir::inline`,
`hir::outline`, `hir::unroll`, `hir::bounds`, `hir::sink_return`, and enum,
tuple and readonly-class SROA. `hir::fold` puts an `int` literal on the right
of a commutative op.

## What ships

Under [`OptLevel`](../../compiler/src/opt/opt_level.rs) (`Standard` is the
default). This is what ships, not a textbook pass list.

| Area | What it does | Ceiling (do not overshoot in docs or code) |
|------|--------------|--------------------------------------------|
| **Opt levels** | `-O0`…`-O3`, `-Os`, `-Og` via CLI / `Pipeline::set_opt_level` | `None ⊂ Basic ⊂ Standard ⊂ Aggressive`. `Size` drops unroll + `sink_return`; `Debug` = HIR constant folding only (no scalar replacement, unroll or inlining). |
| **CSE** (`mir/cse.rs`) | SSA numbering on MIR: a repeated pure operator / cast, and in one block a repeated field / element / length / pure-call read, reads the first value | Heap reads are killed by a store, an append or a call that may write the heap or resize. Hit: `cse_index_recompute`, `cse_cast_recompute`. |
| **LICM** (`mir/licm.rs`) | Hoists invariant values out of natural loops; an op that may trap (int arithmetic, an element read, a pure call) moves only where the loop is sure to compute it, behind a copy of the loop test | Hit: `licm_nested_chains`, `licm_lir_pure_call`. |
| **Tail calls** | A call in return position with a matching ABI is planned as a tail call (`MirFunc::tail_targets`) | The MIR interpreter takes the frame. Native code compiles it as a plain call. Hit: `tail_sibling`. |
| **`loop_bounds`** | Proven sites lower to an unchecked `Index` / `StoreIndex`. The checker's `index_facts` sidecar proves them on source; `hir::bounds` proves the shapes inlining and unrolling leave; `mir/licm.rs` hoists `a.len()` | **`<=` / `>=` headers are not length proofs** (COI-85 / COI-98). Unproven, yield, growing-array and resizing-call loops stay checked. |
| **`loop_unroll`** (`hir/unroll.rs`) | Full unroll of counted loops, trip ≤ 8, after inlining; each copy reads the counter as a literal, then `hir::fold` folds the copies (merging `x = x + a; x = x + b`) | Calls, exits, closures and inner loops that do not unroll themselves refuse; nests flatten over up to 3 rounds; ≤ 512 added nodes per loop. `<=` accepted for **trip count** only, separate from bounds proofs (COI-98). |
| **`escape_analysis`** | The option gates HIR enum, tuple and readonly-class scalar replacement | Off at `-O0`, `-O1`, `-Og`, and in a debugger build. |
| **`--opt-stats`** | HIR lowering, inlining and compile-time counters to stderr / JSON | **Default off** (COI-131). |
| **Branch layout** | Heuristic: MIR block layout puts early exits after the body (`HirPlan::cold_ok`) | Default **on** from `-O2` (COI-128 / COI-129). |

**PGO is gone** ([#301](https://github.com/ardax-corp/coil-lang/pull/301)): no
flags, ingest, instrumentation, heat knobs or `BranchProfile`. Branch layout
stays heuristic.

Pass headers in `compiler/src/hir/**` and `compiler/src/mir/**` are the source
of truth when this table and Linear disagree.

## Hit-bench prove rule

If an opt is sound but does not fire on flagship `.ccoil` (`mandelbrot`, `tak`,
`nsieve`, `binary_trees`, `fib`), add focused `examples/perf` hit benches and
**prove those**. Do **not** skip merge solely because flagship archives are
identical; flagships remain controls. Skip only on hit-bench wash or regress.

Build flagship archives with `--fold-calls off`. At `-O2` and above a pure
call with constant arguments is computed at compile time
([comptime.md](comptime.md)), so `mandelbrot`, `tak` and `nsieve` otherwise
compile `main` to a constant and the benchmark never runs:

```bash
./target/release/coil compile --auto-par off --fold-calls off \
  --root .deps/coil-stdlib/src examples/perf/mandelbrot.coil -o /tmp/m.ccoil
```

Landed hit benches: `iv_mul_sr`, `licm_nested_chains`, `tail_sibling`,
`cse_index_recompute` / `cse_cast_recompute`, `dest_prop_field_alias`,
`result_try_churn`, `mir_cse_divf`, `mir_licm_divf`, `mir_instcombine`,
`mir_destprop`, `mir_iv_sr`, `mir_gvn_divf`, `mir_float_pipeline`,
IPA top-site `examples/perf/fib.coil` (`--auto-par on`, COI-361 E3),
`vec_scan`, `vec_axpy`, `s2j_class_sroa`, `s2d_inloop_pack`,
`s2d_inloop_escape`, `option_self_call`.

## Open work

### Bounds proofs

What keeps a loop's length proof is `purity::LengthStability`: the proof asks
"can this change an array's length?", not "is this pure?". A user callee is
length-stable when its call-graph closure has no `RESIZE` effect. When a
`fn drop()` in the compile may resize, only pure calls are length-stable and
allocating ops stay barriers, because finalizers run at allocation safepoints.
Hit: `examples/perf/vec_scan_impure.coil`. A dynamic index, or a loop whose
array may grow, stays checked. Refusals: [limitations](limitations.md).

### Allocation and GC

Aggregate construction is not copy-bound: making the builders one `memcpy`
measured neutral on `binary_trees` (on the bytecode VM, since deleted). What
is left is payload layout, not identity: `find_object_by_addr` is a slab probe
([heap-identity.md](heap-identity.md)), and collection is incremental
([gc-incremental.md](gc-incremental.md)).

### Calls

A ground trait or instance method site is a direct MIR `Call`. `CallIndirect`
stays for function values, dictionary method slots and generic shared-body
evidence that is not static at the call site.

### Auto-par fork-site profitability

Priority: landed; F0 drops fib-unit inversion (COI-367); F1 one parameterized
fork worker per site (COI-366).

IPA specialization used to invert guard-pruned fork-site nodes `W` into
fib-equivalent units so the expression threshold could stay **20**. F0 compares
`W` **directly** to a grain floor (default **10945** = `W(fib(20))` =
`Fib(21)-1`). Verdicts on the calibrated loads stay the same: `fib(21)`
forks, `fib(20)` refuses, `tak(24, 22, 20)` refuses, and the fair
`tak(18, 12, 6)` bench load is 8398 grain (below the tight fib floor, above
the loose `SelfCall` floor **8000**, so it forks). Loop IPA
keeps trip-count grain via `DEFAULT_LOOP_GRAIN` (20). Full formula and
verdict table in [auto-par](auto-par.md#expression-grain-w).

The work cost is compile-time and bounded by construction: the walk is memoized
per `(fn, arg vector)`, capped at 256 levels deep and 2^14 memo entries, and
saturates one node past the grain floor — counting further cannot change the answer.
F1 emits **one** parameterized worker per demanded site (`__coil_par_{f}`);
nested AlwaysPar is `PAR_SPEC_HOPS` depth on that worker, not a constellation
of frozen arg clones. Below-threshold or dynamic arg sites stay on the
sequential original, so there is no hot-path grain skip-threshold.

## Staged gates

1. **Baseline gate:** `perf_matrix.sh` produces metadata and raw results for
   every comparison; no benchmark is accepted without a correctness checksum.
2. **Opt gate:** prove the rewrite on a bench it actually changes. Prefer a
   focused `examples/perf` hit bench when flagships do not fire (see
   [Hit-bench prove rule](#hit-bench-prove-rule)). A hit bench should move
   wall time or instructions without regressing controls; identical
   flagship `.ccoil` is not a skip. Do not revive PGO to manufacture heat.

## Compile-time scaling (#1857)

`coil compile` on a file of N one-line functions that `main` calls in
turn (release build, wall time, one machine, 2026-10-25):

| N    | before   | after   |
|------|----------|---------|
| 900  | 834 ms   | 303 ms  |
| 1800 | 2498 ms  | 622 ms  |
| 3600 | 9845 ms  | 1251 ms |

Before, each doubling cost about 4x; now it costs 2x. Five passes were
quadratic in the function count or the body size:

- The checker's module frame looked names up by a backwards scan of every
  top-level binding; it now keeps a name index.
- Unification extended a clone of the whole-program substitution, copying
  it on every binding, and composing the result rebuilt it again. Writes
  to shared data go to a small overlay now, and `Subst::absorb` folds an
  extension in place, rewriting only the positions `compose` would change.
- Enum and readonly-class SROA walked the body once per local; they now
  only check locals that some `let` builds a variant or class into.
- Index facts searched the program for each function's body and call
  sites; both are looked up by name now.
- Tree shaking moved every PC, entry and func record once per dead
  function; disjoint dead spans then moved them all in one pass (that
  code went with the bytecode).

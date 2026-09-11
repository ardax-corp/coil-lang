# Auto-par shape selection (post-MIR)

Design / analysis only. **No IPA semantics rewrite in this note.** Dimitar
asked whether compile-time AlwaysPar is still the right way to pick
parallel shapes after MIR / dense / Q6–Q8 / A0–C3. Later: he
**deprioritized IPA shapes** in favor of a tree-shake / code-reorder
assessment — treat P1+ here as parked hunches, not a queue.

Related: [auto-par.md](auto-par.md) (current IPA),
[auto-par-rss.md](auto-par-rss.md) (PR #403: isolate tax, AlwaysPar
spawn tree, `COIL_AUTO_PAR` compile-time only),
[opt-generalization.md](opt-generalization.md) (A0 doctrine + A4
measurement), [q6-iterator-protocol.md](q6-iterator-protocol.md).

**Invite discard.** Several ranked items below are hunches. Kill them
if measurement or tree-shake work makes them irrelevant.

## Current AlwaysPar / auto_par picking

### Files

| Piece | Where |
|-------|--------|
| Compile-time on/off | `auto_par_enabled` in [`compiler/src/codegen/mod.rs`](../../compiler/src/codegen/mod.rs) (`COIL_AUTO_PAR=0` / `false` / `off` / `no`) |
| Profitability cutoff | `par_cost_threshold` in [`par_profit.rs`](../../compiler/src/typechecking/par_profit.rs) (`COIL_PAR_THRESHOLD`, default **20**) |
| Expression IPA | `analyze_par_fork_sites`, `par_work_units` / `args_worth_parallel`, `collect_par_specialization_args` |
| Loop IPA | [`loop_par.rs`](../../compiler/src/typechecking/loop_par.rs) (`analyze_loop_par_sites`) |
| Purity | [`purity.rs`](../../compiler/src/typechecking/purity.rs) |
| AlwaysPar emit | `emit_one_par_specialization`, `try_emit_par_loop` in [`compiler.rs`](../../compiler/src/codegen/compiler.rs) |
| Reactor | [`machine/src/reactor.rs`](../../machine/src/reactor.rs), [`WorkerCap`](../../machine/src/thread.rs) |

`COIL_AUTO_PAR` is **not** a `.hyc` run-time switch. Recompile to change
IPA. Pool size (`COIL_MAX_WORKER_THREADS`) *is* run-time.

### What is specialized

**Expression IPA** — one primary fork site per pure function (prefer
return-path, evaluable guards). Combine shapes: `BinOp` `+`/`-`/`*`,
`EnumCtor`, `SelfCall` (tak), `ApplyCall`, `Tuple`. Constructor-pattern
`match` arms stay **opaque** (no specialize). Irrefutable `_` / binding
arms are ok.

For each demanded **const** arg vector whose work score exceeds the
threshold, codegen emits a nullary `__coil_par_{f}_{args…}` clone that
**always** `thread_spawn`s arm 0, runs other arms inline, `thread_join`s,
combines. Failed spawn/join → sequential arms. Matching const call sites
rewrite to `CALL` the clone. Dynamic / below-threshold args stay sequential
(no hot-path threshold tax).

Closure: BFS under arm `ArgForm`s, drop children that miss guards or
fall below the cutoff, cap **64** clones per function (`PAR_SPEC_BUDGET`).
That is AlwaysPar **down to the cutoff**: `fib(22)` also emits `fib(21)`.

**Loop IPA** — AST `while i < K` / `i <= K` only (not `for x in`, not
C-style `for`). Const trip count `end - begin > 20`, one `+1` step, one
int `+`/`*` reduction, body independent of `acc`, no index/field/branch.
Two chunks: `__coil_par_loop_{n}(lo, hi, acc)`, spawn upper half from
identity, inline lower half from live `acc`.

### Work score (expression)

Not `max(args)`. `W` counts guard-pruned **fork-site nodes**, then inverts
the fib recurrence so `fib(n)` scores `n`:

```
W(f, args) = 0 if guards miss / negative / no site
           = 1 + Σ_arms W(callee, arm(args))
score      = min { n : Fib(n+1)-1 >= W }
fork iff score > COIL_PAR_THRESHOLD
```

Memo + depth 256 + 2^14 entries; saturate one node past cutoff. Imprecision
resolves **down** (unknown → refuse). Fair `tak(18,12,6)` sits on the
cutoff and stays sequential; `tak(24,22,20)` is 53 calls and refuses.

Loop IPA reuses the **same 20** as a **trip-count** floor, not `W`.

### What actually forks on flagships

With the work score, `mandelbrot` / `tak` / `nsieve` / `binary_trees`
typically never spawn ([optimization-roadmap.md](optimization-roadmap.md)
§6). `fib(n)` for `n > 20` AlwaysPars every level down to 21. RSS/wall
on `fib(32)`: ~1.6× on 4 cores, nested spawn tax near the cutoff
([auto-par-rss.md](auto-par-rss.md)).

`.cargo/config.toml` forces `COIL_MAX_WORKER_THREADS=1` for cargo; IPA
then looks like a wash or a loss.

## Why MIR/opt work matters (and may not)

A0: majority programs, checksum + **cost gate**, never denser-but-slower.
MIR `emit_cost` (LOAD/STORE/Seek weighted vs dense) is a **reconstruct**
gate, not a parallel-work model.

| Recent work | IPA today | Tension |
|-------------|-----------|---------|
| **Q6 / B5 / C2** counted `for` / range → dense i64 loops | Loop IPA is `while` AST only; `for x in` is a different node; body **index** is a refuse | Majority counted loops never fork; the sequential path got *faster* |
| **Q7 / B2** dense one-word rec `CALL` | Expression IPA still fib-node `W` on AST | Sequential `fib`/`tak` cheaper → spawn/join relatively worse |
| **Q8** niche/two-slot dense `Br` | Constructor match still opaque to AlwaysPar | Do not invent match-IPA without evidence |
| **A2 / dense ops** | `W` counts fork nodes, not `DenseBin` / `VReduce` work | A dense saxpy loop can be huge work and still fail loop IPA (index) |
| **A4** no env toggles for opts | `COIL_AUTO_PAR` / `COIL_PAR_THRESHOLD` are compile env | Keep them compile-only; do not add a run-time archive IPA switch |

**Hunch (discard freely):** after Q7, recursive fib-style IPA may be the
*wrong* majority shape. Counted independent iterations with grain ≫
isolate tax are more likely to win — if we can see them.

## Ranked redesign (shape selection only)

Not full runtime IPA. Not reactor isolate work (that is RSS #403 items
1–6). Policy for **which sites emit a fork**.

1. **Top-site / evidence-gated fork** over recursive AlwaysPar to cutoff.
   Emit at most the *demanded* const call (or a small depth), children
   stay sequential `f`. Cuts `__coil_par_fib_21…n` fan-out and near-cutoff
   spawn tax. RSS #403 item 7. **Policy change** — needs a hit bench, not
   this doc.

2. **Counted `for` / range (Q6) as candidates vs recursive fib.** Reuse
   loop-IPA independence + associative reduction on the **desugared**
   counted latch (`for x in arr` / `0..n` / B5 local / C2 param-ret),
   not a second AST walker. Trip count or `len` must still beat spawn
   tax. Index in a reduction `e` is the current hard refuse — lifting
   that for proven-local arrays is the actual majority unlock, not
   another fib clone.

3. **Work estimate from MIR/dense when it is better, not instead of
   gates.** Keep purity + independence on the AST/sidecar. Use `W` only
   where the site is recursive fork-nodes. For loops: `trip_count ×`
   per-iter `emit_cost` (or dense op count) vs a **measured** spawn+join
   floor — do not reuse fib-units as if 21 trips ≈ `fib(21)`. Refuse if
   the sequential body already won the MIR cost gate as a tiny helper.

4. **Optional: compile-time plan + cheap runtime skip**, without
   `COIL_AUTO_PAR` on `coil run foo.hyc`. Archive still contains the
   fork (or not) from compile. At the existing spawn site: if
   `WorkerCap.max() == 1` (or reactor not worth starting), take the
   sequential arm path **without** `submit`. Optional grain: dynamic
   `n > T` for loops that compile-time could not prove — one compare,
   not a recursive threshold. Do **not** start the pool to decide.

## Phases (parked)

| Phase | Intent | Code? |
|-------|--------|-------|
| **P0** | Inventory: which `.hy` in examples/tests actually emit `__coil_par_*`; flagship on/off noise; grain vs isolate tax | measure only |
| **P1** | Top-site AlwaysPar (stop closing the spec chain to cutoff) | yes, evidence-gated |
| **P2** | Counted `for` / range loop IPA on Q6 desugar + loop-shaped cost, not fib-units | yes, A4 |
| **P3** | Cheap runtime skip (`workers==1`, optional dynamic grain) on an already-emitted plan | small VM/codegen; **no** archive IPA flag |

Do not start these while tree-shake / reorder is the live question.

## What NOT to do

- Rewrite IPA semantics in the same PR as RSS / tree-shake / this doc.
- Make `COIL_AUTO_PAR` a run-time archive switch.
- Lower `COIL_PAR_THRESHOLD` to “force” forks (already slower + stack/
  spec-budget risk).
- Hot-path runtime threshold on every recursive call (`if n > 20 spawn`).
- Bench-shaped parallel opcodes or allowlists (`fib` / `nsieve` only).
- Dual semantic IR for par; fuse-IL as a competing par optimizer.
- Fork denser-but-slower MIR reconstructs; skip the cost gate for Q7
  `fib`/`tak`.
- Start the reactor at `Machine` construct (RSS on sequential programs).
- Treat Q8 constructor `match` as a fork site without a prove board.
- Revive PGO / `BranchProfile` to pick sites.

## Hunches to throw out

Please discard any of these rather than implement them out of inertia:

- “MIR `emit_cost` should replace `W` for recursive IPA.” Maybe `W` is
  fine for trees and MIR cost only matters for loops.
- “Top-site alone fixes fib wall time.” Isolate memcpy/help-VM (#403)
  may dominate nested AlwaysPar.
- “Loop IPA on `for x in` is the majority win.” Independence + no
  shared index writes may still refuse `nsieve` / mandelbrot.
- “P3 `workers==1` skip is free.” The sequential fallback already
  exists after a failed spawn; skipping `submit` only helps the cargo
  `WorkerCap=1` case, which we might instead document as “don’t measure
  IPA under cargo.”
- “More chunks / recursive chunking.” Two-way + steal may be enough
  once grain is real.

P0 measurement should kill or promote before any of P1–P3 land.

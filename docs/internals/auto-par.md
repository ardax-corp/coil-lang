# Automatic parallelization

coil can fork-join **independent parallel arms** (IPA) without a source-level
`par` / `spawn` annotation. Two shapes qualify today:

```coil
return fib(n - 1) + fib(n - 2);      // expression IPA — independent pure calls
return sq(n) + sq(n - 1);            // helper arms (no self-recursion required)
while i < 100 { acc = acc + f(i); i = i + 1; }   // loop IPA — iteration arms
```

Both go through the same four gates — **purity**, **independence**,
**profitability**, **semantic identity** — and both are recognized structurally.
There are no function, module or program allowlists: a shape either proves out or
stays sequential.

## Purity analysis

After typecheck, [`purity`](../../compiler/src/typechecking/purity.rs) walks
the AST and records [`EffectFlags`](../../compiler/src/typechecking/purity.rs)
on the typed sidecar (`DefId` + bind names). Codegen copies those names into
`PureCallCtx` for LICM / length proofs:

- A function is **locally impure** if it uses `panic` / `yield` / FFI / `defer`,
  mutates via index/field assignment, or calls a non-identifier callee.
- Calls to names that are not user `fn`s (e.g. imported `write_all`, `spawn`,
  `collect`, `attach`) take the matching effect bit (unknown names are impure).
- Impurity propagates through the user-function call graph (fixed point).
- `analyze_pure_fns` returns everything that survives; `analyze_recursive_pure`
  keeps only the subset that **calls itself**. `$mono$` clones of a pure bind
  stay pure for LICM.

Expression IPA runs on **any pure** function whose body contains a fork site
(self-calls or independent helper calls). Loop IPA also needs pure body callees.

Disable both transforms with `COIL_AUTO_PAR=0` (or `false` / `off` / `no`).

## Expression IPA: static profitability (no runtime threshold checks)

[`par_profit`](../../compiler/src/typechecking/par_profit.rs) detects **fork
sites** on pure functions — expressions whose operands are two or more
independent pure calls — and collects **constant** call-site arguments
(`fib(32)`, …). Combine shapes recognized:

| Combine | Source shape |
|---|---|
| `BinOp` | `f(…) ⊕ g(…)` (`+` / `-` / `*`) |
| `EnumCtor` | `E::V(f(…), g(…))` (tuple or record payload) |
| `SelfCall` | `f(f(…), f(…), …)` (tak-style) |
| `ApplyCall` | `h(f(…), g(…))` for a pure `h` |
| `Tuple` | `(f(…), g(…))` |

Arms are described structurally (`ArgForm::Const` / `Param` / `ParamMinus`), so
any arity works and child arg vectors are derived statically. Detection walks
full function bodies, including **irrefutable** match arms (`_` / binding);
constructor-pattern arms stay opaque (AlwaysPar would skip the match). Forks
never span exclusive alternatives.

For each demanded constant argument vector whose **work score** exceeds
`COIL_PAR_THRESHOLD` (default **20**), and that still reaches the fork under the
site's path guards, codegen emits a nullary specialization
`__coil_par_{f}_{a}_{b}_…` that **always** forks:

1. `MakeFn` of a child specialization when one exists for an arm's derived args,
   otherwise `MakeFn` of the arm's callee with those concrete args.
2. `thread_spawn` the first arm into the work-stealing reactor (no `GT` gate).
3. On `Ok(handle)`: evaluate remaining arms locally, `join` (help-steals), apply
   the site's combine.
4. On `Err` (spawn or non-sendable join): sequential fallback of all arms + combine.

Call sites with matching const args rewrite to `CALL` the specialization.
Below-threshold / dynamic args stay on the original sequential `f` (no hot-path
runtime threshold tax).

### The work score

A site's cost used to be `max(args)`, which reads argument *magnitude* as if it
were work. It is not: `tak(24, 22, 20)` is 53 calls but outranked `fib(23)`.

Instead, `par_work_units(sites, f, args)` counts the **fork-site nodes** reachable
from a concrete arg vector:

```
W(f, args) = 0                                  if args miss f's guards, go
                                                negative, or f has no fork site
           = 1 + Σ_arms W(callee, arm(args))    otherwise
```

Guard pruning is what makes this a work model rather than a size model: a child
that fails the site's path conditions is a base case and contributes nothing.
Arms into *other* pure functions recurse into that function's own site, so heavy
helper arms count and trivial ones do not. The walk is memoized per arg vector
and bounded by a depth cap, a memo-entry cap, and saturation at the cutoff.

`W` is then converted back into **threshold units** by inverting the same
recurrence on the canonical shape — the units are “as much work as `fib(n)`”:

```
fib_nodes(n) = Fib(n + 1) - 1        // W for fib(n-1) + fib(n-2), base n <= 1
score(args)  = min { n : fib_nodes(n) >= W }
fork iff score > COIL_PAR_THRESHOLD
```

So the threshold keeps its old meaning on the shape it was calibrated on:
`fib(n)` scores exactly `n`, and the default **20** still admits `fib(21)` and
refuses `fib(20)`. What changed is everything that is *not* fib-shaped:

| Site | `max(args)` | Work score | Real calls |
|---|---|---|---|
| `fib(21)` | 21 → fork | 21 → fork | 35 421 |
| `fib(20)` | 20 → refuse | 20 → refuse | 21 891 |
| `tak(18, 12, 6)` (fair bench) | 18 → refuse | 20 → refuse | 63 609 |
| `tak(21, 12, 6)` | 21 → fork | 23 → fork | 230 613 |
| `tak(24, 22, 20)` | 24 → fork | 5 → refuse | 53 |
| `sq(n) + sq(n - 1)` at 22 | 22 → fork | 2 → refuse | 2 |
| `fib(n) + fib(n - 1)` at 22 | 22 → fork | >20 → fork | 92 734 |

Every imprecision resolves *downwards* — an arm into a function with no fork
site, a `SelfCall` combine's re-entry on joined values (unknowable statically),
the caps — so the score is a lower bound on the tree and unknown structure can
only make a site refuse. `tak` is the interesting case: its arms rotate
parameters, so a large component stays alive, but many children miss the `y < x`
guard and the combine's re-entry is invisible. The fair benchmark
load lands exactly *on* the cutoff and stays sequential; only a genuinely deeper
tree crosses it.

The default **20** is a profitability floor, not an arbitrary gate: forking below
it (e.g. `COIL_PAR_THRESHOLD=12` on `fib(32)` or `tak(18,12,6)`) multiplies
reactor spawn/join work and is typically **slower** than sequential, and very
low values can exhaust the specialization budget or overflow worker stacks.
Raise the workload (larger const args) when you want IPA evidence; do not lower
the threshold to “force” more forks.

## Loop IPA: chunked fork-join over an induction range

A counted loop is the same idea with the arms spread over an induction range.
When the iterations only communicate through one **associative** reduction, any
partition of the range folds to the sequential result, so the range splits into
contiguous chunks that each accumulate a private partial.

[`loop_par`](../../compiler/src/typechecking/loop_par.rs) admits a `while` loop
only when **every** gate holds:

| Gate | Requirement |
|---|---|
| Shape | `while i < K` / `i <= K`; body is a statement list |
| Induction | exactly one `i = i + 1` / `i += 1` / `i++`; `i` is a const-initialized local |
| Trip count | `K` is compile-time, and `end - begin > COIL_PAR_THRESHOLD` |
| Reduction | exactly one `acc = acc + e` / `acc = acc * e` (or `+=` / `*=`) on a const-initialized local |
| Independence | `e` never reads `acc`; the body reads only `i`, its own `let` temps and int literals |
| Purity | body calls only pure user functions; no index / field / static writes, no branches, `break`, `return` or `yield` |
| Types | the induction variable and `e` both infer to `int` — float reduction is not associative |

Ranges are normalized half-open (`i <= K` becomes `end = K + 1`), so a split is
just a partition of `[begin, end)`.

Codegen emits one private **chunk worker** per site,
`__coil_par_loop_{n}(lo, hi, acc)`, holding the original body over `[lo, hi)` and
returning the partial. At the loop site:

1. `MakeFn` the worker, then `thread_spawn(worker, mid, end, identity)` — the
   upper chunk starts from the operator's identity (`0` for `+`, `1` for `*`) so
   the accumulator's initial value is counted exactly once.
2. Call the worker inline for `[begin, mid)` seeded with the live `acc`.
3. `thread_join` (help-steals), then fold the two partials with `ADD` / `MUL`.
4. Store the fold into `acc` and set `i` to `end`, the value the sequential loop
   would have left behind.

On a failed spawn or join, a single worker call covers `[begin, end)`.

## Deferred

- C-style `for (let i = 0; i < N; i = i + 1)` — the analysis shape is the same,
  but the step lives outside the body so the worker needs a second emit path.
  (Const trip counts up to 8 already fully unroll, well below the threshold.)
- Dynamic trip counts. Splitting `while i < n` needs a runtime `n > threshold`
  branch; the first slice refuses to pay that tax.
- More than two chunks, and nested / recursive chunking.
- Conditionals in the body, float reductions, and reductions over `min` / `max`
  or user operators.

## Work-stealing reactor

[`machine/src/reactor.rs`](../../machine/src/reactor.rs) owns a fixed pool of OS
threads (size = [`WorkerCap`](../../machine/src/thread.rs), default
`available_parallelism`). Jobs land on a crossbeam injector / local deques;
idle workers steal. `thread::spawn` / auto-par share this pool — no per-call
`std::thread::spawn`.

| Env | Effect |
|-----|--------|
| `COIL_MAX_WORKER_THREADS` | Pool size (1..=512). Default `available_parallelism` (min 2), or **1** when `CI` is set. `.cargo/config.toml` also sets this to `1` (`force = false`) for local cargo test runs. Export a higher value to profile parallelism. |
| `COIL_AUTO_PAR` | `0` / `false` / `off` / `no` disables auto fork-join codegen. |
| `COIL_PAR_THRESHOLD` | Compile-time profitability cutoff — fork-site work score and loop trip count (default 20). |

Pool workers pin a TLS local deque tagged with the owning reactor identity.
`submit` / join-help only push or pop that deque when it belongs to the same
reactor; otherwise work goes through the shared injector. That keeps concurrent
`Machine`s (parallel tests) and nested reactors from cross-feeding jobs.

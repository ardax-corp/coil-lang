# Automatic parallelization

coil can fork-join **independent parallel arms** (IPA) without a source-level
`par` / `spawn` annotation. Two shapes qualify today:

```coil
return fib(n - 1) + fib(n - 2);      // expression IPA: independent pure calls
return trib(n - 1) + trib(n - 2) + trib(n - 3);  // n-ary associative + (detected)
return fib(n) + fib(n - 1) + fib(n - 2);         // n-ary helper arms (`triple_fib`)
let a = fib(n - 1); let b = fib(n - 2); return a + b;  // let-bound arms
return sq(n) + sq(n - 1);            // helper arms (no self-recursion required)
while i < 100 { acc = acc + f(i); i = i + 1; }   // loop IPA: while
for x in 0..100 { acc = acc ^ f(x); }            // loop IPA: xor reduce
```

Both go through the same four gates (purity, independence, profitability,
semantic identity) and both are recognized structurally.
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
| `BinOp` | `f(…) ⊕ g(…)` (`+` / `-` / `*` / `^`); nested `+` / `*` / `^` flatten to N arms |
| `EnumCtor` | `E::V(f(…), g(…))` (tuple or record payload) |
| `SelfCall` | `f(f(…), f(…), …)` (tak-style) |
| `ApplyCall` | `h(f(…), g(…))` for a pure `h` |
| `Tuple` | `(f(…), g(…))` |

F2 (COI-368) also admits **let-bound** arms when the statements immediately
before the combine are `let name = pure_call(…)` and each name is used
**exactly once** as a leaf (`let a = f(…); let b = g(…); return a + b`). A
reused name (`a + a`) is one value, not two arms. Any intervening statement
refuses: the worker re-emits only arms + combine.

Arms are described structurally (`ArgForm::Const` / `Param` / `ParamMinus` /
`ParamPlus`), so any arity works and child arg vectors are derived statically. Detection walks
full function bodies, including **irrefutable** match arms (`_` / binding);
constructor-pattern arms stay opaque (AlwaysPar would skip the match). Forks
never span exclusive alternatives.

For each demanded constant call whose **fork-tree grain** `W`
exceeds `COIL_PAR_THRESHOLD` (default **10945**), and that still reaches the fork under the
site's path guards, codegen emits **one** parameterized worker
`__coil_par_{f}(args…, hop)` (COI-366 F1) that **always** forks:

1. Path guards and `hop <= 0` fall through to the sequential original (reachability
   / depth, not a grain skip-threshold).
2. `MakeFn` the first arm: self-arms with `hop > 1` re-enter this worker with
   `hop - 1` and live `ArgForm` args; otherwise the sequential callee.
3. `thread_spawn_shared` that arm (HostInvoke **137**; isolate
   `thread_spawn` when maps are missing for non-immediates, `COIL_SHARED_HEAP=0`,
   debugger attached, or an arg misses the C0 whitelist). No `GT` grain gate.
4. On `Ok(handle)`: evaluate remaining arms locally (same hop policy), `join`
   (help-steals), apply the site's combine. Shared join publishes raw `Value` bits
   (no graph copy).
5. On `Err` (spawn or non-sendable join): sequential fallback of all arms + combine.

Const call sites rewrite to an ordinary `CALL` of that worker (plus
`PAR_SPEC_HOPS`, default **2**). Below-floor / dynamic / base-case args stay on
the original sequential `f` (no hot-path runtime grain tax). Hops are depth on
the same function — not a constellation of frozen `__coil_par_f_a_b_…`
clones, and not AlwaysPar every level down to the cutoff.

### Expression grain (`W`)

A site's cost used to be `max(args)`, which reads argument *magnitude* as if it
were work. It is not: `tak(24, 22, 20)` is 53 calls but outranked `fib(23)`.
A later step converted the fork-tree node count back into **fib-units**
(`score = min { n : Fib(n+1)-1 >= W }`) so `fib(n)` scored `n`. That
mis-ranks every other shape and ties policy to one recurrence.

`par_work_grain(sites, f, args)` counts the **fork-site nodes** reachable
from a concrete arg vector and uses that count **directly**:

```
W(f, args) = 0                                  if args miss f's guards, go
                                                negative, or f has no fork site
           = 1 + Σ_arms W(callee, arm(args))    otherwise
fork iff W > COIL_PAR_THRESHOLD
```

Guard pruning is what makes this a work model rather than a size model: a child
that fails the site's path conditions is a base case and contributes nothing.
Arms into *other* pure functions recurse into that function's own site, so heavy
helper arms count and trivial ones do not. The walk is memoized per arg vector
and bounded by a depth cap, a memo-entry cap, and saturation one node past the
grain floor.

The default floor **10945** is a profitability constant in **grain** (nodes),
not “fib(n)”. It is `W(fib(20))` for the `n <= 1` recurrence
(`Fib(21) - 1`): the same spawn-profitability point previously written as
fib-unit 20. `fib(20)` still refuses, `fib(21)` still forks. The fair
`tak(18, 12, 6)` load is **8398** grain — below that floor (fib-units used
to round it up to 20). It stays sequential.

| Site | `max(args)` | Grain `W` | Verdict (default floor) | Real calls |
|---|---|---|---|---|
| `fib(21)` | 21 | 17710 | fork | 35 421 |
| `fib(20)` | 20 | 10945 | refuse | 21 891 |
| `tak(18, 12, 6)` (fair bench) | 18 | 8398 | refuse | 63 609 |
| `tak(21, 12, 6)` | 21 | >10945 | fork | 230 613 |
| `tak(24, 22, 20)` | 24 | 53-scale | refuse | 53 |
| `sq(n) + sq(n - 1)` at 22 | 22 | 1 | refuse | 2 |
| `fib(n) + fib(n - 1)` at 22 | 22 | >10945 | fork | 92 734 |

Every imprecision resolves *downwards* — an arm into a function with no fork
site, a `SelfCall` combine's re-entry on joined values (unknowable statically),
the caps — so `W` is a lower bound on the tree and unknown structure can
only make a site refuse. `tak` is the interesting case: its arms rotate
parameters, so a large component stays alive, but many children miss the `y < x`
guard and the combine's re-entry is invisible. The fair benchmark
load lands **below** the floor (`W = 8398`) and stays sequential; only a genuinely deeper
tree crosses it.

The default is a profitability floor, not an arbitrary gate: forking below
it (e.g. `COIL_PAR_THRESHOLD=100` on `fib(32)` or `tak(18,12,6)`) multiplies
reactor spawn/join work and is typically **slower** than sequential, and very
low values can exhaust the specialization budget or overflow worker stacks.
Raise the workload (larger const args) when you want IPA evidence; do not lower
the grain floor to “force” more forks. Old fib-unit `N` corresponds to grain
`Fib(N+1)-1` (for `n <= 1` fib). One worker per site also means lowering the
floor cannot explode archive size via extra clones.

## Loop IPA: chunked fork-join over an induction range

A counted loop is the same idea with the arms spread over an induction range.
When the iterations only communicate through one **associative** reduction, any
partition of the range folds to the sequential result, so the range splits into
contiguous chunks that each accumulate a private partial.

[`loop_par`](../../compiler/src/typechecking/loop_par.rs) admits a counted loop
only when **every** gate holds:

| Gate | Requirement |
|---|---|
| Shape | `while i < K` / `i <= K`, or counted `for x in START..END` / `..=` (Q6 literal), or `for x in r` when `r` is a const range local (B5) |
| Induction | `while`: exactly one `i = i + 1` / `i += 1` / `i++` on a const-initialized local. `for`: the binding is the IV; the Q6 `+ 1` latch is implicit (no extra step in the body) |
| Trip count | compile-time `[begin, end)` with `end - begin > COIL_LOOP_GRAIN` (default **20**; trip-count grain — same spawn-floor idea as expression `W`, different unit) |
| Reduction | exactly one `acc = acc ⊕ e` / `acc = e ⊕ acc` / `acc ⊕= e` for associative `⊕` (`+` / `*` / `^`) on a const-initialized local |
| Independence | `e` never reads `acc`; the body reads only the IV, its own `let` temps, int literals, and enclosing **const-int** locals (those become worker immediates) |
| Purity | body calls only pure user functions; no index / field / static writes, no branches, `break`, `return` or `yield` |
| Types | the induction variable and `e` both infer to `int` — float reduction is not associative |

Ranges are normalized half-open (`i <= K` and `..=` become `end = K + 1`), so a
split is just a partition of `[begin, end)`. Dynamic `for x in 0..n` / C2
parameter ranges stay sequential (same refusal as `while i < n`).

Codegen emits one private **chunk worker** per site,
`__coil_par_loop_{n}(lo, hi, acc)`, holding the original body over `[lo, hi)` and
returning the partial. For `for`, the worker emits the unit step after the body.
At the loop site:

1. `MakeFn` the worker, then `thread_spawn_shared(worker, mid, end, identity)`
   (HostInvoke **137**; falls back to isolate `thread_spawn` when maps are
   missing, `COIL_SHARED_HEAP=0`, or an arg misses the C1 whitelist) — the
   upper chunk starts from the operator's identity (`0` for `+`/`^`, `1` for `*`) so
   the accumulator's initial value is counted exactly once.
2. Call the worker inline for `[begin, mid)` seeded with the live `acc`.
3. `thread_join` (help-steals), then fold the two partials with `ADD` / `MUL` / `XOR`.
4. Store the fold into `acc` and set the IV to `end`, the value the sequential loop
   would have left behind.

On a failed spawn or join, a single worker call covers `[begin, end)`.

Array / dict / coro / user-`Iterator` `for` stays sequential: isolate IPA does not
send the heap collection (that is C1 shared-heap steal, [COI-365](https://linear.app/ardax/issue/COI-365)).

## F2 admission vs still sequential

Admitted under the same purity / independence / grain gates (no allowlists,
no new floors):

| Newly admits | Why it is sound |
|---|---|
| `f(a)+g(b)+h(c)` (and `*` / `^`) | `int` `+`/`*`/`^` are associative; one fork, N arms; hit bench `triple_fib` |
| `let a = f(…); let b = g(…); return a ⊕ b` | same independent calls; unused/intervening stmts refuse |
| `walk(n-1, k) + walk(n-1, k+1)` | `ParamPlus` is the same structural arg form as `ParamMinus` |
| `acc = e + acc` / `acc = acc ^ e` | commutative/associative `int` fold; xor identity `0` |
| `acc = acc + scale * f(i)` with `let scale = 3` | const int is an immediate in the chunk worker, not a live capture |

Still refuses (and why):

| Still sequential | Why |
|---|---|
| `a - b - c` as three arms | subtraction is not associative |
| `a + a` from one let | one value, not two calls |
| `f(n) + (n - 2)` mixed operand | combine needs every leaf to be a pure call |
| `n % 2 == 0` path guard | unevaluable → `Opaque`, never specialize |
| `while i < n` / `for x in 0..n` | dynamic trip count would be a runtime grain tax |
| `acc = acc + k` for parameter `k` | worker frame has no live outer slots |
| `min` / `max` / user operators, float reduce | not proven associative here |
| `i += 2`, countdown `i = i - 1` | first slice keeps unit-step `[begin, end)` |
| conditionals / `break` / heap writes in the body | independence unproven |
| array / dict / coro `for` | sendability (C1 leftover), not F2 |

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
| `COIL_PAR_THRESHOLD` | Expression IPA grain floor (fork-tree nodes `W`). Default **10945**. |
| `COIL_LOOP_GRAIN` | Counted-loop IPA trip-count floor. Default **20**. |
| `COIL_SHARED_HEAP` | `0` / `false` / `off` / `no` forces isolate `PortableValue` spawn for loop chunks and expression IPA (C1/C2 off). Default on. |

`.hyc` / embed execute sizes each isolate operand stack from the persisted
compiler bound (archive minor 13). Pre-13 archives still use the Seek+CALL
heuristic; that is not an IPA policy change. Minor 14 stores S2b maps so
archive/embed GC relocate matches compile-and-run; older maps stay empty.
Minor 15 appends HostInvoke `thread_spawn_shared` (**137**) for C1 loop-chunk
steal. Pre-15 archives never emit that id.

Pool workers pin a TLS local deque tagged with the owning reactor identity.
`submit` / join-help only push or pop that deque when it belongs to the same
reactor; otherwise work goes through the shared injector. That keeps concurrent
`Machine`s (parallel tests) and nested reactors from cross-feeding jobs.

Isolate-per-job tax (COI-360 E2): workers execute from the `Arc`
`ThreadProgram` image; join-help checks out a TLS helper `Machine`; after an
**isolate** job the private heap is reset (unmap when more than one 64KiB slab
is mapped). User `thread::spawn` stays on this path.

C1 shared-heap loop steal (COI-365 E6): counted-loop chunks submit
`SpawnArg::Shared` `Value` bits onto one Heap (helpers **bind** that Heap;
they do not `reset_isolate_heap` unmap it). Layer A epoch STW: no collect
during the steal; a stolen chunk that would GC aborts to sequential /
isolate fallback; the joiner collects after `end_steal`. Maps are mandatory
for non-immediate args (empty maps → isolate). See
[shared-heap-sendability.md](shared-heap-sendability.md).

C2 expression IPA (COI-364 E7 / COI-366 F1): parameterized AlwaysPar
workers emit the same `thread_spawn_shared`. Fib/tak args are immediates
(Layer A may steal without maps). EnumCtor/Tuple arms allocate on the shared
Heap and publish the pointer at join (rooted through Layer A collect). User
`thread::spawn` stays isolate.

## F3 — userland locks + hints (call-bag escapes)

Unlocked FD / FFI / mutex / user-object escapes still **refuse** IPA (C0).
When the shape is a would-be call bag, compile-time **E0805** info names
**every** lockable edge on the bag (FD / FFI / mutex / heap object), not only
`stdout`. The compiler does
not insert locks or fork on the hint. A covering lock is detected and
documented; shared steal stays sequential this cut. See
[par-lock-hints.md](par-lock-hints.md) (Architect may discard).

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
the AST and works out each function's
[`EffectFlags`](../../common/src/effects.rs). Only the checker's length
proofs (`index_facts`) read the result, as the set of functions that cannot
change an array's length; auto-par, the E0805 hints and codegen read the
HIR summaries (`hir::effects`). The walk's rules:

- A function is **locally impure** if it uses `panic` / `yield` / FFI / `defer`,
  mutates via index/field assignment, or calls a non-identifier callee.
- Calls to names that are not user `fn`s (e.g. imported `write_all`, `spawn`,
  `collect`, `attach`) take the matching effect bit (unknown names are impure).
- An operator on a class, an enum or a type parameter (`a < b`, `-a`,
  `a += b`) runs the type's trait impl, and a `for … in` over anything but
  an array, a tuple or a range runs an iterator's `next` or resumes a
  generator. Both are unknown code that may resize (#2388); the checker's
  types say which operands are built-in. A body the checker never typed
  as written (one with an untyped rest parameter) counts as unknown too.
- Impurity propagates through the user-function call graph (fixed point).
- Everything that survives is pure, whether or not it calls itself. `$mono$`
  clones of a pure bind stay pure for LICM.

Codegen solves effects on the module's HIR
([`hir::effects`](../../compiler/src/hir/effects.rs), E1) and takes its pure
functions from there. A body's summary keeps its own effects apart from
the function parameters it calls, and each call site fills those in from
its arguments, so `map(xs, fn (int x) => x * 2)` is pure while the same call
with a lambda that writes a static is not. A lambda, a named `fn` or a
`let f = fn …` never reassigned is a known callee; a function value from
anywhere else is unknown. A `Vec` built in the function (`Vec::new()`, a
literal) that only leaves it by `return` may be pushed to without
`HEAP_MUT`. Summaries are kept across modules, so a stdlib higher-order
function is resolved from the caller's module. LICM and early CSE only get
the pure functions whose result is a scalar or `string`: two calls that each
return a fresh array are not the same value
([coil-lang#790](https://github.com/ardax-corp/coil-lang/issues/790)).

Codegen takes its pure and length-stable functions from the HIR summaries
alone; the AST walk above still feeds the checker's own length proofs
(`index_facts`), which run before any HIR exists, and the pipeline's
"may a `drop` resize" scan of every file. The HIR set names every function
and method body by its own name (`f`, `Owner::m`, `Trait for Owner::m`),
local and module-qualified, and a call matches by its whole name. To see
where the two analyses disagree on length stability, run a debug build with
`COIL_EFFECTS_SEED_DIFF=1`: one stderr line per function or method, with the
body's shape (`Function`, `Method`, `TraitImpl`, `Nested`), which side calls
it length-stable, and what it does in HIR.

A `static let` read is `HOST` and a write is `HEAP_MUT` in both analyses:
a chunk worker reads a copy of the statics taken when it was spawned, and
another call may change one
([coil-lang#793](https://github.com/ardax-corp/coil-lang/issues/793)). A
module `const` stays pure: one that folds is inlined, and one set at startup
is a slot nothing writes after it, which the worker's copy holds too
([const tables](const-tables.md)).

`coil dissect --effects` prints each function's effects with the first reason
for each, and why auto-par left a loop or a fork site sequential when purity
is all that stopped it. It reruns loop and fork-site detection as if every
user function were pure: a site found only then names the impure callee and
its first reason (`loop over `i` in `main` not parallelized: `step` calls
`write_all` (write, suspend)`). The editor hover (`coil-lsp`) shows the same
one-line effects for a function in the open file.

### Declared effects (E3)

`pure fn f()` and `fn f() uses {read, write}` (after `where`) are promises
the compiler checks; inference is unchanged and always runs. A summary keeps
`visible` effects apart from `flags`: panics, `defer`, GC, generator yields,
resizes and `ALLOC` (byte/string conversions that return a fresh object)
stay in `flags` for auto-par, LICM and CSE but are no effect a user sees or
declares. The vocabulary maps onto the bits in `hir::effects::VOCABULARY`:
`read` (`READ`, `HOST`: the clock and mutable statics), `write`, `net`,
`env`, `exec`, `ffi`, `thread`, `suspend` (`SUSPEND`, `ATTACH_PARK`) and
`mutate` (`HEAP_MUT`). Unknown effects cannot be declared. A declaration
covers the body's own effects, not the function parameters it calls.

`ModuleEffects::violations` reports E0413 (`EffectMismatch`) for a body whose
visible effects exceed its declaration, with the call chain that needs the
missing effect (`run → step → bump needs mutate: it writes static `HITS``).
A trait method's declaration bounds every impl (`Area for Sq::area` is
checked against `Area::area`) and is what a call through the trait costs
(`a.area()` or `Area::area(a)` on an existential): all traits with that method must
declare it, or the call stays unknown. Trait declarations are kept across
modules in `ProgramEffects`. Codegen checks every module. `coil-lsp` reports the
same errors for a well-typed file. Hover, `coil dissect --effects` and
auto-par reasons print effects in this vocabulary (`uses {read, mutate}`).

Expression IPA runs on **any pure** function whose body contains a fork site
(self-calls or independent helper calls). Loop IPA also needs pure body callees.

Disable both transforms with `--auto-par off`.

## Expression IPA: static profitability (no runtime threshold checks)

[`par_profit`](../../compiler/src/hir/par/par_profit.rs) detects **fork
sites** on pure functions — expressions whose operands are two or more
independent pure calls — and collects **constant** call-site arguments
(`fib(32)`, …). It walks the module's HIR as built
(`HirModule::par_fork_sites`, `par_worker_fns`), before the copy-on-write
rewrite and every pass, and keys a site by its function's bare name: that is
how arms name their callees and how the plan finds a call's worker. Combine
shapes recognized:

| Combine | Source shape |
|---|---|
| `BinOp` | `f(…) ⊕ g(…)` (`+` / `-` / `*` / `^`); a chain `a ⊕ b ⊕ c` of `+` / `*` / `^` is N arms, folded in the order written |
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

A worker stands in for a call of its function and returns the site's
combine, so a site is only specialized when the fork is all the function
does on that path ([coil-lang#2425](https://github.com/ardax-corp/coil-lang/issues/2425)):

- the fork is the operand of a `return`, not a part of another expression
  (`f(n - 1) + f(n - 2) + n`, a call's argument, a `let`'s value);
- nothing runs ahead of it but the path guards (`if … { return … }`) and its
  own `let`-bound arms: any other statement may trap, return or not
  terminate (`let d = 100 / (n - 27);`, a loop, an `if let`, `e?`), and so
  may a pending `let` arm a fork in a nested block does not name;
- the function has no contract check compiled in (`requires`, `ensures`, a
  loop's `invariant`, a class invariant): a worker would run none of them.
  At `-O2` that is any function with a `requires`; `--contracts off` lifts it.

Any other site is still found (the E0805 hints and `coil dissect --effects`
name it) and carries `ParGuard::Opaque`, which never specializes.

The walk looks at functions and inherent methods (not a trait's methods,
whose calls never name a worker; not tests or lambdas), and into statement
lists, `if` chains, `match` arms (an `if let` is one), loops, the operands
of `+ - * / % ^` and comparisons, call and constructor arguments; not
under `while let`, `&&` / `||`, an index or a field. A fork in a `match`
arm at or after a constructor pattern runs only when that pattern matched
or missed, so it is opaque. An arm is a bare call `f(…)`: a path (`m::f`,
`C::f`), a call through a trait (`Trait::m(x)`) and a variant named without
its enum are not. Parentheses change nothing: a group is the expression in
it. A const call under an `if let` demands its function's worker like one
anywhere else.

For each demanded constant call whose **fork-tree grain** `W`
exceeds `DEFAULT_EXPR_GRAIN` (default **10945**), and that still reaches the fork under the
site's path guards, codegen emits **one** parameterized worker
`__coil_par_{f}(args…, hop)` (COI-366 F1) that **always** forks:

1. Path guards and `hop <= 0` fall through to the sequential original (reachability
   / depth, not a grain skip-threshold).
2. `thread_spawn_shared` every arm this thread does not run itself, the
   last arm at least (HostInvoke **137**; isolate `thread_spawn` when an arg
   misses the C0 whitelist). No `GT` grain gate. A self-arm with `hop > 1`
   re-enters this worker with `hop - 1` and live `ArgForm` args; any other
   arm is its sequential callee.
3. Run the leading self-arms here, in order (same hop policy). These are
   the function's calls of itself that come before any other arm.
4. `join` each job in order (help-steals), then apply the site's combine.
   Shared join publishes raw `Value` bits (no graph copy).
5. On a failed spawn or join: join the jobs still out, then call the
   sequential original with the worker's arguments. Compile-time code takes
   this path: its `thread_spawn_shared` always fails.

Which thread runs an arm is what keeps a panic the sequential program's
([coil-lang#2424](https://github.com/ardax-corp/coil-lang/issues/2424)):

- A job is quiet (see [below](#a-failed-attempt-reports-nothing)), so an
  arm that panics there fails its join and nothing more. The worker then
  calls the function itself, which panics where the program does: at the
  first arm that fails, though a later one fails too.
- An arm this thread runs is not quiet, so it must be one whose panic is
  the program's as it stands. A leading self-arm is: every arm before it
  has returned, and its call is a call in the sequential program too (a
  function with a worker is never inlined, with auto-par on or off). An
  arm that calls another function is always a job: the sequential function
  may have that call inlined, and its frames would differ.
- The combine does not trap in the worker. An `int` `+` / `-` / `*` step is
  tested first (`hir::synth::int_overflows`, the loop fork-join's test) and
  the function runs instead of a step that would overflow. Steps are taken
  in the order the operands are written, since `(a + b) + c` can trap where
  `a + (b + c)` does not; a float sum rounds by its order too. A combine
  that calls another function (`ApplyCall`) is one more quiet job, joined
  at once. A combine that calls the function itself (`SelfCall`) is the
  function's last call, which runs in its frame.
- In a backtrace (`Locator::backtrace`), a worker's frame at an arm's call
  is printed as a frame of the function under it, at that call; its frame
  at the call of the sequential original is left out
  (`common::par_fork_fn`).

The worker is a HIR body the compiler builds itself
([`hir::synth`](../../compiler/src/hir/synth.rs),
`Compiler::par_worker_hir`): `thread_spawn_shared` / `thread_join` are host
calls, each spawned arm a function value, and each result `match`ed as a `Result`.
It lowers like any other body (HIR lowering, then direct MIR), with no HIR
passes or inlining and no par rewrite of the calls inside it. A site whose
parameters, arm results or combine are not one boxed word each, or whose
argument forms are not `int`, gets no worker.

A function that gets a worker is not inlined into its callers, and that
holds with `--auto-par off` too (`Compiler::fork_fns`): the sequential
program then has the frames the parallel one reruns into.

Const call sites rewrite to an ordinary call of that worker (plus
`PAR_SPEC_HOPS`, default **2**). Below-floor constant args stay on the
original sequential `f`. A **unary** call whose argument is a local
(`fib(n)`, not `fib(n - 1)`) compares that local with the cutoff — the
smallest `n` in `0..=48` whose grain clears the floor — and calls the same
worker when it does. The check is one
compare at that call. Calls inside the sequential function stay ordinary
calls, because the worker is registered after that function is emitted;
calls inside the worker stay ordinary too.
Hops are depth on
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
fork iff W > DEFAULT_EXPR_GRAIN
```

Guard pruning is what makes this a work model rather than a size model: a child
that fails the site's path conditions is a base case and contributes nothing.
Arms into *other* pure functions recurse into that function's own site, so heavy
helper arms count and trivial ones do not. The walk is memoized per arg vector
and bounded by a depth cap, a memo-entry cap, and saturation one node past the
grain floor.

The default **tight** floor **10945** is a profitability constant in **grain**
(nodes), not “fib(n)”. It is `W(fib(20))` for the `n <= 1` recurrence
(`Fib(21) - 1`): the same spawn-profitability point previously written as
fib-unit 20. Tight sites (const-guard on one parameter, `W` tracks sequential
work) still refuse `fib(20)` and fork `fib(21)`.

Sites whose `W` **undercounts** sequential work — `SelfCall` combine re-entry,
or a guard that relates two parameters (`tak` `y >= x`) — use a scaled **loose**
floor (default **8000**, `DEFAULT_EXPR_GRAIN * 8000 / 10945`). The fair
`tak(18, 12, 6)` load is **8398** grain: below the tight floor, above the loose
floor, so it forks.

| Site | `max(args)` | Grain `W` | Floor | Verdict | Real calls |
|---|---|---|---|---|---|
| `fib(21)` | 21 | 17710 | tight 10945 | fork | 35 421 |
| `fib(20)` | 20 | 10945 | tight 10945 | refuse | 21 891 |
| `tak(18, 12, 6)` (fair bench) | 18 | 8398 | loose 8000 | fork | 63 609 |
| `tak(21, 12, 6)` | 21 | >10945 | loose 8000 | fork | 230 613 |
| `tak(24, 22, 20)` | 24 | 53-scale | loose 8000 | refuse | 53 |
| `sq(n) + sq(n - 1)` at 22 | 22 | 1 | tight 10945 | refuse | 2 |
| `fib(n) + fib(n - 1)` at 22 | 22 | >10945 | tight 10945 | fork | 92 734 |

Every imprecision resolves *downwards* — an arm into a function with no fork
site, a `SelfCall` combine's re-entry on joined values (unknowable statically),
the caps — so `W` is a lower bound on the tree and unknown structure can
only make a site refuse. `tak` is the interesting case: its arms rotate
parameters, so a large component stays alive, but many children miss the `y < x`
guard and the combine's re-entry is invisible. The fair benchmark
load lands **below** the tight floor (`W = 8398`) and **above** the loose floor,
so it forks; a narrow tree such as `tak(24, 22, 20)` still refuses.

The tight floor is a profitability constant, not an arbitrary gate: forking a
tight fib-shaped tree below it (e.g. a floor of 100 on `fib(32)`)
multiplies reactor spawn/join work and is typically **slower** than sequential,
and very low values can exhaust the specialization budget or overflow worker
stacks. The loose floor exists because `W` is a lower bound on those shapes —
unknown structure still only refuses. Old fib-unit `N` corresponds to grain
`Fib(N+1)-1` (for `n <= 1` fib). One worker per site also means lowering the
floor cannot explode archive size via extra clones.

## Loop IPA: chunked fork-join over an induction range

A counted loop is the same idea with the arms spread over an induction range.
When the iterations only communicate through one **associative** reduction, any
partition of the range folds to the sequential result, so the range splits into
contiguous chunks that each accumulate a private partial.

[`loop_par`](../../compiler/src/hir/par/loop_par.rs) (`HirModule::loop_par_sites`)
walks the module's HIR as built, before any pass, and admits a counted loop
only when **every** gate holds:

| Gate | Requirement |
|---|---|
| Shape | `while i < K` / `i <= K`, or counted `for x in START..END` / `..=` (Q6 literal), or `for x in r` when `r` is a const range local (B5). `K` may be a local `const`; the index may be `_`. In any function or method body, a trait's included, and under `if let` / `while let` |
| Induction | `while`: exactly one `i = i + 1` / `i += 1` / `i++` on a const-initialized local. `for`: the binding is the IV; the Q6 `+ 1` latch is implicit (no extra step in the body) |
| Trip count | compile-time `[begin, end)` with iteration count `> DEFAULT_LOOP_GRAIN` (default **20**), or a dynamic int bound checked once at runtime |
| Reduction | exactly one `acc = acc ⊕ e` / `acc = e ⊕ acc` / `acc ⊕= e` for associative `⊕` (`+` / `*` / `^`) on a const-initialized local. A pure `if` is admitted when every arm uses that same fold |
| Independence | `e` never reads `acc`; the body reads only the IV, its own `let` temps, int literals, enclosing **const-int** locals (worker immediates), and enclosing **int** parameters/locals (extra worker args) |
| Purity | body calls only pure user functions; no index / field / static writes, no `break`, `return` or `yield`. Division and modulo need a non-zero literal divisor |
| Stride | unit `+ 1`, or a positive constant `i = i + k` |
| Types | the induction variable and `e` both infer to `int` — float reduction is not associative |
| Sign | `+`: every term proved `>= 0`, or every term proved `<= 0`. `*`: every term proved `>= 1` or `<= -2`. `^`: any term. Proved for every value, or under a test of the captures and bounds the fork-join makes as the loop starts. See [the sign rule](#the-sign-rule) |
| Range | an inclusive end read at run time (`i <= n`, `a..=n`) is proved below `MAX`, so the fork-join's `n + 1` is in range |

A site is keyed by its loop's source span and names its locals (index,
accumulator, captures, bounds), so the plan finds it again in a body the HIR
passes rewrote or inlined into another function.

Ranges are normalized half-open (`i <= K` and `..=` become `end = K + 1`), so a
split is just a partition of `[begin, end)`. A dynamic int bound (`while i < n`,
`for x in 0..n`) is the same partition with `lo` / `hi` loaded at runtime; the
grain compare runs once. Const ranges with wide mode on split into up to four
chunks (`trips / DEFAULT_LOOP_GRAIN`, clamped to 2..=4). Every chunk runs on
a worker and starts from the operator identity; the joiner folds them onto
`acc`.

Codegen emits one private **chunk worker** per site,
`__coil_par_loop_{n}(lo, hi, acc)`, holding the original body over `[lo, hi)` and
returning the partial. For `for`, the worker emits the unit step after the body.
The fork-join below is a second private function, `__coil_par_loop_fork_{n}(acc,
…live captures, begin, end)`, returning the new `acc`; the loop site is a
plain call of it. Both are emitted after the enclosing function, so neither
sits inside its code: the enclosing body and the worker (planned as its own
HIR body, `while lo < hi { body }; return acc`) each lower to MIR. After the
call, the enclosing body sets a `while` site's index, when it is read after
the loop, to the value the sequential loop leaves: a const range's last
value, and a dynamic range's first value at or past its end counting from
its start by the stride (`begin + trips * stride`; the start when the range
is empty).

A listed site is one the plan can build: the analysis holds its words (the
accumulator, a `while` index, the bounds, the live captures) to `int` locals
few enough for one spawn, and a `for` loop's checks to what its body may
read. The plan may still leave a site sequential when chunking gains nothing
(an iteration that never reads its index and repeats work LICM hoists). A
listed site it cannot build runs sequentially and is a bug on one side:
`Compiler::hir_par_disagreement` notes it, and the corpus HIR test
(`compiler/tests/hir_corpus.rs`) fails on one (#2450).
In the fork-join:

1. `MakeFn` the worker, then `thread_spawn_shared(worker, lo, hi, identity)` for
   each chunk (HostInvoke **137**; falls back to isolate `thread_spawn` when an arg
   misses the C1 whitelist).
   Each chunk starts from the operator's identity (`0` for `+`/`^`, `1` for `*`).
2. `thread_join` every chunk (help-steals), then fold `acc` and the partials in
   order with `ADD` / `MUL` / `XOR`.
3. Store the fold into `acc` and set the IV to `end`, the value the sequential loop
   would have left behind.

On a failed spawn or join, a single worker call covers `[begin, end)`.

Int overflow traps ([Int overflow](int-overflow.md)), and that shapes the
order. The joiner runs no chunk itself: a chunk that panics must not unwind
the joiner while other chunks still use its heap. A chunk that overflows
fails its join, so the loop reruns sequentially, which panics where the
program does, or finishes when only that chunk's partial overflowed. The
fold waits for the last join, and folding in order makes each step a prefix
of the sequential reduction, so a fold that overflows is one the sequential
loop performs too. The fork-join tests each `+` / `*` step before taking it
(compares against `MAX - p` / `MIN - p`, or the quotients `MAX / p` and
`MIN / p` by sign) and reruns the range instead of trapping, so that panic
too is raised by the loop's body.

A dynamic range's trip count is the fork-join's own arithmetic, which the
sequential loop never does. `hi - lo` is tested before it is taken, and so
is its rounding up to whole strides: a range longer than an `int` holds
runs on the calling thread, as one worker call. The runtime's lane
loops (`machine/src/lanes.rs`) count what is left of such a range as an
unsigned difference, so a vector loop runs it too
([coil-lang#2522](https://github.com/ardax-corp/coil-lang/issues/2522)).

### The sign rule

A trapping `+` or `*` is not associative: a chunk folds from the identity,
so it cannot see the running fold leave the `int` range part way through
the chunk and come back
([coil-lang#2423](https://github.com/ardax-corp/coil-lang/issues/2423)).
`total = total + step(i)` from `MAX - 7`, where `step` is `10` for ten
iterations and `-1` after, panics in the sequential program; each chunk's
partial fits, and so does the fold of the partials.

A loop is a site only when that cannot happen, which is a fact about the
terms alone:

- **`+`**, every term `>= 0` (or every term `<= 0`): the running sum only
  moves one way, so once it is past an end of the range it stays past it,
  and the whole fold is out of range too.
- **`*`**, every term `>= 1` or `<= -2`: a value out of range is at least
  `2^63` in magnitude and is not `-2^63`, and multiplied by such a term it
  stays so. A `0` brings a product back, and so does a `-1` (`2^63 * -1`
  is `MIN`), so neither is allowed. Terms that are only proved `>= 0` do
  not qualify.
- **`^`** never traps.

Under the rule the checks that were already there are exact. If some
running value of the sequential fold is out of range, the whole fold is;
the chunks' partials and the fork-join's fold of them compute that whole
fold with every step tested, so one of them fails and the range reruns
sequentially, which panics where the program does. If no running value is
out of range, a chunk's partial may still be (a product from `1` where the
loop starts at `0`); that reruns too, and finishes. The chunk worker is
the plain loop: the rule costs nothing at run time, and needs no trip
count, so a dynamic range qualifies.

A loop the rule refuses runs sequentially. `coil dissect --effects` names
it (``loop over `i` in `run` not parallelized: the terms of its `+` fold
onto `total` are not proved all `>= 0` or all `<= 0```).

The proof is [`hir::par::sign`](../../compiler/src/hir/par/sign.rs)
(`Signs`): an interval evaluation of the HIR as built. `int` arithmetic
traps on overflow, so an operator's range is the exact arithmetic on its
operands' ranges, cut to the `int` range. What it reads:

| Source | Range |
|---|---|
| a literal; `x * x` | itself; not negative |
| `+ - * / % ** & \| ^ << >>`, unary `-` and `!` | by the operator (`%` takes its dividend's sign, a shift count is `0..=63`, `<<` only where no bit is shifted out) |
| the index | its range: the literal bounds, and for a bound read at run time what is known of that local. A `while` index is also read after its step |
| a `let` nothing writes again | its initializer's range |
| a `let` assigned later (in a callee) | the join of every value written to it, widened to a sign after two rounds (`s = s + i * i` from `0` is `>= 0`) |
| a parameter or capture | anything, unless a condition or [a test](#a-test-of-the-captures) narrows it |
| `if c { … }`, `if c { return … }` / `if c { panic … }` ahead, a `match` on literals, a `for` loop's `invariant` | `c` narrows the locals nothing writes (`x < 0`, `x >= k`, `&&`, a failed `\|\|`) |
| a `byte` value; structural `len()`; a node the checker proved non-negative | `0..=255`; `>= 0`; `>= 0` |
| a module `const` (not a `static let`); an element of a `readonly` array literal a `const` names | its initializer's range; the join of the elements. A module `const` may also read `0`: one set at startup is a slot, which code run ahead of its initializer sees empty |
| a call of a function of the module | the range of what its body returns for the ranges of the arguments (`weight(i)` with `i >= 0` for `fn weight(int i) { return i % 7; }`) |
| anything else (a field, an element of a mutable array, a call through a trait or into another module, a recursive call) | anything |

Unknown means not proved. What it does not prove today, by what stops it
in the corpus and the examples:

- a term of either sign whatever the captures (`Result::Err(_) => -1`,
  `i - 50`), or one whose sign depends on the captures in a way the
  tests above do not express (`i - lo` from `lo`: the analysis does not
  relate two values);
- a value read from an object or a mutable array (`n.v + m.v`, `xs[0]`);
- a range a function returns (`for x in make_range(n)`);
- a function that calls itself (`even` / `odd`).

#### A test of the captures

The sign is often the caller's: `s + i * c` with `c` a parameter,
`r * base`, an index from a start read at run time (`for x in lo..hi`
with `acc + x % 7`). Nothing in the loop writes those values (the
captures, the bounds, a `while` index's start), so they are the same in
every iteration of a run, and the proof can be made for each of their
values. `Scan::sign_guard` (`hir/par/loop_par.rs`) narrows each one, the
bounds first and then the captures by name (at most 4), and evaluates the
fold again with `Signs`:

1. A split of one of them, `v >= t` against `v <= t - 1` for `t` in
   `0, 1, -1`, where both halves sign the fold (each its own way): every
   run has one sign. No test: `i * c` with `i >= 0` has the sign of `c`.
2. Else one test of one of them, `v >= k` (`k` in `0, 1, 2`) or `v <= k`
   (`0, -1, -2`), weakest first: `base >= 1` for `r * base` (`0` and `-1`
   are not product terms), `lo >= 0` for `x % 7` from `lo`.
3. Else the same test of all of them, then without each one the others
   do without: `j >= 0` and `k >= 0` for `j * k + i`.

A test no value passes is never taken, and at most 48 narrowings are
evaluated per loop. The site carries the tests (`LoopParSite::guard`).
The fork-join makes them once, ahead of everything else (for a dynamic
range, once `lo` and `hi` are read): when one fails, the whole range runs
on the calling thread as one worker call, the same path as a failed
spawn, so the loop prints, panics and returns as the sequential one. One
compare per test per entry to the loop, none per iteration. `coil
dissect --effects` names such a loop (``loop over `_` in `power`
parallel only when `base >= 1`: otherwise the terms of its `*` fold onto
`r` may be `0` or `-1```).

### A failed attempt reports nothing

A parallel attempt is speculative: what the program prints and how it exits
are the sequential program's ([coil-lang#2419](https://github.com/ardax-corp/coil-lang/issues/2419)).

- A job spawned with `thread_spawn_shared` (only fork-joins call it) is
  marked `Job::speculative`, also when it falls back to an isolate job. Its
  driver runs quiet (`Driver::set_quiet`): a panic, a failed contract
  included, stops the job and fails its join without a report. A thread the
  program spawns itself (`thread::spawn`) is not speculative and reports as
  before.
- The rerun's panic is the program's. Its backtrace
  (`Locator::backtrace`, `machine/src/locate.rs`) leaves out the loop's two
  compiler-made functions, named by `common::par_loop_fn`: the chunk
  worker's frame is printed as a frame of the function the loop is written
  in, at the worker's location, in place of that function's call into the
  fork-join.

Expression IPA follows the same rule; its worker is described above. An
arm the worker runs on its own thread can panic while later arms are still
jobs, which unwinds the worker past its joins. The run that panicked
cancels and joins those jobs itself, once the panic is reported
(`Driver::settle_speculative` over `Runtime::speculative_out`, which every
speculative spawn is added to): no job runs on while the heap is used or
dropped under it, and the steal epoch closes. That holds for a panic a
task scope catches and for a quiet job's panic too.

### Cancelling a failed attempt

A failed attempt's jobs are not waited for
([coil-lang#2442](https://github.com/ardax-corp/coil-lang/issues/2442)):
when an arm or chunk fails, the work after it is cancelled, so the
program reports about when the sequential one does, and not never when a
later arm does not terminate.

- **What is cancelled.** A job that fails (`worker::run_on`, before it
  stores its result, so before its fork-join can go past the join)
  cancels the speculative jobs its spawner added after it and has not
  joined (`JoinState::cancel_later`): the later arms or chunks of the same
  fork-join, and whatever the spawner forked since in the self-arms it
  runs itself. A fork-join joins every job it spawns before it returns,
  so none of those belongs to another, and the sequential rerun replaces
  all of them. Until the failed job is joined, a job its spawner adds is
  cancelled as it is added (the spawner may not be done spawning that
  fork-join's arms). The jobs ahead of it are an enclosing fork-join's and
  run on. A panic on the spawner's own thread cancels every job it has out
  (`settle_speculative`). Cancelling a job cancels the jobs it spawned
  (`Cancel::children`).
- **Only speculative jobs.** A `JoinState` made by `thread_spawn_shared`
  (and its isolate fallback) has a `Cancel`; one made for `thread::spawn`
  has none, and nothing cancels it.
- **How it stops, at no cost to its code.** A job not started yet does not
  start. Native code compares the stack pointer with
  `NativeCtx::stack_limit` at every function entry: each native call a
  speculative job runs (`native::run_call`) is listed under the job
  (`CallGuard`), and a cancel stores `CANCELLED_STACK_LIMIT` into each
  listed limit, so the job traps at its next call. The interpreter
  charges fuel at every call and taken back-edge: a speculative job runs
  on slices of `FUEL_SLICE` (4096) steps, and when a slice runs out
  (`coil_native_refuel`) it gets another unless it was cancelled. The
  store comes from another thread, so it is atomic, and the job reads it
  where it already reads the limit or the fuel.
- **What it leaves.** A cancelled job unwinds as a quiet panic does and
  fails its join with no message. A speculative job runs a pure body (and
  the compiler-made fork-joins in it), which takes no lock and has no
  `defer`; its frames' heap words are stack-map roots that go with the
  frames, and its allocations are on the steal epoch's heap like those of
  a job that panics. A job it had out is cancelled and joined by its own
  settle.

- **A loop with no call.** A native loop may run without a function
  entry (`spin` in the issue's repro: `while n > 0 { … }` that never
  ends). One that may run forever, in a body a speculative job may run,
  polls the stack limit at its header as an entry does, a load, a compare
  and a branch per trip. The bodies are those the function values a
  fork-join spawns name, through their calls, function values and code
  pointers; once one calls a value or the host, every body whose code is
  taken anywhere, and once one allocates, every `drop`
  (`coil-native`'s `speculative_reach`). Other code pays nothing. The
  loops are those with no `Call` or `CallIndirect` and no exit test on a
  counter (`mir::spin_headers`). A counted loop ends on its own and is not
  polled: an exit test, on a block every trip runs, of a basic induction
  variable (or its stepped value) against a value from outside the loop,
  the way its step runs toward (`i < n` with `i` climbing). A step's sign
  is known for a non-zero constant and for an enclosing loop's counter
  that starts at a constant of the sign and steps away from zero (the
  sieve's `k = k + p`). The poll is an atomic load: a plain one, with no
  store in the loop, is the entry's load again to Cranelift's alias
  analysis. A counted loop still runs to its end once cancelled, so a
  long one delays the report until it ends (a later arm cancelled while
  in it stops at its next call).

Array / dict / coro / user-`Iterator` `for` stays sequential: isolate IPA does not
send the heap collection (that is C1 shared-heap steal, COI-365).

## F2 admission vs still sequential

Admitted under the same purity / independence / grain gates (no allowlists,
no new floors):

| Newly admits | Why it is sound |
|---|---|
| `f(a)+g(b)+h(c)` (and `*` / `^`) | one fork, N arms, folded left to right as the function does; hit bench `triple_fib` |
| `let a = f(…); let b = g(…); return a ⊕ b` | same independent calls; unused/intervening stmts refuse |
| `walk(n-1, k) + walk(n-1, k+1)` | `ParamPlus` is the same structural arg form as `ParamMinus` |
| `acc = e + acc` / `acc = acc ^ e` | commutative/associative `int` fold; xor identity `0` |
| `acc = acc + scale * f(i)` with `let scale = 3` | const int is an immediate in the chunk worker, not a live capture |

Still refuses (and why):

| Still sequential | Why |
|---|---|
| `a - b - c` as three arms | subtraction is not associative |
| `f(a) + (g(b) + h(c))` as three arms | the function adds in that order, and `+` traps on overflow: only a chain written without parentheses is folded (#2424) |
| `a + a` from one let | one value, not two calls |
| `f(n) + (n - 2)` mixed operand | combine needs every leaf to be a pure call |
| `f(n - 1) + f(n - 2) + n`, `g(f(n - 1) + f(n - 2))` | the fork is not all the function returns: the worker would drop the rest (#2425) |
| a statement ahead of the fork that is no guard and no arm | the worker would skip it (#2425) |
| a function with a contract check compiled in | the worker would not run it (#2425) |
| `n % 2 == 0` path guard | unevaluable → `Opaque`, never specialize |
| `acc = acc + k` for a non-int `k` | only int parameters and int locals are worker arguments |
| a `byte` accumulator, index, bound or capture | the fork-join's words are `int` locals (#2450) |
| more than 13 live captures | the worker's arguments fit one thread spawn |
| `min` / `max` / user operators, float reduce | not proven associative here |
| `i = i - 1` countdown | the split walks a positive stride |
| `break` / heap writes in the body | independence unproven |
| `if` arms that use different folds | one associative operator per loop |
| a `+` fold whose terms are not proved to have one sign; a `*` fold with a term that may be `0` or `-1`, also under [a test of the captures](#a-test-of-the-captures) | a chunk cannot see the running fold leave the range and come back (#2423, [the sign rule](#the-sign-rule)) |
| `i <= n` / `a..=n` where `n` may be `MAX` | the fork-join adds one to the end; the sequential loop does not (#2423) |
| a `for` under an `invariant` / `decreases` that reads a local the loop writes (the accumulator) | the clause is about the sequential state at that iteration; a chunk's fold starts at the identity (#2412). A clause over values the loop leaves alone is the same test in every chunk and stays, when it reads and calls only what a body may |
| a `while` under any `invariant` / `decreases` | it runs its checks ahead of every test, the one that ends it too, and once when it runs no trips; no chunk worker does (#2450) |
| array / dict / coro `for` | sendability (C1 leftover), not F2 |
| a fork in a trait's method (an instance's or a default) | a site is keyed by its function's bare name and its worker is reached from a bare call; a trait's method is never called that way |
| a fork in an arm of an `if let`, or of a `match` at or after a constructor pattern | the arm runs only when the pattern matched or missed, which a worker's guards (comparisons of `int` parameters) cannot test: found, never specialized |

## Work-stealing reactor

[`machine/src/reactor.rs`](../../machine/src/reactor.rs) owns a fixed pool of OS
threads (size = [`WorkerCap`](../../machine/src/thread.rs), default
`available_parallelism`). Jobs land on a crossbeam injector / local deques;
idle workers steal. `thread::spawn` / auto-par share this pool — no per-call
`std::thread::spawn`. Join help-steals, then parks on the reactor condvar until
the `JoinState` result is ready **or** `submit` / job-complete `notify` (COI-390);
there is no 1 ms poll. Idle workers park on the same `sleep_cvar` until `notify`
(submit / job-complete / shutdown) — no 2 ms empty-steal poll (COI-391).

| Switch | Effect |
|--------|--------|
| `COIL_MAX_WORKER_THREADS` | Pool size (1..=512). Default `available_parallelism` (min 2). `.cargo/config.toml` sets this to `1` (`force = false`) for cargo test runs. Export a higher value to profile parallelism. |
| `--auto-par on\|off` | `off` compiles no auto fork-join (default `on`). A program prints, reports and exits the same either way: `scripts/auto_par_diff.py` checks that on every program with a `main` ([test-runner](test-runner.md#configuration-cells-and-known-failures)). |
| `COIL_PAR_STATS` | Unit tests of `machine` only: `1` / `true` / `on` / `yes` prints reactor steal/idle/join counters on shutdown (see [auto-par-branch-misses.md](auto-par-branch-misses.md)). Other builds never count. |

Shared-heap spawn is always on. An argument outside the whitelist falls back to isolate `PortableValue` spawn.

Pool workers pin a TLS local deque tagged with the owning reactor identity.
`submit` / join-help only push or pop that deque when it belongs to the same
reactor; otherwise work goes through the shared injector. That keeps concurrent
runtimes (parallel tests) and nested reactors from cross-feeding jobs.

Isolate-per-job tax (COI-360 E2): workers execute from the `Arc`
`ThreadProgram` image; join-help checks out a TLS `Runtime`
(`machine/src/worker.rs`); after an **isolate** job the private heap is reset
(`Runtime::reset_isolate`). User `thread::spawn` stays on this path.

C1 shared-heap loop steal (COI-365 E6): counted-loop chunks submit
`SpawnArg::Shared` `Value` bits onto one Heap (helpers **bind** that Heap;
they do not reset it). Layer A epoch STW: no collect
during the steal; a stolen chunk that outgrows the epoch GC ceiling aborts to sequential /
isolate fallback; the joiner collects after `end_steal`. See
[shared-heap-sendability.md](shared-heap-sendability.md).

C2 expression IPA (COI-364 E7 / COI-366 F1): parameterized AlwaysPar
workers emit the same `thread_spawn_shared`. Fib/tak args are immediates.
EnumCtor/Tuple arms allocate on the shared
Heap and publish the pointer at join (rooted through Layer A collect). User
`thread::spawn` stays isolate.

## F3 — userland locks + hints (call-bag escapes)

Unlocked FD / FFI / mutex / user-object escapes still **refuse** IPA (C0).
When the shape is a would-be call bag, compile-time **E0805** info names
**every** lockable edge on the bag (FD / FFI / mutex / heap object), not only
`stdout`. The compiler does
not insert locks or fork on the hint. A covering lock is detected and
documented; shared steal stays sequential this cut. The hint describes the
source, so a run prints it with `--auto-par off` too (stderr is the same
either way, #2551). See
[par-lock-hints.md](par-lock-hints.md) (Architect may discard).

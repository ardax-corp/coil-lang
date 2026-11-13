# Optimizer hints: `#[inline]`, `#[cold]`, `#[hot]`

Function attributes steer the optimizers where they cannot tell on their own. Each changes speed only: a program behaves the same with or without them. The compiler infers cold code where the program makes it clear. It never infers hot, and nothing here measures a run (no PGO, #301).

Branch: `feature/opt-hints`, one PR per step. Plan agreed 2026-11-02.

## Surface

On a function or a method (inherent, trait impl or trait default). Limits are multiples of the level's inline cost limit (`max_inline_cost`; the cross-module one likewise), and the multiples depend on the level too (`OptLevel::hint_factors`). The table quotes `-O2`:

| Level | Base limit | Hot by spreading | `#[inline]` | `#[hot]` |
|-------|------------|------------------|-------------|----------|
| `-O1` | 25 | 2x (50) | 3x (75) | 5x (125) |
| `-O2` | 100 | 2x (200) | 3x (300) | 5x (500) |
| `-O3` | 200 | 4x (800) | 5x (1000) | 10x (2000) |
| `-Os` | 40 | 1x (40) | 2x (80) | 3x (120) |

`-O0` and `-Og` inline nothing, so only `#[inline(always)]` splices there.


| Attribute | Meaning | What the compiler does |
|-----------|---------|------------------------|
| `#[inline]` | Inline this function where it is worth it | Best effort: a 3x cost limit (300). The long-loop refusals still hold, and nothing is reported when it stays a call |
| `#[inline(always)]` | Splice this function in wherever it is called directly | No cost limit, loop bodies included. It refuses only what cannot be inlined (recursion, coroutines, function values), with a warning that says why (**E0807** `InlineRefused`) |
| `#[inline(never)]` | Keep calls to this function | Never inlined, even from a hot body; unlike `#[cold]` its calls are not laid out after the body or cut out. For code size, stack traces and benchmarks |
| `#[cold]` | Calls to this function are rare | Never inlined. A branch that calls it is laid out after the body, and is cut out into a helper when its caller is inlined or specialized (partial inlining) |
| `#[hot]` | This function is on a hot path | Always gets a per-type clone, outside `--clone-budget`, and inlines under a 5x limit (500). Calls inside it use a 2x limit (200). Its direct callees and the other methods of its `impl` block are treated as hot too, at 2x |

`#[cold]` cannot be combined with `#[inline]`, `#[inline(always)]` or `#[hot]`, and `#[inline(never)]` with any of those three (**E0806** `ConflictingOptHints`); `#[cold]` with `#[inline(never)]` is allowed. A hint on a class or enum, or with any other argument, is E0119.

### Precedence

Highest first:

1. A tag on the item itself: `#[inline(never)]`, then `#[cold]`, `#[inline(always)]`, `#[hot]`, `#[inline]`. `#[cold]` on a callee of a hot item keeps it cold.
2. Hot inherited from a hot caller (direct callees only) or a hot `impl` sibling.
3. Inference.

Inherited hot outranks inferred cold, except on a diverging function: specializing code that only panics gains nothing.

### Inference

Cold is inferred only from code that is rare in every program:

- **Diverging functions.** A function whose every path ends in `panic`, `exit` or a failed contract is cold.
- **Failure arms.** An arm that ends in `panic`, `raise` or a failed `assert` is cold, with or without statements before the end.
- **Calls to cold functions.** A branch that calls a cold function, tagged or inferred, is cold.

There are no growth guesses: "the grow path is rare" holds for a `Vec` push but not for every program, so the stdlib tags it instead.

## Where it lives

| Piece | Where |
|-------|-------|
| Placement, arguments, E0806 | `compiler/src/attrs.rs` (`check_opt_hints`) |
| The hints of a body | `HirBody::hints` (`hir::OptHints`), set in `hir::build`, kept on mono clones |
| Functions that never return, failing arms | `hir::cold` (`fails`, `diverging_bodies`) |
| Cold functions of the program | `Compiler::cold_fns` (by table key, filled as each module's HIR is built: `#[cold]` or never returning, `exit` counted), `cold_labels` (their entries, recorded in `named_entry_target`) |
| Hot functions | `Compiler::hot_fns` (by table key, filled as each module's HIR is built, `note_hot_fns`): `#[hot]` functions and methods, the other methods of a `#[hot]` method's class, and what a hot body calls by name (`f(..)`, or `x.m(..)` naming a method of its own class), less cold ones. `is_hot_body` / `is_hot_fn` (clones by their base name; an explicit `#[cold]` wins) |
| Hot clones | Free generic functions: `monomorphize::mark_hot` (`#[hot]`, and generic functions a `#[hot]` one calls by name) makes each ground call's clone a must, past `--clone-budget`, as keyed ones are. Generic class methods: `instance_clone` / `method_clone_room` let a hot method's clone past the budget, into `HOT_CLONE_ROOM` (256) extra entry labels per program |
| Inliners | `Compiler::hir_inline_round`: `factor` picks a callee's multiple of the level's limit from `InlineCostOptions::hint_factors` (`OptLevel::hint_factors`: `hot` for `#[hot]`, `inline` for `#[inline]`, `spread` for a callee hot by spreading and a hot body's own calls); `budget_of` / `over_cross` apply it same- and cross-module and `#[inline(always)]` lifts it, with the long-loop and invariant-loop refusals; `callee_of` refuses an `#[inline(never)]` or cold callee; `note_inline_refused` warns E0807 once per `#[inline(always)]` function of the module that stays a call |
| Partial inlining | `hir::outline` (`outline`, `cut`): a statement `if` arm that is plain (no loop, closure, `defer`, `break` / `continue`), has no `return`, holds at least `MIN_NODES` (4) nodes and fails or calls a cold function becomes a call of a helper `{callee}$cold{k}`. Its free locals are the parameters; it may assign one of them by name, which the helper returns (`x = helper(..)`). An arm that writes through a free local (a field, an element, or a copy-on-write `unique` before a mutating call), borrows or captures one, or holds a fixed array, tuple, record or open type keeps its place. The helper is `#[cold]`. `Compiler::hir_outline_cold` cuts the callee at the call (once per site, the call's span on a foreign callee's nodes), checks the helper's arguments and result are words and registers its signature (`outlined`), which `resolve_hir_callee` answers for its calls; the replan's `fit` tries the cut when the whole callee is over its cost limit or holds a panic (a `construct` `inline::inlinable` refuses), same- and cross-module. `outlined_plan` plans each helper as its own function before the caller's replan is accepted, else that callee is not cut again (`outline_refused`); `queue_outlined` / `flush_outlined` emit the helpers a body calls once each, after the body, lowered to direct MIR as a par worker's body is |
| Layout | `LowerCx::cold_target` on each call site's entry; `mir::from_hir` `cold_arm` / `calls_cold` mark the arm's blocks cold, `layout` puts them after the body |

### Layout

An `if` or `match` arm is laid out after the body when it only returns or fails (a panic, after any statements), or when it calls a cold function, even one it falls through from. A branch whose one edge carries φ copies runs them inline on that edge and jumps away on the other (`mir::emit`, `Terminator::Br`), so the hot edge takes no jump to reach its copies.

Hit benches (callgrind, release; step 2 rows for layout, step 3 for `#[inline]`):

| Bench | Before | After |
|-------|--------|-------|
| `examples/perf/cold_failure_arm.coil` | 1.634G | 1.578G (−3.4%) |
| `examples/perf/cold_call_arm.coil` | 909.8M | 829.8M (−8.8%) |
| `examples/perf/inline_hint_over_limit.coil` (step 3) | 2.667G | 1.772G (−33.6%) |
| `examples/perf/partial_inline_cold_arm.coil` (step 4) | 453.5M | 209.9M (−53.7%) |
| `examples/perf/hot_hint_inline.coil` (step 5) | 2.667G | 1.772G (−33.6%) |
| `examples/perf/hot_hint_clone.coil` (step 5, `--clone-budget 1`) | 1.086G | 627.4M (−42.2%) |

`hot_hint_clone.coil` at the default budget is neutral (627.3M both): its clones exist anyway. A stdlib `HashMap.insert` tagged `#[hot]` (step 6 decides the tag) takes an int insert loop from 641.3M to 561.9M (−12.4%), and at `--clone-budget 1` from 1.140G to 561.9M.

## Steps

| Step | Status |
|------|--------|
| 1. Attributes: parse, check, carry on `HirBody` | done |
| 2. Cold inference and layout | done |
| 3. `#[inline]` / `#[cold]` in the same-module and cross-module inliners; the `#[inline]` warning (**E0807** `InlineRefused`) | done. Clone planning is unchanged: a cold generic function still gets its clones, so its call keeps the ground ABI |
| 4. Partial inlining: cut cold regions into helpers before the inline cost check | done. A callee that holds a panic did not inline before at any size; a failing arm cut out lets its rest inline. It alone leaves `HashMap.insert` a call (640.5M both); the `#[hot]` tag is what moves it (step 6) |
| 5. `#[hot]`: clones outside the budget, a raised limit, spread to callees and `impl` siblings | done. Siblings are the other methods of the class, from every `impl` block of it; callees are those a hot body names, not every function it reaches |
| 6. Stdlib pass: tags only where a bench moves | done, no tags. `#[hot]` on `HashMap.insert` / `get_or` / `upsert` (or on `KeyIndex.find_hashed`): an int insert loop −3.6% (607.3M → 585.6M), a get loop and a string upsert loop flat; word count (1 MB of `gen_words.py` output) flat with `upsert` and −1.6% with `get_or` + `insert`, for 2.3% more code. Inlining a map method into every caller is not worth that outside a microbench, so the stdlib carries no hints |
| 7. `#[inline(always)]` / `#[inline(never)]`; tiers as multiples of the level's limit | done. Built at `#[inline]` 5x, `#[hot]` 10x, hot by spreading 4x; lowered in 7c |
| 7c. Tier multipliers measured: `#[inline]` 3x, `#[hot]` 5x, hot by spreading 2x | done. Six library workloads with author-style tags (coil-json, -csv, -deflate, -toml, -msgpack, word count) gain 0.2-0.4% at any setting from 2x up. At 4x / 5x / 10x coil-toml was +6.9% and coil-json +2.2%, from an inlined arena `add` (coil-lang#2241), and msgpack −2.5% for +75% of its code. These runs were on the bytecode VM, since deleted, where a merged body past the register limit of its fast tier cost about 2x the instructions (coil-lang#2242). Measure the tiers again on native code |
| 7d. Tiers per level: `-O1` / `-O2` 2x / 3x / 5x, `-O3` 4x / 5x / 10x, `-Os` 1x / 2x / 3x | done. Remeasured on the same workloads after #2241 / #2242 were fixed (main plus the phi-sharing follow-up). At `-O2`, 2x / 3x / 5x gives −0.2% to −1.8% for +3% code, and no setting regresses. 4x / 5x / 10x gives msgpack −3.9% for +13% code. At `-O3`, 2x / 3x / 5x and 4x / 5x / 10x build the same archives (a 200 base already takes every tagged callee): msgpack −2.1% and word count −16.1%, for +10% code. At `-Os`, 1x / 2x / 3x gives coil-json −4.4% and coil-csv −4.3% for +1% code |

# Optimizer hints: `#[inline]`, `#[cold]`, `#[hot]`

Three function attributes steer the optimizers where they cannot tell on their own. Each changes speed only: a program behaves the same with or without them. The compiler infers cold code where the program makes it clear. It never infers hot, and nothing here measures a run (no PGO, #301).

Branch: `feature/opt-hints`, one PR per step. Plan agreed 2026-11-02.

## Surface

On a function or a method (inherent, trait impl or trait default), without arguments:

| Attribute | Meaning | What the compiler does |
|-----------|---------|------------------------|
| `#[inline]` | Splice this function in wherever it is called directly | Inlines past the cost limits, loop bodies included. It refuses only what cannot be inlined (recursion, coroutines, function values), with a warning that says why |
| `#[cold]` | Calls to this function are rare | Never inlined. A branch that calls it is laid out after the body, and is cut out into a helper when its caller is inlined or specialized (partial inlining) |
| `#[hot]` | This function is on a hot path | Always gets a per-type clone, outside `--clone-budget`. Calls inside it use a 4x inline limit (400). Its direct callees and the other methods of its `impl` block are treated as hot too |

`#[cold]` cannot be combined with `#[inline]` or `#[hot]` (**E0806** `ConflictingOptHints`). A hint on a class or enum, or with arguments, is E0119.

### Precedence

Highest first:

1. A tag on the item itself. `#[cold]` on a callee of a hot item keeps it cold.
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
| Inliners | `Compiler::hir_inline_replan`: in a hot body every call, and anywhere a hot callee, inlines under `HOT_INLINE_COST` (400, same- and cross-module); `budget_of` lifts the cost limit (same- and cross-module) and the long-loop and invariant-loop refusals for an `#[inline]` callee; `callee_of` refuses a cold callee; `note_inline_refused` warns E0807 once per `#[inline]` function of the module that stays a call |
| Layout | `HirMirInput::cold_calls`; `mir::from_hir` `cold_arm` / `calls_cold` mark the arm's blocks cold, `layout` puts them after the body |

### Layout

An `if` or `match` arm is laid out after the body when it only returns or fails (a panic, after any statements), or when it calls a cold function, even one it falls through from. A branch whose one edge carries φ copies runs them inline on that edge and jumps away on the other (`mir::emit`, `Terminator::Br`), so the hot edge takes no jump to reach its copies.

Hit benches (callgrind, release; step 2 rows for layout, step 3 for `#[inline]`):

| Bench | Before | After |
|-------|--------|-------|
| `examples/perf/cold_failure_arm.coil` | 1.634G | 1.578G (−3.4%) |
| `examples/perf/cold_call_arm.coil` | 909.8M | 829.8M (−8.8%) |
| `examples/perf/inline_hint_over_limit.coil` (step 3) | 2.667G | 1.772G (−33.6%) |
| `examples/perf/hot_hint_inline.coil` (step 5) | 2.667G | 1.772G (−33.6%) |
| `examples/perf/hot_hint_clone.coil` (step 5, `--clone-budget 1`) | 1.086G | 627.4M (−42.2%) |

`hot_hint_clone.coil` at the default budget is neutral (627.3M both): its clones exist anyway. A stdlib `HashMap.insert` tagged `#[hot]` (step 6 decides the tag) takes an int insert loop from 641.3M to 561.9M (−12.4%), and at `--clone-budget 1` from 1.140G to 561.9M.

## Steps

| Step | Status |
|------|--------|
| 1. Attributes: parse, check, carry on `HirBody` | done |
| 2. Cold inference and layout | done |
| 3. `#[inline]` / `#[cold]` in the same-module and cross-module inliners; the `#[inline]` warning (**E0807** `InlineRefused`) | done. Clone planning is unchanged: a cold generic function still gets its clones, so its call keeps the ground ABI |
| 4. Partial inlining: cut cold regions into helpers before the inline cost check | |
| 5. `#[hot]`: clones outside the budget, 4x limit, spread to callees and `impl` siblings | done. Siblings are the other methods of the class, from every `impl` block of it; callees are those a hot body names, not every function it reaches |
| 6. Stdlib pass: tags only where a bench moves | |

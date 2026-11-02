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

## Steps

| Step | Status |
|------|--------|
| 1. Attributes: parse, check, carry on `HirBody` | done |
| 2. Cold inference and layout | |
| 3. `#[inline]` / `#[cold]` in the same-module and cross-module inliners and clone planning; the `#[inline]` warning | |
| 4. Partial inlining: cut cold regions into helpers before the inline cost check | |
| 5. `#[hot]`: clones outside the budget, 4x limit, spread to callees and `impl` siblings | |
| 6. Stdlib pass: tags only where a bench moves | |

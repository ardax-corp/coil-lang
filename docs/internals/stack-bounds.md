# Recursion stack bounds

The VM pre-allocates an operand stack sized from this analysis:

- non-recursive programs → [`DEFAULT_OPERAND_STACK_SLOTS`](../../compiler/src/typechecking/stack_bound.rs) (256)
- proven / attributed recursion → `max_frames × 16 + 16` (clamped to
  [`MAX_OPERAND_STACK_SLOTS`](../../machine/src/lib.rs))

[`Machine::with_operand_capacity`](../../machine/src/vm.rs) builds the VM; reactor
workers resize when a job needs a larger stack.

## Analysis

After typecheck, [`analyze_stack_bounds`](../../compiler/src/typechecking/stack_bound.rs)
walks the AST:

1. Build the user-function call graph and find **cycles** (self or mutual).
2. For each self-recursive function, try to prove a finite **frame depth** via a
   unified **measure shape**:
   - Among `int`/`byte` parameters, pick the first `p` that has a recognizable
     base case (`if p <= K` / `p < K` / `p == K`) **and** every self-call of `f`
     passes `p - k` (`k > 0`) in that argument slot. Other arguments are ignored
     for depth.
   - Walk the whole body: surrounding operators (`+`, `/`, `%`, nested `let`s,
     …) do not matter once self-calls are collected. `min_step` is the minimum
     positive `k` across those calls.
   - Depth ≈ `((max_entry - base) / min_step) + 1`.
   - **Tail-only** self-calls (`return f(...)`) → depth `1` (matches `TailCall`).
3. Entry measure values may be:
   - integer literals (`fib(32)`);
   - intra-procedural const bindings via `const_fold::eval_expr`
     (`let n = 30; fib(n)`, `const N = 10; fib(N)`, `fib(10 + 20)`);
   - shallow interprocedural wrappers: non-recursive helpers whose params are
     constant at every call site propagate into recursive callees
     (`main → helper(32) → fib(n)`).
4. If depth is unprovable, require `#[max_depth(N)]` on that function.

Assignments to a traced name kill the binding (fail closed), including plain
`=`, compound `+=` / `-=` / …, and `++` / `--`. Opaque / dynamic
arguments (`fib(noise())`), mutual recursion without a self-measure, and shapes
where some self-call does not decrease the measure are **unprovable**.

## Attribute

```coil
#[max_depth(64)]
fn walk(int n) -> int {
    // …
}
```

`N` is a positive integer upper bound on simultaneous call frames of that
recursive function. Valid only on `fn` (see [Syntax — Attributes](https://github.com/ardax-corp/coil-website/blob/main/src/content/docs/references/syntax.md#attributes)).

## Relation to auto-par

[`par_profit`](../../compiler/src/typechecking/par_profit.rs) uses its own
fork-site detector for auto-par profitability. Stack-bound analysis remains the
more general path (arbitrary measures, const tracing) and runs even when
`COIL_AUTO_PAR=0`, including for impure recursive functions.

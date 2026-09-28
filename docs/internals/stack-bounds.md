# Operand stack sizing and growth

Locals and operands share one VM buffer ([`Stack`](../../machine/src/memory/stack.rs)).
Its size is decided in two places:

- **Compile time, a starting size.** Programs without recursion start at
  [`DEFAULT_OPERAND_STACK_SLOTS`](../../compiler/src/typechecking/stack_bound.rs)
  (256). Proven or hinted recursion starts at `max_frames × 16 + 16`; after
  dense specialize, a recursive body that `Seek`s more than 16 slots
  reapplies `max_frames × seek + seek`
  ([`rescale_operand_slots_for_dense_seek`](../../compiler/src/typechecking/stack_bound.rs), Q7 `tak`).
- **Run time, growth.** The VM never relies on that size. Whenever it opens
  a frame without room, it grows the buffer (doubling) up to
  [`MAX_OPERAND_STACK_SLOTS`](../../machine/src/lib.rs), then panics with
  `stack overflow`. The limit on live frames,
  [`MAX_CALL_FRAMES`](../../machine/src/lib.rs), stops recursion that never
  raises the cursor.

## Frame reserve

[`common::frame_reserve`](../../common/src/frame_bound.rs) bounds how far any
one frame can raise the cursor above where it was opened. It splits the code
into components joined by fall-through and jumps (calls and code pointers
open new frames, so they do not join), and takes the largest
`highest slot touched + sum of each op's pushes`. Loops have no net push, and
`STORE` / `Seek` / dense registers only raise the cursor to a slot counted in
the first term. The per-op table is an exhaustive `match`, so a new opcode
does not build until it is classified.

The VM computes the reserve once per program (keyed by the code slice
`execute` runs, so old archives are covered too) and keeps
`cursor + reserve ≤ capacity` at:

- `CALL`, `TailCall`, `CallIndirect` (after its captures / dictionaries), and
  host `call_function`;
- coroutine resume and `yield from` (the saved segment lands above the cursor);
- a `JumpIfMatch` hit (the payload width is only known at run time).

`CALL` pays one compare against the cached capacity; the rest are off the hot
path.

Archive **minor 13** (COI-358 E0) stores `operand_stack_slots` on
[`ArchivedProgram`](../../common/src/archive.rs). `coil run foo.hyc` and
`coil-embed` start from that size. Envelopes older than 4.13 still load and
start from the Seek+CALL heuristic (`256` or `MAX_OPERAND_STACK_SLOTS`).

Archive **minor 14** (COI-359 E1) stores S2b stack maps on the same envelope.
`.hyc` / embed GC relocate matches compile-and-run. Pre-14 archives load
with empty maps (conservative stack scan), same as E0 execute. Shared-heap
steal requires real maps ([shared-heap-sendability.md](shared-heap-sendability.md)).

## Depth analysis

After typecheck, [`analyze_stack_bounds`](../../compiler/src/typechecking/stack_bound.rs)
walks the AST to pick the starting size:

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
   - **Tail-only** self- or sibling-cycle calls (`return f(...)` / `return g(...)`
     among an SCC) → depth `1` (matches `TailCall`,
     [#316](https://github.com/ardax-corp/coil-lang/pull/316); hit bench
     `examples/perf/tail_sibling.hy`). Matching one- or two-word ABI, including
     arity-2 immediate products.
3. Entry measure values may be:
   - integer literals (`fib(32)`);
   - intra-procedural const bindings via `const_fold::eval_expr`
     (`let n = 30; fib(n)`, `const N = 10; fib(N)`, `fib(10 + 20)`);
   - shallow interprocedural wrappers: non-recursive helpers whose params are
     constant at every call site propagate into recursive callees
     (`main → helper(32) → fib(n)`).
4. Anything else (dynamic entries, mutual recursion, no measure) gets no
   bound and starts from the default size.

Assignments to a traced name kill the binding (fail closed), including plain
`=`, compound `+=` / `-=` / …, and `++` / `--`.

## Attribute

```coil
#[max_depth(64)]
fn walk(int n) -> int {
    // …
}
```

Optional. `N` presizes the stack for `N` simultaneous frames of that function
when the analysis cannot prove a depth; a wrong `N` only costs a regrow. Valid
only on `fn`, and `N` must be a positive integer (see
[Syntax — Attributes](https://github.com/ardax-corp/coil-website/blob/main/src/content/docs/references/syntax.md#attributes)).
`E0802` / `E0803` (unbounded recursion, stack need past the VM limit) are no
longer emitted.

## Relation to auto-par

[`par_profit`](../../compiler/src/typechecking/par_profit.rs) uses its own
fork-site detector for auto-par profitability. Stack-bound analysis remains the
more general path (arbitrary measures, const tracing) and runs even when
`COIL_AUTO_PAR=0`, including for impure recursive functions.

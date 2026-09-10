# Q6 — MIR-friendly iterator protocol (near-term)

Linear: [COI-328](https://linear.app/ardax/issue/COI-328/q6-mir-friendly-iterator-protocol-for-for).
Not a permanent fuse-IL ceiling. Not a full `Iterator` trait rewrite.

## Choice

**Counted desugar** for the majority surface. Tip investigation (post Q5
`34c7a311`):

| Shape | Tip before this cut | Why |
|-------|---------------------|-----|
| `for x in arr` / `Vec` / `[T; N]` helper | Already dense (`DenseIndex` / `DenseArrayLen`) | Codegen was already `i`, `n`, `arr[i]`, `i+1` |
| Same helper, `continue`-split latch | No `VReduce` (body ≠ latch) | Extra continue label split the counted loop |
| `for x in 0..n` helper | Fuse-IL (`MakeDict` / `STRING` / `GetField`) | Pratt wraps `0..n`; the no-heap `Range` arm missed |
| `for` inside `main` + format / `push` | Fuse-IL | I4 + grow — not the iterator protocol |
| User `Iterator` / coro / dict / first-class range value | Fuse-IL | `CALL` + `Option` match, `ResumeCoro`, heap fields |

A full `Iterator` interface would keep `next` → `Option` in the hot loop
(JumpIfMatch / CALL). That is Q8 / I2 / I6 work, not the majority of
`for_in_sum`. Counted desugar unblocks array / slice / literal range first.

## Protocol

Language `for x in expr` is unchanged (`IntoIterator` / `Iterator` at the
type level). Runtime lowering:

1. **Array / Vec / `[T; N]`** — counted index loop (`idx < len`, load,
   `idx + 1`). Length-stable sites still pin. No `continue` in the body →
   while-shaped latch so MIR vectorize / dense match `while i < len`.
2. **Literal `start..end` / `..=`** — peel `Expr`/`Group` wrappers; two
   locals (`cur`, `end`); unit step. Same latch rule. No heap dict.
   Read-only `x` aliases the IV (while-shaped) so dense DestProp cannot
   drop the increment. Assignment to `x` keeps a per-trip copy.
3. **First-class range value** (`let r = 0..n; for x in r`) — unboxed
   `[start, end]` locals (inclusive lives in `Range` vs `RangeInclusive`).
   Same counted latch as (2). Escape (`to_vec`, pass as a value, method
   `self`) still boxes `{start,end,inclusive}`.
4. **Parameter / returned numeric Range** (C2) — free-fn params and
   direct `CALL`/`RETURN` use the same `[start, end]` pair. A local bind
   (`let iter = r`) then uses the B5 counted latch. `for x in make(n)`
   and `let r = make(n)` stay counted. Escaped fn values, inherent
   methods, and heap-field dicts still `GetField`. Bare `for x in r` on
   a live-in param pair can DestProp-peel back onto entry slots.
5. **Tuple** — temp array, then (1).
6. **Dict / coro / user `Iterator`** — existing protocol. Stay fuse-IL
   until a later rung (match / CALL / resume).

No new opcode. No env toggle. Keep/refuse is checksum + cost gate.

## Prove

- `examples/perf/for_in_sum.hy` — `sum` helper is the island (`VReduce` /
  dense Index). `main` stays format + fill (I4).
- `examples/perf/for_in_range.hy` — literal range helper is dense counted
  i64.
- `examples/perf/for_in_range_value.hy` — first-class `let r = 0..n`
  helper is the same dense counted i64 (B5).
- `examples/perf/for_in_range_param.hy` — `Range<int>` parameter helper
  is the same dense counted i64 (C2).
- `examples/perf/for_in_range_ret.hy` — returned `Range<int>` bind is
  the same dense counted i64 (C2).

Flagships do not need this island.

## Later rungs

Ranked as **B5** in [opt-generalization.md](opt-generalization.md) B0
(after B1 entry hygiene and B3 two-slot CALL):

- First-class range unpack without I4 `STRING` / heap `GetField` — **landed**
  for unboxed locals (`let r = 0..n`). **C2** lands free-fn param /
  returned numeric Range as two-slot `[start, end]`. Heap-field dicts,
  escaped fn values, and method `self` still `GetField`.
- User `Iterator::next` on MIR (needs Q8 dense+match or LIR + CALL)
- Coro / dict for-in

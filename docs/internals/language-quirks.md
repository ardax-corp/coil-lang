# Language quirks (locked Q1–Q9)

Locked **2026-09-10**. Source of truth: Linear project
Language quirks
(COI-323 … COI-332;
this note is COI-331).

This file is **spec**, not a compiler changelog. Implementation tickets are
Q1–Q9 themselves. Q1 box-once is implemented. Q2 box-once is implemented.
Q3 grow on `[T; N]` is a typechecker
diagnostic (`FixedArrayGrow` / E0412). Q4 indexing `i % N` is Euclidean
into `0..N`. Q5 `panic` vs `raise`
is documented (CLI / embed abort only on `panic`; checksum boards use
`panic`).

User-facing language docs live in
[coil-website](https://github.com/ardax-corp/coil-website) (`src/content/docs/`).
There is no in-repo user manual to update here; cookbook / types pages there
should cite this note when those repos take the Q1–Q5 surface. In-repo
cookbook for Q5 is the section below plus
[`.claude/skills/coil-language`](../../.claude/skills/coil-language/SKILL.md).

## Locked table

| Id | Issue | Title | Locked |
|----|-------|-------|--------|
| Q1 | COI-323 | `[T; N]` reference + box-once | Reference semantics. Invisible unbox (frame slots / SROA) only when proven non-escaping. On escape, **box once** and reuse that heap identity. |
| Q2 | COI-324 | Named class reference + field-SROA | Named `new C` locals are references. Invisible field-SROA when field-only and non-escaping. Any identity use forces a real heap instance. Mirrors Q1. |
| Q3 | COI-325 | `[T; N]` cannot grow | `ArrayPush` / grow on a fixed array is a **type error**. Use `Vec`. |
| Q4 | COI-326 | Defined `i % N` for indexing | For **indexing**, `i % N` always maps into `0..N` (Euclidean-or-equivalent). Plain `xs[k]` without `%` still OOB outside `0..N`. |
| Q5 | COI-327 | `panic` vs `raise` | Keep both: `panic` **aborts the process**; `raise` **is catchable**. Document hard. Checksum / CLI boards use `panic`. |
| Q6 | COI-328 | MIR-friendly `for` / iterator | **Done.** Every `for` lowers to MIR. |
| Q7 | COI-329 | Recursion on MIR | **Done.** Recursive callees lower to MIR as any body. |
| Q8 | COI-330 | Match on niche / two-slot | **Done.** `match` on niche and two-slot values lowers to MIR. |
| Q9 | COI-332 | Full format / string on MIR | **Done.** Format and string ops are MIR instructions. |

## Q1 — `[T; N]` is a reference; box once on escape

`[T; N]` is a **heap-identity type**. Users observe one object: `===`,
return, call-arg, field store, and host edges all see the same pointer
once the value has escaped.

Invisible unbox (consecutive frame slots, S2f/S2k select, MIR SROA) is
allowed only while the local is **proven non-escaping**. That rewrite is
not a language-level value-array ABI.

On the first escape, materialize **one** heap object and reuse it on every
later escape edge. Later private index of that local goes through the
boxed object (slots are only a pre-escape rewrite). Value-array semantics
and always-heap are non-goals.

Compiler (A1 COI-334 + Q1 COI-323): boxes once.

## Q2 — Named `new C` is a reference; invisible field-SROA

Named `let p = new C(...)` has the same identity story as Q1. Field-only
non-escaping uses may unbox into consecutive field slots (S2j / I3). Any
identity use — call, return, method receiver, `drop`, alias, aggregate,
host/FFI, identity compare — forces a real heap instance.

On the first identity use, materialize **one** heap object and reuse it
on every later escape edge. Field uses before that edge stay slots;
field uses after it go through the boxed instance. Do not allocate a fresh
instance per edge. `fn drop()` and arity 0 or > 32 stay heap from
construction (finalizer / layout).

Compiler (S2j COI-320 + Q2 COI-324): HIR lowering decides which locals
never escape.

**COI-84 non-goal is superseded
for this narrow case.** COI-84 closed as “named locals stay allocated”
(#134 pin). Q2 reopens field-SROA and aligns that pin with box-once:
identity still forces heap, but not a heap-from-`new` refuse.

## Q3 — `[T; N]` cannot grow

Fixed arrays do not grow. `ArrayPush` (or any grow) on `[T; N]` is a
**type error**, not a silent specialize refuse and not a runtime no-op.
Growable storage is `Vec`. Length-changing Vec methods (`push`, `insert`,
`pop`, `remove`, `clear`, `reserve`) on a static array are refused in
the typechecker (call site and method-as-value access), not later as a
MIR / escape refuse.

Compiler (COI-325):
`FixedArrayGrow` (E0412) with help `use Vec<T> for growable storage`.

## Q4 — Indexing `i % N` maps into `0..N`

When an index is written `i % N` (or equivalent) against a length-`N`
array, the remainder is **defined** to land in `0..N`. Negative `i` is a
valid index. The mapping is Euclidean (or equivalent):

- Non-index `%` stays toward-zero (`(-2) % 3 == -2`).
- For an index, toward-zero `r = i % N` (`N > 0`) is rewritten to
  Euclidean `r + (N & (r >> 63))`, i.e. `r < 0` then `r += N`.
  Equivalently `((i % N) + N) % N`.
- Examples: `(-1) % 3 → 2`, `(-2) % 3 → 1`, `(-3) % 3 → 0`.

SROA select last-arm is slot `N-1` of that range — **spec**, not a soft
refuse that dumped every negative remainder on the last slot (that was
wrong for `(-2) % 3`). Skip the fix-up only when the dividend is
proven `>= 0` (counted-loop `i`); unknown / param / negative dividends
always fix up.

Plain `xs[k]` **without** `%` still panics OOB when `k` is outside
`0..N`. Defined mod does not weaken unproven index checks.

Compiler (COI-326): `lower::index_euclid` and `index_word`
(`mir/from_hir.rs`), with the sidecar's `nonneg_expr`.

## Q5 — `panic` aborts; `raise` is catchable

Keep both spellings. Do not collapse them. They are not synonyms and
opts must not rewrite one into the other.

| Form | Meaning |
|------|---------|
| `panic expr` | **Aborts the process.** Lowers to `MirInst::Panic`. Writes `panic: <msg>` (plus a location suffix when known). Sets the driver's `panicked`. Checksum boards, CLI / embed hard failure, and “this must not continue” paths use `panic`. |
| `raise expr` | **Catchable.** Early-return `Result.Err(expr)` from a Result-mode function. `try` / `?` / `match` can swallow it. Not a process abort. |

See [debug-info.md](debug-info.md) for panic locations.

### Cookbook

| Use | Spelling |
|-----|----------|
| Checksum / hit-bench / “if this fires the program is wrong” | `panic "… checksum"` |
| CLI / packaged-app hard failure | `panic` |
| Recoverable user error, `?` chains, `match Result` | `raise` / `return Result.Err(…)` |
| `coil test` expected-fail path | `raise` or `assert!(false)` (harness treats `Err` as a failed case) |
| `coil test` “this must not continue” | `panic` (also a failed case, and aborts that run) |

Uncaught `raise` from `main` is a `Result.Err` **return**. Default `coil`
run, `coil run`, and `coil-embed` exit **0** unless a `panic`
ran. That is why a checksum written as `if bad { raise "…" }` can
print nothing and still look green.

`coil test` is stricter: a case fails on **either** `panic` or a
non-`Ok` Result (`!panicked && result_is_ok`). Do not rely on that
harness when writing CLI / embed / example boards.

```coil
// Abort — CLI / embed exit 1. Cannot be caught.
fn must_hold(int got, int want) {
    if got != want {
        panic "checksum";
    }
}

// Catchable — caller uses ? or match.
fn parse_pos(int n) {
    if n < 0 {
        raise "neg";
    }
    return n;
}
```

Runnable contrast: [`examples/panic.coil`](../../examples/panic.coil) vs
[`examples/raise_try.coil`](../../examples/raise_try.coil).

## Q6–Q9 — Delivered

These four were delivery commitments. All are met: every body lowers from
HIR to MIR. The notes below say what each covers.

| Quirk | Implementation | Notes |
|-------|----------------|-------|
| Q6 | Counted desugar for array / Vec / `[T; N]` / literal range (`for_in_sum` / `for_in_range`). First-class `let r = 0..n` locals unbox (B5 / `for_in_range_value`). Free-fn param / returned numeric Range unbox (C2 / `for_in_range_param` / `for_in_range_ret`). Heap-field / array-held Range counted (C2b rung 1 / `for_in_range_field` / `for_in_range_array`). User `Iterator::next` on MIR (C2b rung 2 / `for_in_iter`); counted `into_iter` Range (`for_in_iter_range`). Records are not iterable (C2b rung 3, dict for-in, was removed). Coro for-in on MIR (C2b rung 4 / `for_in_coro`; `ResumeCoro` / `DoneCoro`) | History: [q6-iterator-protocol.md](q6-iterator-protocol.md). |
| Q7 | Self, sibling and mutual calls are MIR `Call`s, one- or two-word | A planned tail call is marked in `MirFunc::tail_targets`. |
| Q8 | Niche, two-slot and boxed `match` all lower to MIR branches | Layouts: [AGENTS.md](../../AGENTS.md) language facts. |
| Q9 | Format and string ops are MIR instructions (`Print`, `Format`, `Stringify`); `from_bytes` / `to_bytes` are `HostInvoke`s | No second format lowering. History: [q9-format-string.md](q9-format-string.md). |

Island history: [mir-islands.md](mir-islands.md),
[opt-generalization.md](opt-generalization.md).

## Opt implications (do not implement here)

| Area | Implication |
|------|-------------|
| S2g | **Box-once** (Q1 / COI-334). Identity across return / call / field / host matches. |
| S2j / I3 | Mirrors Q2. Field-only unbox stays; identity boxes once and reuses. COI-84 non-goal does not block that case. |
| Grow | Type error on `[T; N]` (Q3). Do not keep grow as an opt refuse for fixed arrays once the checker lands. |
| Defined mod | S2f / S2k / S2h select and runtime `%` follow Q4. Negative `i % N` is in-range. |
| `panic` / `raise` | Checksum and CLI boards: `panic` (Q5). Opts must not rewrite `panic` into `raise`. |

Related opt notes: [opt-generalization.md](opt-generalization.md) (A0 doctrine),
[optimization-roadmap.md](optimization-roadmap.md),
[limitations.md](limitations.md) (COI-84 / S2j).

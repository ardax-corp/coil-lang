# Language quirks (locked Q1–Q9)

Locked **2026-09-10**. Source of truth: Linear project
[Language quirks](https://linear.app/ardax/project/language-quirks-025aacdd851c)
([COI-323](https://linear.app/ardax/issue/COI-323) … [COI-332](https://linear.app/ardax/issue/COI-332);
this note is [COI-331](https://linear.app/ardax/issue/COI-331)).

This file is **spec**, not a compiler changelog. Implementation tickets are
Q1–Q9 themselves. Do not treat leftover I4 / dense refuse rows as
overriding these decisions. Q1 box-once is implemented (codegen + IL). Q2 box-once is implemented
(codegen field-SROA + identity cache). Q3 grow on `[T; N]` is a typechecker
diagnostic (`FixedArrayGrow` / E0412). Q4 indexing `i % N` is Euclidean
into `0..N` (codegen rem + SROA last-arm as spec). Q5 `panic` vs `raise`
is documented (CLI / embed abort only on `panic`; checksum boards use
`panic`).

User-facing language docs live in
[coil-website](https://github.com/ardax-corp/coil-website) (`src/content/docs/`).
There is no in-repo user manual to update here; cookbook / types pages there
should cite this note when those repos take the Q1–Q5 surface. In-repo
cookbook for Q5 is the section below plus
[`.cursor/skills/coil-language`](../../.cursor/skills/coil-language/SKILL.md).

## Locked table

| Id | Issue | Title | Locked |
|----|-------|-------|--------|
| Q1 | [COI-323](https://linear.app/ardax/issue/COI-323) | `[T; N]` reference + box-once | Reference semantics. Invisible unbox (frame slots / SROA) only when proven non-escaping. On escape, **box once** and reuse that heap identity. |
| Q2 | [COI-324](https://linear.app/ardax/issue/COI-324) | Named class reference + field-SROA | Named `new C` locals are references. Invisible field-SROA when field-only and non-escaping. Any identity use forces a real heap instance. Mirrors Q1. |
| Q3 | [COI-325](https://linear.app/ardax/issue/COI-325) | `[T; N]` cannot grow | `ArrayPush` / grow on a fixed array is a **type error**. Use `Vec`. |
| Q4 | [COI-326](https://linear.app/ardax/issue/COI-326) | Defined `i % N` for indexing | For **indexing**, `i % N` always maps into `0..N` (Euclidean-or-equivalent). Plain `xs[k]` without `%` still OOB outside `0..N`. |
| Q5 | [COI-327](https://linear.app/ardax/issue/COI-327) | `panic` vs `raise` | Keep both: `panic` **aborts the process**; `raise` **is catchable**. Document hard. Checksum / CLI boards use `panic`. |
| Q6 | [COI-328](https://linear.app/ardax/issue/COI-328) | MIR-friendly `for` / iterator | **Commit** a near-term MIR-friendly iterator protocol — not a permanent fuse-IL ceiling. |
| Q7 | [COI-329](https://linear.app/ardax/issue/COI-329) | Dense / LIR recursion | **Commit** near-term dense or LIR recursion / typed recursive `CALL`. Recursive callees are not forever fuse-only. |
| Q8 | [COI-330](https://linear.app/ardax/issue/COI-330) | Dense+match niche / two-slot | **Commit** near-term **dense+match** for niche / two-slot (not only MIR→LIR I2). |
| Q9 | [COI-332](https://linear.app/ardax/issue/COI-332) | Full format / string on MIR | **Commit** full format / string on MIR. **Reopen I4** as a delivery ladder, not a permanent barrier. |

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

Compiler (A1 [COI-334](https://linear.app/ardax/issue/COI-334) + Q1
[COI-323](https://linear.app/ardax/issue/COI-323)): codegen
`emit_escape_stack_array` and IL `escape_analysis` box once. A refused
tiny-inline / peel must not leave a stale box cache (that was a fresh
`LOAD` of an unwritten slot). S2g's old fresh-`MakeArray`-per-edge policy
is gone.

## Q2 — Named `new C` is a reference; invisible field-SROA

Named `let p = new C(...)` has the same identity story as Q1. Field-only
non-escaping uses may unbox into consecutive field slots (S2j / I3). Any
identity use — call, return, method receiver, `drop`, alias, aggregate,
host/FFI, identity compare — forces a real heap instance.

On the first identity use, materialize **one** heap object and reuse it
on every later escape edge. Field uses before that edge stay slots;
field uses after it go through the boxed instance. Do not emit a fresh
`InitTyped` per edge. `fn drop()` and arity 0 or > 32 stay heap from
construction (finalizer / layout).

Compiler (S2j [COI-320](https://linear.app/ardax/issue/COI-320) + Q2
[COI-324](https://linear.app/ardax/issue/COI-324)): codegen binds eligible
`new C` to field slots and `emit_escape_unboxed_class` boxes once.
`local_escape` `frame_local` remains the never-escaped I3 fact.

**[COI-84](https://linear.app/ardax/issue/COI-84) non-goal is superseded
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

Compiler ([COI-325](https://linear.app/ardax/issue/COI-325)):
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
wrong for `(-2) % 3`). Skip the IL fixup only when the dividend is
proven `>= 0` (counted-loop `i`); unknown / param / negative dividends
always fix up.

Plain `xs[k]` **without** `%` still panics OOB when `k` is outside
`0..N`. Defined mod does not weaken unproven index checks.

Compiler ([COI-326](https://linear.app/ardax/issue/COI-326)):
`compile_array_index_expr` + sidecar `nonneg_expr`. Heap `Index` and
SROA select share the same rem.

## Q5 — `panic` aborts; `raise` is catchable

Keep both spellings. Do not collapse them. They are not synonyms and
opts must not rewrite one into the other.

| Form | Meaning |
|------|---------|
| `panic expr` | **Aborts the process.** Emits `Instruction::Panic`. Writes `panic: <msg>` (plus a line-table suffix when known). Sets `Machine::panicked`. Checksum boards, CLI / embed hard failure, and “this must not continue” paths use `panic`. |
| `raise expr` | **Catchable.** Early-return `Result.Err(expr)` from a Result-mode function. `try` / `?` / `match` can swallow it. Not a process abort. |

See [debug-info.md](debug-info.md) for panic line-table output.

### Cookbook

| Use | Spelling |
|-----|----------|
| Checksum / hit-bench / “if this fires the program is wrong” | `panic "… checksum"` |
| CLI / packaged-app hard failure | `panic` |
| Recoverable user error, `?` chains, `match Result` | `raise` / `return Result.Err(…)` |
| `coil test` expected-fail path | `raise` or `assert(false)` (harness treats `Err` as a failed case) |
| `coil test` “this must not continue” | `panic` (also a failed case, and aborts that VM) |

Uncaught `raise` from `main` is a `Result.Err` **return**. Default `coil`
run, `coil run`, and `coil-embed` exit **0** unless `Machine::panicked`
is set. That is why a checksum written as `if bad { raise "…" }` can
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

Runnable contrast: [`examples/panic.hy`](../../examples/panic.hy) vs
[`examples/raise_try.hy`](../../examples/raise_try.hy).

## Q6–Q9 — Roadmap commits (not permanent refuse)

These four are **near-term delivery commitments**. They do not land in
this docs PR. They **do** change how island / refuse docs may speak:

| Quirk | Today (implementation) | After the commit |
|-------|------------------------|------------------|
| Q6 | Counted desugar for array / Vec / `[T; N]` / literal range (`for_in_sum` / `for_in_range`). User `Iterator` / coro / dict / first-class range still fuse-IL | Phased ladder. Not a permanent fuse-IL ceiling. See [q6-iterator-protocol.md](q6-iterator-protocol.md). |
| Q7 | Dense one-word self-recursive `CALL` / `TailCall` on `tak` / `fib` | Landed (dense; LIR `CALL` still refuse). Mutual / two-slot stay fuse-IL. |
| Q8 | I2 is MIR→LIR match; **dense+match refuses** | Near-term dense+match for niche / two-slot, not only I2 LIR. |
| Q9 | I4 closed as a **hard MIR barrier** | **I4 reopened** as a phased delivery ladder. Full format / string on MIR. Avoid a half-format second lowering. Phase so numeric / array work is not stalled. |

Island inventory: [mir-islands.md](mir-islands.md). Dense refuse rows:
[specialize-refuse.md](specialize-refuse.md).

## Opt implications (do not implement here)

| Area | Implication |
|------|-------------|
| S2g | **Box-once** (Q1 / COI-334). Identity across return / call / field / host matches. |
| Heap ops | A2 ([COI-335](https://linear.app/ardax/issue/COI-335)): dense-native `Index` / `StoreIndex` / `ArrayLen` / `Make*` when maps allow. Box only at CALL / RETURN / HostInvoke (`DensePush`). Residual LOAD/STORE boxing stays fuse-IL via the cost gate. |
| S2j / I3 | Mirrors Q2. Field-only unbox stays; identity boxes once and reuses. COI-84 non-goal does not block that case. |
| Grow | Type error on `[T; N]` (Q3). Do not keep grow as an opt refuse for fixed arrays once the checker lands. |
| Defined mod | S2f / S2k / S2h select and runtime `%` follow Q4. Negative `i % N` is in-range. |
| `panic` / `raise` | Checksum and CLI boards: `panic` (Q5). Opts must not rewrite `panic` into `raise`. |
| Q6–Q9 | Update island / refuse copy as those tickets land. Until then, “today path” may still be fuse-IL, but the **target** is the commit, not a forever barrier. |

Related opt notes: [opt-generalization.md](opt-generalization.md) (A0 doctrine),
[s2d-inloop-make-tax.md](s2d-inloop-make-tax.md) (S2f–S2l),
[optimization-roadmap.md](optimization-roadmap.md) (`escape_analysis`),
[limitations.md](limitations.md) (COI-84 / S2j).

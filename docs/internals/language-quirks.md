# Language quirks (locked Q1–Q9)

Locked **2026-09-10**. Source of truth: Linear project
[Language quirks](https://linear.app/ardax/project/language-quirks-025aacdd851c)
([COI-323](https://linear.app/ardax/issue/COI-323) … [COI-332](https://linear.app/ardax/issue/COI-332);
this note is [COI-331](https://linear.app/ardax/issue/COI-331)).

This file is **spec**, not a compiler changelog. Implementation tickets are
Q1–Q9 themselves. Do not treat today's S2g / I4 / dense refuse rows as
overriding these decisions.

User-facing language docs live in
[coil-website](https://github.com/ardax-corp/coil-website) (`src/content/docs/`).
There is no in-repo user manual to update here; cookbook / types pages there
should cite this note when those repos take the Q1–Q5 surface.

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
later escape edge. Value-array semantics and always-heap are non-goals.

**S2g today is out of spec.** S2g ([COI-317](https://linear.app/ardax/issue/COI-317))
emits a fresh `MakeArray` per named escape and allows multiple snapshot
boxes when no private use follows. That **fresh-box-per-edge** policy is
wrong under Q1. Follow-up: materialize-once (A1 / COI-323). Until then,
do not add more per-edge snapshot boxing.

## Q2 — Named `new C` is a reference; invisible field-SROA

Named `let p = new C(...)` has the same identity story as Q1. Field-only
non-escaping uses may unbox into consecutive field slots (S2j / I3). Any
identity use — call, return, method receiver, `drop`, alias, aggregate,
host/FFI, identity compare — forces a real heap instance.

**[COI-84](https://linear.app/ardax/issue/COI-84) non-goal is superseded
for this narrow case.** COI-84 closed as “named locals stay allocated”
(#134 pin). Q2 reopens **field-only non-escaping** SROA as the written
rule. Whole-object identity still forces heap; `fn drop()` still boxes.
S2j ([COI-320](https://linear.app/ardax/issue/COI-320) / #372) already
mirrors this shape — align remaining pins and tests with Q2, do not
treat COI-84 as a forever refuse.

## Q3 — `[T; N]` cannot grow

Fixed arrays do not grow. `ArrayPush` (or any grow) on `[T; N]` is a
**type error**, not a silent specialize refuse and not a runtime no-op.
Growable storage is `Vec`. Implementation is the typechecker / name
resolution diagnostic (COI-325), plus fixing examples that push onto
fixed arrays.

## Q4 — Indexing `i % N` maps into `0..N`

When an index is written `i % N` (or equivalent) against a length-`N`
array, the remainder is **defined** to land in `0..N`. Document
Euclidean-or-equivalent (negative `i` is a valid index, not last-arm
luck). SROA select / runtime must follow the spec; the old “negative
remainder → last slot, not OOB” refuse is no longer the language rule
for `%`.

Plain `xs[k]` **without** `%` still panics OOB when `k` is outside
`0..N`. Defined mod does not weaken unproven index checks.

## Q5 — `panic` aborts; `raise` is catchable

Keep both spellings. Do not collapse them.

| Form | Meaning |
|------|---------|
| `panic` | Aborts the process. Checksum boards, CLI / embed hard failure, and “this must not continue” paths use `panic`. |
| `raise` | Catchable. `try` / `?` / user error flow. |

Misusing `raise` for abort hides checksum failures (a catch can swallow
the board). See [debug-info.md](debug-info.md) for panic line-table
output. Audit examples that still `raise` for abort (COI-327).

## Q6–Q9 — Roadmap commits (not permanent refuse)

These four are **near-term delivery commitments**. They do not land in
this docs PR. They **do** change how island / refuse docs may speak:

| Quirk | Today (implementation) | After the commit |
|-------|------------------------|------------------|
| Q6 | `for` / iterator bodies (`for_in_sum`) stay fuse-IL | MIR-friendly protocol (desugar, Iterator, or counted). Not a permanent fuse-IL ceiling. |
| Q7 | Recursive `tak` / `fib` stay fuse-IL on the callee | Dense or LIR recursion / typed recursive `CALL`. Recursion is not forever fuse-only. |
| Q8 | I2 is MIR→LIR match; **dense+match refuses** | Near-term dense+match for niche / two-slot, not only I2 LIR. |
| Q9 | I4 closed as a **hard MIR barrier** | **I4 reopened** as a phased delivery ladder. Full format / string on MIR. Avoid a half-format second lowering. Phase so numeric / array work is not stalled. |

Island inventory: [mir-islands.md](mir-islands.md). Dense refuse rows:
[specialize-refuse.md](specialize-refuse.md).

## Opt implications (do not implement here)

| Area | Implication |
|------|-------------|
| S2g | **Box-once** (Q1 / COI-334). Identity across return / call / field / host matches. |
| S2j / I3 | Mirrors Q2. Field-only non-escaping unbox stays; identity use forces heap. COI-84 non-goal does not block that narrow case. |
| Grow | Type error on `[T; N]` (Q3). Do not keep grow as an opt refuse for fixed arrays once the checker lands. |
| Defined mod | S2f / S2k / S2h select and runtime `%` follow Q4. Negative `i % N` is in-range. |
| `panic` / `raise` | Checksum and CLI boards: `panic` (Q5). Opts must not rewrite `panic` into `raise`. |
| Q6–Q9 | Update island / refuse copy as those tickets land. Until then, “today path” may still be fuse-IL, but the **target** is the commit, not a forever barrier. |

Related opt notes: [opt-generalization.md](opt-generalization.md) (A0 doctrine),
[s2d-inloop-make-tax.md](s2d-inloop-make-tax.md) (S2f–S2l),
[optimization-roadmap.md](optimization-roadmap.md) (`escape_analysis`),
[limitations.md](limitations.md) (COI-84 / S2j).

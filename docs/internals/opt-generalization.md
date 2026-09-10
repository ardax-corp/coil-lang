# Opt generalization (COI-333 A0)

Doctrine for generalizing MIR / IL opts so they fit **majority** programs,
not prove-board shapes. No compiler changes in this note.

Linear: [Opt generalization](https://linear.app/ardax/project/opt-generalization-0adb1fb677d9)
([COI-333](https://linear.app/ardax/issue/COI-333/a0-opt-generalization-doctrine-doc)).
Constrained by locked [language quirks](language-quirks.md) Q1–Q9
([COI-331](https://linear.app/ardax/issue/COI-331/q0-docs-language-quirksmd-locked-q1-q9);
relative path — that file may land on a separate PR).

Island history and refuse tables stay in [mir-islands.md](mir-islands.md),
[specialize-refuse.md](specialize-refuse.md), and
[mir-stack-maps.md](mir-stack-maps.md). This note is the **default** those
tables shrink toward.

## Principles

1. **MIR is the default** for eligible bodies. Fuse-IL is the fallback
   lowerer, not a second optimizer to beat. After stack-IL opts, lift →
   SSA opt → dense or MIR→LIR. Keep fuse-IL when the body is ineligible
   or the cost gate loses. Do not grow fuse-only peeps that exist only to
   outscore a dense reconstruct.

2. **One object story** (quirks [Q1](https://linear.app/ardax/issue/COI-323/q1-t-n-reference-box-once-on-escape)
   / [Q2](https://linear.app/ardax/issue/COI-324/q2-named-class-reference-invisible-field-sroa)).
   The heap object is language truth. Frame slots / scalar SSA are a
   **proven non-escaping rewrite**. Escape (call, return, field, host,
   alias, identity compare) **boxes once** and reuses that identity — not
   a fresh `MakeArray` / `InitTyped` per edge.

3. **Prefer dense-native ops** over residual stack boxing. Index /
   `StoreIndex` / `ArrayLen` / `Make*` / CALL should run as typed dense
   ops when maps and ABI allow ([COI-335](https://linear.app/ardax/issue/COI-335/a2-dense-native-heap-ops-indexmakecall)).
   Box and unbox only at ABI edges (CALL / RETURN / HostInvoke), not as
   the in-loop tax.

4. **Ship only if checksum + cost gate.** Reconstruct must match fuse-IL
   semantics (checksum) and must not be **denser-but-slower** by default.
   That is the S2l spirit ([s2d-inloop-make-tax.md](s2d-inloop-make-tax.md)):
   try the MIR path so SROA / LICM can delete work; keep it only when the
   reconstruct is cheaper or equal. Residual in-loop `Make*` that still
   boxes via LOAD/STORE stays fuse-IL. A2 emits `DenseIndex` /
   `DenseStoreIndex` / `DenseArrayLen` / `DenseMake` (stack-neutral) and
   `DensePush` at CALL / HostInvoke edges; the cost gate still refuses a
   boxed reconstruct that is denser-but-slower.

5. **Refuse inventory shrinks to hard walls.** A hard wall is unsound or
   a true language / runtime barrier until a locked commit lands (Q6–Q9
   iterators / recursion / boxed match / strings; I7 debugger / `-Og`;
   unmapped alloc). Everything else is **cost-gated**: lift, opt, emit,
   compare to fuse-IL. Do not add feature-shaped refuses (W3 work-op
   floors, Seek≤64 prove quirks, HostInvoke **id** allowlists) as
   permanent walls — fold them into purity bits + measured cost
   ([COI-336](https://linear.app/ardax/issue/COI-336/a3-broaden-mir-entry-shrink-refuse-tables)).

## Object story (opts)

| Proven | Rewrite | Identity |
|--------|---------|----------|
| Non-escaping `[T; N]` or field-only named `new C` | Consecutive slots or scalar SSA | No heap object |
| Escapes | Box **once** on first escape; later edges reuse it | Heap is truth |
| Unproven / grow / `Vec` / method `self` | Stay heap | Heap is truth |

Q3: `[T; N]` cannot grow (type error; use `Vec`). Q4: indexing `i % N`
is defined into `0..N`. Those are language rules, not opt refusals.
S2f–S2l shape tickets collapse into one shared escape answer
([COI-334](https://linear.app/ardax/issue/COI-334/a1-unify-tn-sroa-under-q1-q4)).

## Phases

| # | Work | Issue | Code? |
|---|------|-------|-------|
| **A0** | This doctrine + A4 measurement | [COI-333](https://linear.app/ardax/issue/COI-333/a0-opt-generalization-doctrine-doc) | docs only |
| **A1** | Unify `[T; N]` / SROA under Q1–Q4 | [COI-334](https://linear.app/ardax/issue/COI-334/a1-unify-tn-sroa-under-q1-q4) | yes |
| **A2** | Dense-native heap ops (Index / Make / CALL) | [COI-335](https://linear.app/ardax/issue/COI-335/a2-dense-native-heap-ops-indexmakecall) | yes (archive **4.10**) |
| **A3** | Broaden MIR entry; shrink refuse tables | [COI-336](https://linear.app/ardax/issue/COI-336/a3-broaden-mir-entry-shrink-refuse-tables) | yes |
| **A4** | Measurement contract (below) | [COI-337](https://linear.app/ardax/issue/COI-337/a4-measurement-contract-natural-suites-embed) | continuous |

Order: A0 → A1 → A2 → A3. A4 applies to every generalization PR.

## A4 — Measurement

Standing gate for generalization PRs (extends island A/B in
[mir-islands.md](mir-islands.md)):

1. **Prefer `coil-embed`.** Same host protocol; fixed VM image when
   comparing compilers. Fat-`coil` LTO noise is not a MIR regression.

2. **Natural suites first.** Prove on bodies people already run:
   `nsieve`, `binary_trees`, `nbody`, `for_in_sum`. `s2d_inloop_*` and
   other pack/bump kernels stay regression canaries, not the only score.

3. **Flagships** (`mandelbrot`, `tak`, `nsieve`, `binary_trees`, `fib`):
   checksums match; wall time **flat or better** on embed when archives
   differ. **Identical archives are OK** when the change does not fire
   there. Identical flagship `.hyc` is not a skip when other claimed
   bodies change — report those too.

4. **Checksum + cost.** No ship if output diverges. No ship if the
   default path is denser-but-slower (S2l). Skip merge only on a wash /
   regress of a body the PR claims, or a flagship miss when archives
   differ.

5. **No env toggles.** Production is one path. Do not revive
   `COIL_S2D_DENSE_INLOOP` or add compile-time force/refuse overrides
   for A/B. Parent vs tip binaries are the comparison.

6. **No PGO. No vanity hit benches** whose only job is a number. Hit
   benches are allowed when a sound opt does not fire on flagships
   (AGENTS.md hit-bench prove); they do not replace the natural suite.

PR checklist: embed A/B table, checksums, flagship archive sha256,
natural-suite rows, no env knobs.

## Non-goals

- Dual AST walkers / a second semantic IR
- Fuse-IL as a competing optimizer
- Permanent refuse rows for cost (those become gates)
- PGO, score-chasing, or env-gated opts
- Combining this PR with [language-quirks.md](language-quirks.md)
  ([COI-331](https://linear.app/ardax/issue/COI-331/q0-docs-language-quirksmd-locked-q1-q9))

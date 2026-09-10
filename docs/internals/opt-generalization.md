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
   a true language / runtime barrier. Q6–Q9 first rungs have landed
   (counted `for`, one-word recursive `CALL`, niche/two-slot dense+match,
   format LIR reconstruct); leftover shapes are **ladders** or **cost
   gates**, not forever refuse. Remaining walls: I7 debugger / `-Og`,
   unmapped alloc, mutual / two-slot recursive `CALL`, boxed multi-payload
   match, residual `Byte`/`Pow`/bitwise. Everything else is
   **cost-gated**: lift, opt, emit, compare to fuse-IL. Do not add
   feature-shaped refuses (W3 work-op floors, Seek≤64 prove quirks,
   HostInvoke **id** allowlists) as permanent walls — fold them into
   purity bits + measured cost
   ([COI-336](https://linear.app/ardax/issue/COI-336/a3-broaden-mir-entry-shrink-refuse-tables)).
   Post-quirks inventory: [B0](#b0--post-quirks-refuse-audit) below.

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
| **B0** | Post-quirks refuse audit + ranked B* | [COI-338](https://linear.app/ardax/issue/COI-338/b0-post-quirks-refuse-audit-ranked-revisit-plan) | docs only |
| **B1** | Q6–Q8 MIR/dense entry hygiene | [COI-339](https://linear.app/ardax/issue/COI-339/b1-q6-q8-mirdense-entry-hygiene) | yes (eligibility + docs; no B2 Seek rewrite) |

Order: A0 → A1 → A2 → A3. A4 applies to every generalization PR.
B0 is the post-Q6–Q9 inventory; B1+ are ranked below (no impl in B0).

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

## B0 — Post-quirks refuse audit

Tip `eaf17283` (Q9 R1 #389). Cross-check:
[specialize-refuse.md](specialize-refuse.md),
[mir-islands.md](mir-islands.md),
[language-quirks.md](language-quirks.md),
[q6-iterator-protocol.md](q6-iterator-protocol.md),
[q9-format-string.md](q9-format-string.md),
Q7 #387 / Q8 #388 boards.

No B1+ implementation in this note.

### Hard walls after A3 (#380) vs after Q6–Q9

A3 replaced W3 / Seek≤64 / HostInvoke **id** floors with a short wall
list. Q6–Q9 first rungs turned most of those named commits into
**ladders** or **cost gates**.

| A3 wall (#380) | After Q6–Q9 (`eaf17283`) | Kind now |
|----------------|--------------------------|----------|
| `FORMAT` / `STRING` / `STRINGIFY` / `PRINT` / I4 bytes | **Q9 R1:** table string ops are SSA + MIR→LIR. Dense infer still refuses. `from_bytes` / `to_bytes` wait **R2**. Unicode / regex wait later rungs | Ladder + cost (R1 may keep fuse-IL) |
| Recursion on the callee (`CALL` / `TailCall`) | **Q7:** one-word self-`CALL` / `TailCall` may dense. **B2:** tight `tak` / `fib` convoy CALL on the stack (no prologue Seek / inter-CALL STORE) so the cost gate can keep them. Mutual / two-slot still refuse. LIR still cannot reconstruct `CALL` | Cost gate + later Q7 rungs |
| `for` / iterator (`for_in_sum`) | **Q6:** counted desugar — array / Vec / `[T; N]` / literal range helpers dense (`sum`, `for_in_range`). `main` + format / `Vec.push` stays fuse-IL. User `Iterator` / coro / dict / first-class range still refuse | Ladder (counted done) |
| Dense+match (stack vs regs) | **Q8:** niche / two-slot match may dense (register `Br`, cost gate). Boxed `JumpIfMatch` stays I2 LIR. Two-slot `CALL` / `RETURN` stay LIR | Cost gate + leftover ABI wall |
| Multi-payload `Unpack` / `JumpIfMatch` arity > 1 | Unchanged — fuse-IL | Hard wall (later island) |
| Debugger-attached / `-Og` | Unchanged — skip dense + LIR ([mir-deopt.md](mir-deopt.md)) | Hard wall (**I7** stays) |
| Unmapped alloc / GC safepoint | Unchanged for residual `Vec.push` / class `new` / unmapped edges. Mapped `Make*` already S2 / A2 | Hard wall until maps |
| Residual `Byte` / `Pow` / `AND`/`OR` | Unchanged | Hard wall (later island) |
| Compare-only (no arith) | Unchanged — fuse-IL or I8 LIR | Cost gate |

Folded at A3 and still not walls: W3 `work_ops ≥ 8`, Seek≤64 prove cap,
W4 HostInvoke **id** allowlists.

### Ranked B* board

Ship one ticket at a time. A4 measurement on every code PR. Prefer
regular reconstruct over bench-shaped peeps.

| # | Win | Why this rank | Prove / stay refuse |
|---|-----|---------------|---------------------|
| **B1** | Shrink leftover **entry** refuses for Q6–Q8 first rungs | **Landed** ([COI-339](https://linear.app/ardax/issue/COI-339)): `lir_eligible` / infer / island copy treat counted `for`, one-word rec `CALL`, and niche/two-slot `Br` as lift→cost, not checklist walls. Cost gate spirit unchanged | Counted `for`, one-word rec `CALL`, niche/two-slot `Br`. Do not force `tak` / `fib` / boxed match |
| **B2** | Q7 cost-gate lose on `fib` / `tak` | **Landed** ([COI-340](https://linear.app/ardax/issue/COI-340)): dense emit convoys one-word `CALL` like fuse-IL (stack args / results; Seek only when extras live). Cost gate unchanged | Checksum + embed wall **≤ fuse**. No skip-the-gate. Regular CALL reconstruct, not a tak opcode |
| **B3** | Two-slot `CALL` / `RETURN` (dense or LIR reconstruct) | Blocks `option_*` / `result_*` churn and Q8 leftovers. Same ABI hole as LIR `CALL` refuse | Churn helpers; flagships likely identical |
| **B4** | **Q9 R2** — `string::{from_bytes,to_bytes}` dense HostInvoke | Next named rung on the I4 ladder. Maps/effects like other I6 | Unit reconstruct; dense infer may open this host only |
| **B5** | Q6 later rungs — first-class range, then user `Iterator` | Counted majority is done. Range-as-value is `GetField` / heap. User `next` needs B3 + Q8 match | `for_in_*`; no full trait rewrite |
| **B6** | Unmapped alloc / grow / class `new` maps | `nsieve` (`Vec.push`), `binary_trees` (heap / classes). I5 maps cover `Make*`, not these residuals | Natural suite; cost gate still refuses boxed reconstruct |
| **B7** | Mutual recursion (later Q7) | After B2/B3. Sibling even/odd `TailCall` already exists; general mutual + two-slot rec stay refuse | Existing sibling + a mutual pair; no new opcode |
| **B8** | **I7** debugger-attached / `-Og` on MIR | True wall: VM debugger is fuse-IL. Deopt edges exist; emit still refuses. Needed before specialized bodies can be stepped | Debugger tests; no MIR stepping rewrite |
| **B9** | Q9 **R3–R4** — maps across `FORMAT`; unicode / regex only if an island says so | After R2. R1 already reconstructs table ops on LIR | No second Format lowering; no vanity string benches |

Parked (not B*): boxed multi-payload match, residual `Byte`/`Pow`/bitwise,
Cranelift P5, PGO.

## Non-goals

- Dual AST walkers / a second semantic IR
- Fuse-IL as a competing optimizer
- Permanent refuse rows for cost (those become gates)
- PGO, score-chasing, or env-gated opts
- Combining A0 with [language-quirks.md](language-quirks.md)
  ([COI-331](https://linear.app/ardax/issue/COI-331/q0-docs-language-quirksmd-locked-q1-q9))
- B3+ compiler work in a B2 PR (two-slot CALL/RETURN)

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
   gates**, not forever refuse.    Remaining walls: I7 native resume (C3 maps exist; P5 parked), leftover unmapped grow /
   class edges, boxed multi-payload
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
| `FORMAT` / `STRING` / `STRINGIFY` / `PRINT` / I4 bytes | **Q9 R1:** table string ops are SSA + MIR→LIR. Dense infer still refuses table ops. **R2:** `from_bytes` / `to_bytes` are I6 dense HostInvoke. **R3:** `FORMAT` / `STRINGIFY` take I5-style maps. Unicode / regex wait R4 | Ladder + cost (R1/R3 may keep fuse-IL) |
| Recursion on the callee (`CALL` / `TailCall`) | **Q7:** one-word self-`CALL` / `TailCall` may dense. **B2:** tight `tak` / `fib` convoy CALL on the stack (no prologue Seek / inter-CALL STORE) so the cost gate can keep them. **B3:** two-slot helper `CALL` / `RETURN` may dense or LIR. **B7:** sibling / mutual `TailCall` may dense (stack-arg + reserved callee entries). **C1:** self two-slot `CALL` / `RETURN` may dense or LIR (same dest + `dest_hi` machinery). N>2 stays refuse until extra dests + archive encoding | Cost gate |
| `for` / iterator (`for_in_sum`) | **Q6:** counted desugar — array / Vec / `[T; N]` / literal range helpers dense (`sum`, `for_in_range`). **B5:** first-class `let r = 0..n` locals unbox `[start,end]` (`for_in_range_value`). **C2:** free-fn param / returned numeric Range two-slot (`for_in_range_param` / `for_in_range_ret`). `main` + format / `Vec.push` stays fuse-IL. User `Iterator` / coro / dict / heap-field range still refuse | Ladder (counted + local + param/ret range) |
| Dense+match (stack vs regs) | **Q8:** niche / two-slot match may dense (register `Br`, cost gate). Boxed `JumpIfMatch` stays I2 LIR. **B3:** two-slot `CALL` / `RETURN` may dense or LIR (cost gate) | Cost gate |
| Multi-payload `Unpack` / `JumpIfMatch` arity > 1 | Unchanged — fuse-IL | Hard wall (later island) |
| Debugger-attached / `-Og` | **B8:** may dense / LIR (cost gate). **C3:** compiler-internal deopt maps, named-let remap, sparse MIR `DebugLoc`. Emit still skips `Deopt`. Native resume / P5 stay leftover ([mir-deopt.md](mir-deopt.md)) | Ladder (**I7** / **B8** / **C3**) |
| Unmapped alloc / GC safepoint | **B6:** `ArrayPush` / `DenseArrayPush` grow sites encode S2b maps; CALL+alloc drafts bind (one-word `CALL`). Cost gate still refuses boxed reconstruct. Multi-payload match stays a wall | Ladder + cost (maps); leftover unmapped edges stay fuse-IL |
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
| **B2** | Q7 cost-gate lose on `fib` / `tak` | **Landed** ([COI-340](https://linear.app/ardax/issue/COI-340)): dense emit convoys **self** one-word `CALL` like fuse-IL (stack args / results; Seek only when extras live). Sibling / mutual `TailCall` stays fuse-IL (B7). Cost gate unchanged. Flagship `fib` / `tak` archives stay identical (reconstruct fuse-selects to the same bytes) | Checksum + embed wall **≤ fuse**. No skip-the-gate. Regular CALL reconstruct, not a tak opcode |
| **B3** | Two-slot `CALL` / `RETURN` (dense or LIR reconstruct) | **Landed** ([COI-341](https://linear.app/ardax/issue/COI-341)): helper two-slot `CALL` / `RETURN` lower; LIR reconstructs width-2 `CALL`; dense emit may keep when cost ≤ fuse. Self / sibling two-slot recursion stays fuse-IL (B7 / later Q7) | `option_match_call`; checksum; cost gate; flagships flat |
| **B4** | **Q9 R2** — `string::{from_bytes,to_bytes}` dense HostInvoke | **Landed** ([COI-342](https://linear.app/ardax/issue/COI-342)): I6 dense HostInvoke (box at the host edge). Table `STRING` / `FORMAT` stay off dense | Unit reconstruct; dense loop + HostInvoke; format loop still fuse-IL |
| **B5** | Q6 later rungs — first-class range, then user `Iterator` | **Landed** ([COI-343](https://linear.app/ardax/issue/COI-343)): unboxed `let r = 0..n` locals (no `GetField`). Param / returned Range opened as **C2**; user `next`, coro, dict, heap-field dict stay refuse | `for_in_range_value`; no full trait rewrite |
| **B6** | Unmapped alloc / grow / class `new` maps | **Landed** ([COI-344](https://linear.app/ardax/issue/COI-344)): `ArrayPush` is a mapped grow safepoint; `DenseArrayPush` (archive **4.11**) is the native reconstruct. Map lift types one-word `CALL` so CALL+`Make*` / `InitTyped` drafts bind. `binary_trees` `item_check` still fuse-IL (multi-payload match). Cost gate still refuses boxed reconstruct | `nsieve`; CALL+enum wrap; flagships flat |
| **B7** | Mutual / sibling / self two-slot recursion | **Landed** ([COI-345](https://linear.app/ardax/issue/COI-345)): one-word and two-slot **sibling / mutual** `TailCall` may dense. Reconstruct puts TailCall args on TOS and reserves callee entry labels so `to_flat` does not treat a sibling jump as local (B2 break). B2 dest convoy stays self-`CALL` only. Leftover at B7: **self two-slot** `CALL` / `RETURN` (opened as **C1**) | `tail_sibling.hy`; even/odd + bounce + a mutual pair; checksum; flagships flat. No new opcode |
| **C1** | Self two-slot (→ N-slot) `CALL` / `RETURN` | **Landed** ([COI-349](https://linear.app/ardax/issue/COI-349)): drop the self two-slot specialize refuse. Helper / sibling / self share `ret_words` + dest + `dest_hi`. Keep/refuse is the cost gate. **N-slot reach:** infer/lower use `ret_words >= 2` capped at `MAX_MODELED_RET_WORDS` (2). N>2 needs extra dests + `CALL`/`RETURN` archive encoding | `self_two_slot.hy`; `option_self_call`; checksum; cost gate; flagships flat. No new opcode |
| **C2** | Param / returned heap Range; user Iterator / coro / dict later | **Landed** ([COI-351](https://linear.app/ardax/issue/COI-351)): numeric free-fn Range params and two-slot Range `CALL`/`RETURN`. Counted `for` without `GetField`. User `Iterator` / coro / dict / heap-field dict stay refuse | `for_in_range_param`; `for_in_range_ret`; checksum; cost gate; flagships flat. No full trait rewrite |
| **C3** | Native deopt maps / named let slots / sparse DebugLoc | **This rung** ([COI-350](https://linear.app/ardax/issue/COI-350)): compiler-internal `DraftDeoptMap` at leave edges; remap `fn_debug_locals` after SSA regs; forward known locs on dense / LIR emit. No archive bump. Do not set `allow_deopt` in production. Leftover: P5 resume, incomplete convoy maps, per-PC locals, codegen-unknown locs | Debugger + `-Og` boards; deopt drafts; named `let`; sparse locs; checksum; flagships flat |
| **B8** | **I7** debugger-attached / `-Og` on MIR | **Landed** ([COI-346](https://linear.app/ardax/issue/COI-346)): drop the blanket specialize refuse. Debugger-attached / `-Og` may dense / LIR; emit skips explicit `Deopt`. C3 ladders maps / named lets / locs | Debugger + `-Og` boards; fib `print n`; checksum; flagships identical on Standard |
| **B9** | Q9 **R3–R4** — maps across `FORMAT`; unicode / regex only if an island says so | **This rung (R3):** `FORMAT` / `STRINGIFY` are mapped GC safepoints (I5 roots). LIR when maps bind and cost ≤ fuse. Dense infer still refuses table ops. **R4 leftover:** unicode / regex stay out of SSA | Format/string boards; live-string map; checksum; cost gate; flagships flat. No second Format lowering |

Parked (not B*): boxed multi-payload match, residual `Byte`/`Pow`/bitwise,
Cranelift P5, PGO.

## Non-goals

- Dual AST walkers / a second semantic IR
- Fuse-IL as a competing optimizer
- Permanent refuse rows for cost (those become gates)
- PGO, score-chasing, or env-gated opts
- Combining A0 with [language-quirks.md](language-quirks.md)
  ([COI-331](https://linear.app/ardax/issue/COI-331/q0-docs-language-quirksmd-locked-q1-q9))
- B4+ compiler work in a B3 PR (Q9 R2 bytes)

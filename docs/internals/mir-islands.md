# MIR language islands (COI-292 I0)

MIR is a **typed SSA sidecar** that grows **island by island**. It is not a
wholesale rewrite of the compiler, and it is not a second semantic language
IR.

Linear project: [MIR language islands](https://linear.app/ardax/project/mir-language-islands-01a45453f2f1).
Numeric dense / LIR history stays in [mir.md](mir.md) and the
[Coil Low IR](https://linear.app/ardax/project/coil-low-ir-51e2b0cb7b46) project.
P5 Cranelift stays parked there.

## Doctrine

**IL stays lowering + fuse-select.** Stack IL is instruction lowering: emit
`IlOp` with symbolic labels, opt in place, one `il::lower` (fuse-select, assign
PCs, encode). Names, types, and call meaning live in DefIds and the typed
sidecar. See [pipeline.md](pipeline.md) (IL intent). Do not promote fuse-IL
into a semantic IR, and do not add a dual AST walker.

**MIR stays a sidecar.** Eligible bodies may lower to SSA, then emit dense
opcodes, MIR→LIR (stack IL reconstruct), or a HostInvoke pack. Everything
else stays fuse-IL. Refuse is a feature: each island documents what still
does not enter MIR. Generalization defaults (MIR first, fuse-IL fallback,
one object story, cost gate): [opt-generalization.md](opt-generalization.md).

**Coverage first, score-chasing never.** Islands exist so more of the
*language* (match, classes, strings, GC coordination, effects, debugger
boundaries) can ride typed SSA. A PR must not exist only to invent a
microbench or bump a flagship. Identical flagship `.hyc` is OK when the
island does not fire there.

**Register VM / Cranelift are later levers**, not prerequisites. Do not
block islands on P5. Do not revive PGO.

**I8 entry (post I1–I3 / A3).** After stack-IL opts, `IlModule` tries dense
specialize, then IL→MIR→LIR when [`lir_eligible`](../../compiler/src/mir/entry.rs)
has **no hard refuse**. Hard refuse today: I4 strings (Q9 **reopens** I4
as a delivery ladder — see [language-quirks.md](language-quirks.md)),
unmapped I5 alloc, I6 `CALL` / HostInvoke (LIR emit cannot reconstruct),
escaping fields, box, I2 multi-payload `Unpack`, I7 debugger-attached /
`-Og`. Heap index is not a wall after A2. No dual AST walker. No
work-op / Seek≤64 / HostInvoke **id** floors — those are purity bits +
cost. Cost gate: replace when LIR emit ≤ opted fuse-IL, with +3 slack for
I2 match / two-slot construct (runtime-neutral `Seek`). Leftover lets stay
strict so ConstReturnImm fuse is not undone.

## Island ladder

| # | Island | Issue | Outcome | Status |
|---|--------|-------|---------|--------|
| I0 | Doctrine + refuse map | [COI-292](https://linear.app/ardax/issue/COI-292/i0-mir-islands-doctrine-refuse-inventory) | This note; feature → path → target island | on main (#340) |
| I1 | Heap / niche types | [COI-293](https://linear.app/ardax/issue/COI-293/i1-heap-niche-types-in-mir-lattice) | `MirTy` / `MirLayout` name heap-ref + niche Option/Result Value words; infer/lower may carry them; no GC maps; no specialize of allocating/escaping bodies | on main (#342) |
| I2 | Match on niche / two-slot / boxed overlap | [COI-294](https://linear.app/ardax/issue/COI-294/i2-match-on-niche-two-slot-in-mir) / [COI-302](https://linear.app/ardax/issue/COI-302/after-unlock-i2-boxedconstructmatch-cost-gate) | JumpIfMatch-shaped control in MIR → LIR; arity 0 overlap + any tag; niche `LogNot` / two-slot tag `Br`; dense still refuses | this PR |
| I3 | Non-escaping class fields | [COI-295](https://linear.app/ardax/issue/COI-295/i3-non-escaping-class-fields-in-mir) | Field load/store using the existing local-escape sidecar; escaping named locals stay fuse-IL | on main (#344) |
| I4 | String / format subset | [COI-296](https://linear.app/ardax/issue/COI-296/i4-string-format-mir-subset-or-refuse) / [COI-332](https://linear.app/ardax/issue/COI-332) Q9 | **Reopened (Q9).** Today still fuse-IL (`FORMAT` / `STRING` / `STRINGIFY` / `PRINT`); target is full format/string on MIR as a **delivery ladder**, not a permanent barrier. No half-format second lowering; no vanity string bench | #345 landed the old barrier; Q9 reopens |
| I5 | Alloc + GC barriers | [COI-300](https://linear.app/ardax/issue/COI-300/i5-alloc-gc-barriers-in-mir) / [COI-305](https://linear.app/ardax/issue/COI-305/s2a-live-root-sidecar-at-mir-gcbarrier-alloc) / [COI-306](https://linear.app/ardax/issue/COI-306/s2b-slot-frame-stack-maps-for-interpreter-gc) / [COI-307](https://linear.app/ardax/issue/COI-307/s2c-specialize-lir-across-alloc-when-maps-exist) / [COI-314](https://linear.app/ardax/issue/COI-314/s2d-map-backed-looping-alloc-further-alloc-opts) | MakeArray / alloc edges; S2a live-root sidecar; S2b interpreter slot / frame maps; S2c specialize / LIR across alloc when maps exist; S2d mapped in-loop / preheader Make* | I5 / S2a–S2c on main; S2d this PR |
| I6 | Effects / HostInvoke | [COI-297](https://linear.app/ardax/issue/COI-297/i6-effects-hostinvoke-as-mir-edges) | Broader than W4 allowlist; purity sidecar drives barriers | on main (#347) |
| I7 | Debugger / deopt | [COI-299](https://linear.app/ardax/issue/COI-299/i7-debugger-deopt-boundaries-on-mir) | Deopt / stop metadata on MIR edges; VM debugger stays source of truth | on main (#348) |
| I8 | Broaden MIR emit | [COI-298](https://linear.app/ardax/issue/COI-298/i8-broaden-mir-emit-entry-post-i1-i3) / [COI-301](https://linear.app/ardax/issue/COI-301/unlock-retarget-i8-shape-tests-broaden-lir-eligible) / [COI-336](https://linear.app/ardax/issue/COI-336) A3 | Lift when there is no hard refuse; keep via cost gate (no work-op / Seek≤64 / host-id floors) | this PR |

I4 was closed as a hard MIR barrier (#345). **Q9 (2026-09-10) reopens
it** as a phased island: full format/string on MIR, not a permanent
refuse. Do not land a half Format second lowering. Unicode / regex stay
out of MIR until a later cut. Spec:
[language-quirks.md](language-quirks.md).

## Feature → today path → target island

| Language / IL feature | Today | Target |
|-----------------------|-------|--------|
| Numeric loops / straight-line (`i32`/`i64`/`f32`/`f64`/`bool`) | dense (`DenseBin` …) after MIR CSE/LICM/peeps when cost ≤ fuse-IL | stay dense (Low IR) |
| HostInvoke inside numeric | dense + box/unbox at host edge; LICM hoists scalar-pure (purity bits); `packed_*` stays in-place | S3 emits I6-typed hosts except I4 `from_bytes` / `to_bytes` |
| One-word `CALL` (dense map or open fuse-IL / LIR) | dense | S3 + **Q7** one-word self-recursive `CALL` / `TailCall` (`tak` / `fib`). Two-slot / niche / mutual still refuse; LIR still cannot reconstruct `CALL` |
| Saxpy-reduce | HostInvoke `simd_axpy_reduce` (P12) | stay |
| Stride-1 numeric store (`v[i] = i` / zip / scale) | `VLoad` / `VStore` / `VBin` / `VMove` (S5a V0) | stay |
| Stride-1 add-reduce / conservative FMA | `VReduce` / `VFma` (S5b V1) | stay; no fast-math |
| `Option<int>` / immediate-Ok `Result` / arity-2 immediate product leafs | P3 MIR→LIR when reconstruct ≤ opted fuse-IL; else fuse-IL | I2 for match; I1 names the layout only; **Q8** commits near-term dense+match (not only LIR) |
| Heap `Option<T>` / heap-heap `Result<T,E>` (COI-92 niche words) | fuse-IL (`CONST 0` / `BITAND` / `BITOR`); layout already `HeapNiche` | I1 SSA types; I2 match; not dense |
| Nested / mixed / `CallIndirect` Option/Result | boxed `ObjEnum` + fuse-IL | stay refuse until a later island says otherwise |
| `match` / `JumpIfMatch` on niche / two-slot / boxed unary (any tag, arity ≤ 1; arity 0 overlap; last-arm `Unpack` writes the reserved slot) | MIR→LIR when reconstruct ≤ opted fuse-IL; else fuse-IL | **I2**; **Q8** dense+match |
| `match` / `JumpIfMatch` on boxed multi-payload (`Unpack` arity > 1) / user polymorphism | fuse-IL | stay refuse |
| Class fields (escaping / heap-backed) | fuse-IL | stay refuse (I3 is **non-escaping** only) |
| Non-escaping named class locals (sidecar) | MIR→LIR `FieldLoad` / `FieldStore` on unboxed slots | **I3** |
| Heap index / `MakeArray` / alloc | SSA `Alloc` + `GcBarrier` when `allow_alloc`; S2a–S2l + **A2** dense-native `Index` / `Make*` / `DensePush`; keep when maps exist and cost ≤ fuse-IL. In-loop `Vec.push` / class `new` stay fuse-IL. Dense+match stays LIR (I2) | **I5** / **S2** / **A2** |
| `FORMAT` / string ops | fuse-IL (`IlOp::Byte` / `String` / `Print`) | **I4** / **Q9** — reopened delivery ladder (today still fuse-IL) |
| Impure HostInvoke / IO / clocks / GC natives | SSA edge + barrier; S3 dense emit (except I4 string bytes); LICM never hoists impure | **I6** / **S3** |
| Debugger stops / deopt | SSA `Deopt` + implicit leave edges; debugger-attached / `-Og` refuse dense + LIR | **I7** |
| Recursion (`tak` / `fib`) | fuse-IL on tight leafs; dense when cost ≤ fuse | **Q7** (this PR). Convoy fused returns unfuse; one-word self-`CALL` / `TailCall` may dense. `tak` / `fib` lose the cost gate (Seek + STORE). Mutual / two-slot stay refuse |
| `for` / iterators | **Q6 counted desugar** on array / Vec / `[T; N]` / literal range helpers (`for_in_sum` `sum`, `for_in_range`); `main` + format and user `Iterator` / coro / dict / first-class range stay fuse-IL | phased ladder — [q6-iterator-protocol.md](q6-iterator-protocol.md); not a permanent fuse-IL ceiling |
| Residual `Byte` / `Pow` / `AND`/`OR` | fuse-IL | stay unless a later island has a regular reason |
| Cranelift / native | parked (P5) | not an island delivery vehicle |

Dense refuse rows that stay current: [specialize-refuse.md](specialize-refuse.md).

## A/B rules (every island PR)

Generalization PRs also follow [opt-generalization.md](opt-generalization.md)
(A4): natural suites (`nsieve`, `binary_trees`, `nbody`, `for_in_sum`), no
env toggles.

1. **Prefer `coil-embed`.** Same host protocol; fixed VM image when comparing
   compilers. Fat-`coil` LTO noise is not a MIR regression.
2. **Flagships** (`mandelbrot`, `tak`, `nsieve`, `binary_trees`, `fib`):
   checksums match; wall time **flat (±5%) or better** on embed when archives
   differ. Prefer **identical numeric archives** when the island does not
   fire there.
3. **Prove on real language surface.** Existing `.hy` tests + natural
   examples that already exercise the feature. Do **not** invent a
   synthetic hit bench whose only job is a number.
4. **Identical flagship `.hyc` is not a skip** when the island *does* change
   other bodies — report those archives too (for example I2 on
   `match_sum` / `result_*` if they change). It *is* enough when the island
   is lattice-only or docs-only.
5. **No PGO. No score-chasing.** Skip merge only on a real wash/regress of
   a body the island claims to touch, or a flagship miss outside ±5% when
   archives differ.

I0 is docs-only (on main as #340 / `347ec5a`). I1 must still publish the
embed A/B table vs that parent (not the earlier COI-291 tip `30cf992f`).

## Non-goals

- Dual AST walkers / a second semantic IR replacing DefIds
- Full-MIR rewrite of every function
- PGO revival
- LLVM / Cranelift as the island delivery vehicle
- Work that exists only to move one named microbench

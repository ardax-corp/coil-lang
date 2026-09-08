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
does not enter MIR.

**Coverage first, score-chasing never.** Islands exist so more of the
*language* (match, classes, strings, GC coordination, effects, debugger
boundaries) can ride typed SSA. A PR must not exist only to invent a
microbench or bump a flagship. Identical flagship `.hyc` is OK when the
island does not fire there.

**Register VM / Cranelift are later levers**, not prerequisites. Do not
block islands on P5. Do not revive PGO.

**I8 entry (post I1–I3).** After stack-IL opts, `IlModule` tries dense
specialize, then IL→MIR→LIR for leftovers that [`lir_eligible`](../../compiler/src/mir/entry.rs)
accepts for a **named reason**: two-slot `RETURN`, I2 match, I3 unboxed
fields, or I1 niche `BITAND`/`BITOR`. Plain `if` diamonds and store-only
loops stay fuse-IL. Hard refuse: I4 strings, I5 alloc, I6 `CALL` / HostInvoke,
heap index / escaping fields, I2-out-of-range match, I7
debugger-attached / `-Og`. No dual AST walker. Cost gate: replace only
when LIR emit ≤ opted fuse-IL.

## Island ladder

| # | Island | Issue | Outcome | Status |
|---|--------|-------|---------|--------|
| I0 | Doctrine + refuse map | [COI-292](https://linear.app/ardax/issue/COI-292/i0-mir-islands-doctrine-refuse-inventory) | This note; feature → path → target island | on main (#340) |
| I1 | Heap / niche types | [COI-293](https://linear.app/ardax/issue/COI-293/i1-heap-niche-types-in-mir-lattice) | `MirTy` / `MirLayout` name heap-ref + niche Option/Result Value words; infer/lower may carry them; no GC maps; no specialize of allocating/escaping bodies | on main (#342) |
| I2 | Match on niche / two-slot | [COI-294](https://linear.app/ardax/issue/COI-294/i2-match-on-niche-two-slot-in-mir) | JumpIfMatch-shaped control in MIR → LIR; niche `LogNot` / two-slot tag `Br`; dense still refuses | on main (#343) |
| I3 | Non-escaping class fields | [COI-295](https://linear.app/ardax/issue/COI-295/i3-non-escaping-class-fields-in-mir) | Field load/store using the existing local-escape sidecar; escaping named locals stay fuse-IL | on main (#344) |
| I4 | String / format subset | [COI-296](https://linear.app/ardax/issue/COI-296/i4-string-format-mir-subset-or-refuse) | **Hard refuse.** `FORMAT` / `STRING` / `STRINGIFY` / `PRINT` stay fuse-IL; no subset, no vanity string bench | on main (#345) |
| I5 | Alloc + GC barriers | [COI-300](https://linear.app/ardax/issue/COI-300/i5-alloc-gc-barriers-in-mir) | MakeArray / alloc edges; safepoint / root placeholders; refuse specialize across GC until maps exist | on main (#346) |
| I6 | Effects / HostInvoke | [COI-297](https://linear.app/ardax/issue/COI-297/i6-effects-hostinvoke-as-mir-edges) | Broader than W4 allowlist; purity sidecar drives barriers | on main (#347) |
| I7 | Debugger / deopt | [COI-299](https://linear.app/ardax/issue/COI-299/i7-debugger-deopt-boundaries-on-mir) | Deopt / stop metadata on MIR edges; VM debugger stays source of truth | on main (#348) |
| I8 | Broaden MIR emit | [COI-298](https://linear.app/ardax/issue/COI-298/i8-broaden-mir-emit-entry-post-i1-i3) | More bodies enter MIR from IL→MIR lift — infer+lower, not specialize-from-IL accidents | this PR |

I4 is closed as a **hard MIR barrier** (not a type-lattice ticket). No
narrow string allowlist: nothing in the current language suite needed
`STRING`/`FORMAT` on SSA, and lifting Format halfway would be a second
lowering. Unicode / regex stay out of MIR.

## Feature → today path → target island

| Language / IL feature | Today | Target |
|-----------------------|-------|--------|
| Numeric loops / W3 straight-line (`i32`/`i64`/`f32`/`f64`/`bool`) | dense (`DenseBin` …) after MIR CSE/LICM/peeps | stay dense (Low IR) |
| Allowlisted HostInvoke inside numeric (W4) | dense + box/unbox at host edge | stay W4 (closed); I6 does not grow it for benches |
| Dense→dense one-word `CALL` (COI-291) | dense | stay; two-slot / niche / recursion still refuse |
| Saxpy-reduce | HostInvoke `simd_axpy_reduce` (P12) | stay |
| `Option<int>` / immediate-Ok `Result` / arity-2 immediate product leafs | P3 MIR→LIR when reconstruct ≤ opted fuse-IL; else fuse-IL | I2 for match; I1 names the layout only |
| Heap `Option<T>` / heap-heap `Result<T,E>` (COI-92 niche words) | fuse-IL (`CONST 0` / `BITAND` / `BITOR`); layout already `HeapNiche` | I1 SSA types; I2 match; not dense |
| Nested / mixed / `CallIndirect` Option/Result | boxed `ObjEnum` + fuse-IL | stay refuse until a later island says otherwise |
| `match` / `JumpIfMatch` on niche / two-slot (tags 0/1, arity ≤ 1) | MIR→LIR when reconstruct ≤ opted fuse-IL; else fuse-IL | **I2** |
| `match` / `JumpIfMatch` on boxed multi-payload / user polymorphism | fuse-IL | stay refuse (not I2) |
| Class fields (escaping / heap-backed) | fuse-IL | stay refuse (I3 is **non-escaping** only) |
| Non-escaping named class locals (sidecar) | MIR→LIR `FieldLoad` / `FieldStore` on unboxed slots | **I3** |
| Heap index / `MakeArray` / alloc | SSA `Alloc` + `GcBarrier` when `allow_alloc`; emit still fuse-IL (no stack maps) | **I5** |
| `FORMAT` / string ops | fuse-IL (`IlOp::Byte` / `String` / `Print`) | **I4 barrier** — no MIR subset |
| Non-allowlisted HostInvoke / IO / clocks / GC natives | SSA edge + barrier when `allow_effects`; production fuse-IL / refuse dense | **I6** |
| Debugger stops / deopt | SSA `Deopt` + implicit leave edges; debugger-attached / `-Og` refuse dense + LIR | **I7** |
| Recursion (`tak` / `fib`) | fuse-IL (`CALL` / `TailCall`) | stay refuse (leaf-first dense map) |
| Residual `Byte` / `Pow` / `AND`/`OR` | fuse-IL | stay unless a later island has a regular reason |
| Cranelift / native | parked (P5) | not an island delivery vehicle |

Dense refuse rows that stay current: [specialize-refuse.md](specialize-refuse.md).

## A/B rules (every island PR)

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

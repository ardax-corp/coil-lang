# Superinstruction / fusion candidates ([COI-378](https://linear.app/ardax/issue/COI-378) S0)

Docs-only inventory for Linear project
[Superinstructions](https://linear.app/ardax/project/superinstructions-54baee2beca8).
Tip `cf7a193c` (`main`). No compiler or VM behavior change.

Architect: refine **S1–S8** from the tables here. Do not re-dissect unless a
ticket’s shape disagrees with the dumps.

Doctrine (project):

1. Emit wherever the body already lives (dense lower **and** fuse-IL peeps).
   Superinstructions are **not** gated on `try_specialize_body` keep.
2. Do not densify printing `main` / `FORMAT`.
3. Do not revive tombstoned `FloatChainStore` / `BinSlotSlotConstJmpf`.
4. Archive-minor packed ops; checksums identical.
5. One fusion shape + hit bench + dissect proof per ticket.

Related: [opcodes.md](opcodes.md), [specialize-refuse.md](specialize-refuse.md),
[opt-generalization.md](opt-generalization.md), [pipeline.md](pipeline.md),
`compiler/src/il/lower.rs` (`fuse_select`), `machine/src/fused.rs`.
Dumps: `artifacts/superinstructions/`.

---

## S1–S8 (refine these tickets)

Layer key: **dense emit** = pack in `mir/emit.rs` / `vectorize.rs` before
ops become residual `IlOp::Byte`. **IL peep** = `fuse_select` / convoy /
instcombine on typed `IlOp`. **Gate lift** = typed-IL / SP / refuse table.

**S8 fact that binds every dense ticket:** `IlOp::from_plain_byte` does
**not** lift `Dense*` / `V*` (`compiler/src/il/op.rs` falls through to
`IlOp::Byte`). `fuse_select` marks `IlOp::Byte` as `Slot::Cold` and
refuses any window that includes it. Dense packing **cannot** be a
post-concat fuse-select peep unless S8 lifts those ops to typed `IlOp`.
Until then S1–S5 are dense-emit (or VM peek) only.

| ID | Linear | Ticket shape | Dissect (this PR) | Layer | Refine |
|----|--------|--------------|-------------------|-------|--------|
| **S1** | [COI-377](https://linear.app/ardax/issue/COI-377) | Dense cmp/bin → jmp | `mandelbrot` inner: `DenseBin×3 ; BinSlotSlotJmpf GTF`. Headers: `DenseMove* ; BinSlotSlotJmpf GT` (no `DenseCmp`). `emit_br_cond` reconstructs stack cmp so fuse-select already made `*Jmpf` (1 dispatch). | dense emit | The escape test is **`DenseBin` (mag) + fused `BinSlotSlotJmpf`**, not `DenseCmp`+jmp. A `DenseCmpJmpf` only wins if emit stops going through stack `Bin`+`Jump`. Packing `DenseBin; *Jmpf` is the real 2→1. Outer `y`/`x` headers are already 1-op jumps. |
| **S2** | [COI-381](https://linear.app/ardax/issue/COI-381) | 2-wide `DenseBin` pack | `mandelbrot` has **18** `DenseBin`. Inner latch is `DenseBin×6 ; DenseMove ; JMP`. | dense emit | **Done (ISA pack).** Two-word `DenseBin2` (archive **minor 16**): first `dense_abc` on the opcode, payload word is the second `DenseBin` (chosen over a pool descriptor so the second packing stays in the instruction stream). Sequential IEEE; `mir/emit.rs` `pack_chained_dense_bin`. Inner latch `DenseBin×6` → three `DenseBin2`. **Not** X4 VM peek, **not** `FloatChainStore`. `fib` bytecode is unchanged; giant-match layout can move `fib` wall. |
| **S3** | [COI-380](https://linear.app/ardax/issue/COI-380) | `DenseCast`+`DenseBin` | Three `DenseCast` sites, all **x/y headers** (`DenseCast ; DenseBin×3`), not the innermost `iter` loop. | dense emit | Flagship wall may be noise (cast is `160²` / `160`, not `160²×50`). Hit-bench the x-loop or a dedicated i2f kernel if mandelbrot A/B is wash. |
| **S4** | [COI-379](https://linear.app/ardax/issue/COI-379) | `DenseIndex` + imm cmp + jmpf | `nsieve` p-loop: `DenseIndex ; BinSlotImmJmpf EQ` (`flags[p] == 1`). | dense emit | Shape matches. Stacks with [COI-372](https://linear.app/ardax/issue/COI-372). Index is dense-native (not `IndexPin`). |
| **S5** | [COI-382](https://linear.app/ardax/issue/COI-382) | Index/StoreIndex stride pair | Inner k-loop is **`DenseMove ; DenseStoreIndex ; DenseBin ; JMP`** — store of 0 with `k = k + p`. **No** `DenseIndex` in that loop. | dense emit | Refine to **stride `DenseStoreIndex` + IV bump** (clear-flags), not consecutive index+store of the same cell. Optional pack `DenseStoreIndex ; DenseBin`. |
| **S6** | [COI-383](https://linear.app/ardax/issue/COI-383) | `DenseMove` coalesce | `mandelbrot` **9** `DenseMove` (φ / latch copies). Pairs: `DenseMove ; DenseMove ; BinSlotSlotJmpf`. | MIR destprop / dense emit | Peep-only is enough. Must not flip cost-gate keep (LOAD/STORE cost 2 vs `DenseMove` 1). |
| **S7** | [COI-384](https://linear.app/ardax/issue/COI-384) | Fuse-IL peep parity for S1–S5 shapes | `fib`/`tak`/`item_check`/`bottom_up` stay fuse-IL. Stack `*Jmpf` / `BinSlotImm` / `BinReturn` **already fire** (`fib` is 7 ops). Dense S1–S5 shapes do not appear on these bodies. | IL peep | Real fuse-IL gaps are **not** S1–S5 copies: `item_check` `CONST ; LOAD ; ADD` (canon Unknown-SP after match); `bottom_up` `MakeEnum ; RETURN`. Do not densify `main`/FORMAT. Pick one of those as the S7 hit, or drop S7 if “parity” meant “already true for stack fuses.” |
| **S8** | [COI-385](https://linear.app/ardax/issue/COI-385) | Fusion not blocked on specialize keep | Cost gate / `lir_eligible` do **not** currently refuse because a packed op appeared (none exist yet). The live block is **`Dense*` as residual `Byte` → fuse-select Cold**. `emit_cost` weights `Seek` and LOAD/STORE, not opcode novelty. | typed `IlOp` for `Dense*` **or** keep packing in dense emit | One-line lift if S1–S5 should share fuse-select: lift `DenseBin`/`DenseCast`/`DenseIndex`/`DenseStoreIndex`/`DenseMove` in `from_plain_byte`. Do **not** make specialize keep a prerequisite. Print `main` stays fuse-IL by design. |

Kick order (project ladder, still valid): **S8 note first** (this doc) → **S2** (landed, two-word `DenseBin2`) → **S1** (inner escape, after clarifying `DenseBin`+jmp not `DenseCmp`) → **S4** (nsieve, clear dump) → **S6** → **S3** (colder) → **S5** (after shape refine) → **S7** (after choosing a real fuse-IL hit).

---

## Beyond S1–S8 — ready-to-kick without specialize-keep

These are **not** on the S1–S8 ladder. They work on bodies that **already**
kept dense / vectorize / fuse-IL. No `try_specialize_body` change.

| Extra | Shape | Hit | Layer | Why it is independent |
|-------|-------|-----|-------|------------------------|
| **X1** | Vectorize remainder `i += 1`: `DenseConst` into scratch instead of `CONST ; STORE ; DenseBin` | `sum` / `scan` / `fill` scalar tails (`artifacts/.../for_in_sum.fn.txt`, `vec_scan.*.fn.txt`) | `mir/vectorize.rs` `emit_const_i64` (today stack `IlOp::Const` + `StorePop`) vs `emit.rs` `emit_const` | Already vectorized. 3 dispatches → 2 with **existing** `DenseConst`. Smallest patch. |
| **X2** | `DenseBin ; JMP` latch coalescing | mandelbrot / nsieve / SIMD tails | dense emit or VM peek | Not S1 (not a cmp) and not S2 (not two bins). Counted-loop latch. |
| **X3** | `MakeEnum ; RETURN` fuse (one-word heap) | `bottom_up` (`MakeEnum ; RETURN` twice) | IL peep — `MakeEnum` is **typed** `IlOp`, `Return` is typed | Body is fuse-IL; fuse-select simply has no pattern. Reuse `is_one_word_return`. |
| **X4** | VM coalescing of adjacent `DenseBin` **without** a new opcode | same as S2 | `machine/src/vm.rs` peek | **Not shipped** — S2 took the ISA pack (`DenseBin2`). Pick one; do not add peek on top. |

Not ready without a gate (still not specialize-keep):

| Extra | Gate |
|-------|------|
| `item_check` `CONST ; LOAD ; ADD` → `BinSlotImm` | `il/canon.rs` refuses Unknown SP (match join). Candidate S7 hit, not a new opcode. |
| `BinSlotImm` for pool float imm | `is_int_bin_op` / `const_inline_value`. No flagship still on fuse-IL float. |

Blocked (do not file as kick tickets): revive `FloatChainStore`; CALL-prep superinstructions (`fib`/`tak` — frame dominates); fuse across `FuseHint::nofuse_value_under_jmp`; two-word `RETURN` into `*Return`; residual `Byte` windows (FORMAT/FFI/`Seek`); dense infer `Pow`/`AND`/`OR`; LIR HostInvoke reconstruct; scalar IEEE FMA.

---

## Existing fused / packed / dense ops

### Fuse-select (`compiler/src/il/lower.rs`)

One named pass after concat. Residual `IlOp::Byte` is `Slot::Cold`.
Labels / `JoinLabel` / `FuseHint` are barriers. Per-function fuse is not
production.

| Opcode | Window | Emit |
|--------|--------|------|
| `BinSlotImmJmpf` / `BinSlotImmJmpt` | `LOAD; CONST; cmp; JMPF/T` or `BinSlotImm; JMPF/T` | fuse-select |
| `BinSlotSlotJmpf` / `BinSlotSlotJmpt` | `LOAD; LOAD; cmp; JMPF/T` or `BinSlotSlot; JMPF/T` | fuse-select |
| `CmpJmpf` / `CmpJmpt` | stack `cmp; JMPF/T` | fuse-select |
| `LogNotJmpf` / `LogNotJmpt` | `LogNot; JMPF/T` | fuse-select |
| `BinSlotImmStore` | `LOAD; CONST; int-bin; STORE` | fuse-select (`is_int_bin_op` only) |
| `BinSlotSlotStore` | `LOAD; LOAD; bin; STORE` | fuse-select (int+float) |
| `BinSlotImm` | `LOAD; CONST; int-bin` | fuse-select (**int only**) |
| `BinSlotSlot` | `LOAD; LOAD\|DUP; bin` | fuse-select (int+float) |
| `LoadReturnSlot` / `ConstReturnImm` / `BinReturn` | producer + one-word `RETURN` | fuse-select + convoy |
| packed `LOAD`/`STORE` n=2/3 | adjacent singles | fuse-select |

`invert_branch_over_jump`: `JMPF A; JMP B; A:` → `JMPT B`. **Loop headers
stay `*Jmpf`** (COI-87).

### Bounds / pin (not fuse-select)

`IndexUnchecked` / `StoreIndexUnchecked` / `ArrayPin` / `IndexPin*` from
`loop_bounds`. Flagship dense bodies use `DenseIndex` / `DenseStoreIndex`
instead.

### MIR specialize / SIMD

`DenseBin` / `DenseBin2` / `DenseCmp` / `DenseConst` / `DenseMove` / `DenseUnary` /
`DenseCast` / dense heap / field / `V*` from `emit_dense` / `vectorize`.
`DenseBin2` is a two-word ISA pack of adjacent `DenseBin` (COI-381); X4 VM peek
is not shipped.

`DenseCmp` is emitted for SSA **values**. Branchy compares go through
`emit_br_cond` (stack `LOAD`/`BinSlotImm`/`Bin` + `Jump`) so fuse-select
emits `*Jmpf`. Measured dense loops contain **zero `DenseCmp`**.

### Tombstones (not selected)

`FloatChainStore` and `BinSlotSlotConstJmpf` panic in the VM. Tests assert
they are not emitted. `BinSlotSlotConstJmpt` still has a live handler.

---

## Refuse / block sites (S8 input)

| Site | Blocks | Why |
|------|--------|-----|
| `il/op.rs` `from_plain_byte` `_ => Byte` | fuse-select on `Dense*` / `V*` / FORMAT / FFI / `Seek` | residual `Byte` is Cold |
| `il/lower.rs` `fuse_slots_with_origins` | fuse across labels / abs JMP / uncond join into `*Return` | stacked arm values |
| `il/op.rs` `FuseHint::nofuse_value_under_jmp` | `cmp; JMPF` fuse | pair-`?` / pair-match payload under cond |
| `il/lower.rs` `is_one_word_return` | `*Return` on `RETURN` width 2 | would drop hi word |
| `il/lower.rs` `is_int_bin_op` | float `BinSlotImm` | pool `CONST` |
| `il/lower.rs` `try_fuse_slots` | `FloatChainStore` / `BinSlotSlotConstJmpf` | “not emitted” |
| `il/canon.rs` | `Const; Load; op` swap | Unknown SP; float; non-commutative ops |
| COI-87 / `opt/cfg.rs` | invert loop headers to `*Jmpt` | headers stay `*Jmpf` |
| `seek_back_edge` | Seek latch on Standard | off except `-O3` |
| `mir/entry.rs` `hard_refuse` | MIR→LIR leftover | Call / Host / heap field / Box / unmapped alloc |
| `mir/specialize.rs` | keep dense | no arith; unmapped alloc; post-loop `return [x]`; cost `emit_cost`; residual heap box |
| `mir/infer.rs` | dense infer | residual `Byte`; `Pow`/`AND`/`OR`; Q9 table string ops |
| `mir/string_barrier.rs` | dense string | R1: table STRING/PRINT/FORMAT/STRINGIFY |
| `mir/emit.rs` | dense emit | maps, untyped host, I4 print, unboxed Field (I3 is LIR) |
| `mir/emit.rs` `emit_br_cond` | `DenseCmp` on branches | stack reconstruct for `*Jmpf` |
| `mir/vectorize.rs` | `V*` | GC/deopt; Call/Host/match; not a single counted latch+Return |
| `mir/abi.rs` `MAX_MODELED_RET_WORDS = 2` | N>2 CALL/RETURN | C1b leftover |

`lir_eligible` is the leftover MIR→LIR reconstruct wall, **not** a fusion
entry tax. New packed opcodes must not be added to `hard_refuse` /
`emit_cost` in a way that drops a keep that already won (S8).

---

## Method / checksums

Release `coil` / `coil-dissect`, `COIL_AUTO_PAR=0`,
`--root .deps/coil-stdlib/src`. No `coil dissect file.hyc`; dissect is
in-memory on the same `Pipeline`. `--opt-stats` is IL-pass counters only.

| Bench | Checksum | Hot body | Path |
|-------|----------|----------|------|
| `examples/perf/mandelbrot.hy` | `625885` | `mandelbrot` | dense + fused `*Jmpf` |
| `examples/perf/fib.hy` | `2178309` | `fib` | fuse-IL + B2 CALL convoy |
| `examples/perf/tak.hy` | `7` | `tak` | fuse-IL + packed `LOAD` + `TailCall` |
| `examples/perf/nsieve.hy` | `1900` | `nsieve` | dense-native Index/Store + `*Jmpf` |
| `examples/perf/binary_trees.hy` | `135854` | `item_check`, `bottom_up` | fuse-IL |
| `examples/perf/for_in_sum.hy` | `12884115456` | `sum` | `VLoad`/`VReduce` + scalar tail |
| `examples/perf/vec_scan.hy` | `536739840` | `scan`, `fill` | `V*` + scalar tail |

```bash
export COIL_AUTO_PAR=0
BIN=./target/release/coil
DISSECT=./target/release/coil-dissect
R=(--root .deps/coil-stdlib/src)
$BIN compile "${R[@]}" examples/perf/mandelbrot.hy -o /tmp/m.hyc
$DISSECT "${R[@]}" examples/perf/mandelbrot.hy --fn mandelbrot
$BIN run /tmp/m.hyc
```

Dumps: `artifacts/superinstructions/dissect/*.fn.txt`.
N-grams: `artifacts/superinstructions/ngrams.txt` (trip weights are
order-of-magnitude, not `vm_profile`).

---

## Hot dumps (verbatim enough to refine)

**mandelbrot** (`DenseBin`×18, `DenseMove`×9, `DenseCast`×3, `BinSlotSlotJmpf`×4):

- y/x headers: `DenseCast ; DenseBin×3 ; DenseMove* ; BinSlotSlotJmpf`
- inner: `BinSlotSlotJmpf GT ; DenseBin×3 ; BinSlotSlotJmpf GTF ; JMP(break) ; DenseBin×6 ; DenseMove ; JMP(latch)`

**nsieve:** p-loop `DenseIndex ; BinSlotImmJmpf EQ`; k-loop
`DenseMove ; DenseStoreIndex ; DenseBin ; JMP`.

**fib:** `BinSlotImmJmpt ; BinSlotImm ; CALL ; BinSlotImm ; CALL ; BinReturn ; ConstReturnImm`.

**item_check:** `JumpIfMatch ; Unpack ; packed STORE ; LOAD ; CALL ; … ; CONST ; LOAD ; ADD ; LOAD ; BinReturn`.

**sum/scan tail:** `CONST ; STORE ; DenseBin ; JMP`.

---

## Threaded execute (sibling, not this project)

[Threaded execute dispatch](https://linear.app/ardax/project/threaded-execute-dispatch-e632b8b7ca96)
is cheaper dispatch, not new packed ops. `machine/src/fused.rs`: rustc
1.98 has no stable `become`; a 256-entry fn-pointer table was ~2% slower
on mandelbrot. 162 match arms including tombstones.

Hot vs cold if that project splits the match: hot = `DenseBin` / fused
jumps / `CALL` / `V*`; cold = `Dyn*`, FFI, coro, FORMAT, tombstones.
Coalescing (S2/X4) cuts trips through the same match without nightly
musttail.

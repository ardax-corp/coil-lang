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
| **S1** | [COI-377](https://linear.app/ardax/issue/COI-377) | Dense cmp/bin → jmp | `mandelbrot` inner: `DenseBin×3 ; BinSlotSlotJmpf GTF`. Headers: `DenseMove* ; BinSlotSlotJmpf GT` (no `DenseCmp`). `emit_br_cond` reconstructs stack cmp so fuse-select already made `*Jmpf` (1 dispatch). | dense emit | **Done (ISA pack).** Two-word `DenseBinJmpf` (archive **minor 17**): first `dense_abc` on the opcode, payload word is the fused `*Jmpf` (same stream-width lesson as `DenseBin2`). Real 2→1 on the inner mag + GTF. Outer y/x headers stay 1-op `BinSlotSlotJmpf`. **Not** `DenseCmp`+jmp. `fib` bytecode unchanged; keep `unlikely(is_hot)`. |
| **S2** | [COI-381](https://linear.app/ardax/issue/COI-381) | 2-wide `DenseBin` pack | `mandelbrot` has **18** `DenseBin`. Inner latch is `DenseBin×6 ; DenseMove ; JMP`. | dense emit | **Done (ISA pack).** Two-word `DenseBin2` (archive **minor 16**): first `dense_abc` on the opcode, payload word is the second `DenseBin` (chosen over a pool descriptor so the second packing stays in the instruction stream). Sequential IEEE; `mir/emit.rs` `pack_chained_dense_bin`. Inner latch `DenseBin×6` → three `DenseBin2`. **Not** X4 VM peek, **not** `FloatChainStore`. `fib` bytecode is unchanged; default table peek of `is_hot` must keep the giant match as fall-through (`unlikely(is_hot)`). |
| **S3** | [COI-380](https://linear.app/ardax/issue/COI-380) | `DenseCast`+`DenseBin` | Three `DenseCast` sites, all **x/y headers** (`DenseCast ; DenseBin×3`), not the innermost `iter` loop. | table/hotmatch VM peek | **Done (peek, no opcode).** After S2 the header is `DenseCast ; DenseBin2 ; DenseBin`. Table/hotmatch consume the trailing `DenseBin`/`DenseBin2` in the `DenseCast` handler (one dispatch for the pair). Bytecode unchanged; no archive bump; not ALWAYS_HOT growth. Giant match stays two-dispatch. Flagship wall is header-noise — prove on `examples/perf/dense_cast_bin.hy`. `fib` has no this shape; keep `unlikely(is_hot)`. |
| **S4** | [COI-379](https://linear.app/ardax/issue/COI-379) | `DenseIndex` + imm cmp + jmpf | `nsieve` p-loop: `DenseIndex ; BinSlotImmJmpf EQ` (`flags[p] == 1`). | dense emit | **Done (ISA pack).** Two-word `DenseIndexJmpf` (archive **minor 18**): first `dense_abc` on the opcode, payload word is the fused `*Jmpf` (same stream-width as `DenseBinJmpf`). Real 2→1 on the p-loop `flags[p] == 1`. Stacks with [COI-372](https://linear.app/ardax/issue/COI-372) last-addr `Object` cache. Index stays dense-native (not `IndexPin`). `fib` bytecode unchanged; keep `unlikely(is_hot)`. |
| **S5** | [COI-382](https://linear.app/ardax/issue/COI-382) | Index/StoreIndex stride pair | Inner k-loop is **`DenseMove ; DenseStoreIndex ; DenseBin ; JMP`** — store of 0 with `k = k + p`. **No** `DenseIndex` in that loop. | table/hotmatch VM peek | **Done (peek, no opcode).** Table/hotmatch consume trailing `DenseBin`/`DenseBin2` then `JMP` in the `DenseStoreIndex` handler (one dispatch for store + IV bump + latch). Bytecode unchanged (`DenseMove` stays its own op). No archive bump; not ALWAYS_HOT growth. Giant match stays three-dispatch on store/bin/jmp. `fib` has no this shape; keep `unlikely(is_hot)`. Not consecutive Index+Store of the same cell. |
| **S6** | [COI-383](https://linear.app/ardax/issue/COI-383) | `DenseMove` coalesce | `mandelbrot` **9** `DenseMove` (φ / latch copies). Pairs: `DenseMove ; DenseMove ; BinSlotSlotJmpf`. | MIR destprop / dense emit | **Done (peep, no opcode).** Sink φ incomings past last use of the dest, then the existing latch overwrite aliases `i = i+1` / mandelbrot `tr`→`zr`. Identity `DenseMove` drop. No new opcode; LOAD/STORE cost 2 vs `DenseMove` 1 unchanged. Nested-accumulator interference coalesce was unsound (SROA pack checksum) and is not shipped. |
| **S7** | [COI-384](https://linear.app/ardax/issue/COI-384) | Fuse-IL peep parity for S1–S5 shapes | `fib`/`tak`/`item_check`/`bottom_up` stay fuse-IL. Stack `*Jmpf` / `BinSlotImm` / `BinReturn` **already fire** (`fib` is 7 ops). Dense S1–S5 shapes do not appear on these bodies. | IL peep | **Done (peep, no opcode).** `item_check` `CONST; LOAD; ADD` → `BinSlotImm`: fuse-select matches const-left commute bins (shape appears after slot-promote, past canon); canon also swaps `Const; Load; op` without Known SP. `bottom_up` MakeEnum+RETURN is X3. Do not densify `main`/FORMAT. Dense S1–S5 “parity” remains N/A on fuse-IL flagships. |
| **S8** | [COI-385](https://linear.app/ardax/issue/COI-385) | Fusion not blocked on specialize keep | Cost gate / `lir_eligible` do **not** currently refuse because a packed op appeared (none exist yet). The live block is **`Dense*` as residual `Byte` → fuse-select Cold**. `emit_cost` weights `Seek` and LOAD/STORE, not opcode novelty. | typed `IlOp` for `Dense*` **or** keep packing in dense emit | One-line lift if S1–S5 should share fuse-select: lift `DenseBin`/`DenseCast`/`DenseIndex`/`DenseStoreIndex`/`DenseMove` in `from_plain_byte`. Do **not** make specialize keep a prerequisite. Print `main` stays fuse-IL by design. |

Kick order (project ladder, still valid): **S8 note first** (this doc) → **S2** (landed, two-word `DenseBin2`) → **S1** (landed, two-word `DenseBinJmpf`) → **S4** (landed, two-word `DenseIndexJmpf`) → **S6** → **S3** (landed, table/hotmatch peek) → **S5** (landed, table/hotmatch peek) → **S7** (landed, `item_check` const-left `BinSlotImm`).

---

## Beyond S1–S8 — ready-to-kick without specialize-keep

These are **not** on the S1–S8 ladder. They work on bodies that **already**
kept dense / vectorize / fuse-IL. No `try_specialize_body` change.

| Extra | Shape | Hit | Layer | Why it is independent |
|-------|-------|-----|-------|------------------------|
| **X1** | Vectorize remainder `i += 1`: `DenseConst` into scratch instead of `CONST ; STORE ; DenseBin` | `sum` / `scan` / `fill` scalar tails (`artifacts/.../for_in_sum.fn.txt`, `vec_scan.*.fn.txt`) | `mir/vectorize.rs` `emit_const_i64` → `emit.rs` `emit_const` (`DenseConst`) | **Done (emit, no opcode).** Remainder IV bump is `DenseConst ; DenseBin` (X2 peeks a trailing `JMP` on table/hotmatch). Existing `DenseConst` only; no specialize-keep; print `main` / FORMAT unchanged. |
| **X2** | `DenseBin ; JMP` latch coalescing | mandelbrot / nsieve / SIMD tails | table/hotmatch VM peek | **Done (no new opcode).** A second `Instruction` discriminant (~1.12–1.17× fib execute/LTO tax on identical `.hyc`) was reverted. Bytecode stays `DenseBin ; JMP` / `DenseBin2 ; JMP`. Default table and hotmatch peek the trailing `JMP` after `DenseBin`/`DenseBin2` (one dispatch on the latch). Giant match is byte-identical to main (two dispatches). Not S1, not S2, not X4 (X4 would peek adjacent `DenseBin`). `fib` has no this shape; keep `unlikely(is_hot)` / `.rodata` `ALWAYS_HOT`. Print `main` / FORMAT unchanged. |
| **X3** | `MakeEnum ; RETURN` fuse (one-word heap) | `bottom_up` (`MakeEnum ; RETURN` twice) | IL peep — `MakeEnum` is **typed** `IlOp`, `Return` is typed | **Done (fuse-select).** `MakeEnumReturn` (archive **minor 19**): same packing as `MakeEnum`. Two sites on `bottom_up` (Leaf arity-0, Node arity-2). Reuses `is_one_word_return` (no two-word RETURN). Not ALWAYS_HOT — alloc+return stays on the giant match like `MakeEnum`/`RETURN` (`unlikely(is_hot)`). Print `main` / FORMAT unchanged. |
| **X4** | VM coalescing of adjacent `DenseBin` **without** a new opcode | same as S2 | `machine/src/vm.rs` peek | **Not shipped** — S2 took the ISA pack (`DenseBin2`). Pick one; do not add peek on top. |

Not ready without a gate (still not specialize-keep):

| Extra | Gate |
|-------|------|
| `item_check` `CONST ; LOAD ; ADD` → `BinSlotImm` | **Done (COI-384 S7).** Fuse-select const-left commute peep; canon no longer needs Known SP for `Const; Load; op`. |
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
| `BinSlotImm` | `LOAD; CONST; int-bin` **or** `CONST; LOAD; commute-int-bin` | fuse-select (**int only**; const-left is COI-384) |
| `BinSlotSlot` | `LOAD; LOAD\|DUP; bin` | fuse-select (int+float) |
| `LoadReturnSlot` / `ConstReturnImm` / `BinReturn` / `MakeEnumReturn` | producer + one-word `RETURN` | fuse-select + convoy (`MakeEnumReturn` is fuse-select only) |
| packed `LOAD`/`STORE` n=2/3 | adjacent singles | fuse-select |

`invert_branch_over_jump`: `JMPF A; JMP B; A:` → `JMPT B`. **Loop headers
stay `*Jmpf`** (COI-87).

### Bounds / pin (not fuse-select)

`IndexUnchecked` / `StoreIndexUnchecked` / `ArrayPin` / `IndexPin*` from
`loop_bounds`. Flagship dense bodies use `DenseIndex` / `DenseStoreIndex`
instead.

### MIR specialize / SIMD

`DenseBin` / `DenseBin2` / `DenseBinJmpf` / `DenseIndexJmpf` / `DenseCmp` / `DenseConst` / `DenseMove` / `DenseUnary` /
`DenseCast` / dense heap / field / `V*` from `emit_dense` / `vectorize`.
`DenseBin2` is a two-word ISA pack of adjacent `DenseBin` (COI-381); X4 VM peek
of a second `DenseBin` is not shipped. COI-387 X2 peeks a trailing `JMP` after
`DenseBin`/`DenseBin2` on table/hotmatch only (no extra discriminant).
COI-380 S3 peeks a trailing `DenseBin`/`DenseBin2` after `DenseCast` on
table/hotmatch only (no extra discriminant). COI-382 S5 peeks trailing
`DenseBin`/`DenseBin2` then `JMP` after `DenseStoreIndex` on table/hotmatch
only (nsieve k-loop stride store + IV bump).

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
| `il/canon.rs` | `Load; Load; op` swap | Unknown SP; float; non-commutative ops. `Const; Load; op` is stack-relative (COI-384). |
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
| `examples/perf/dense_cast_bin.hy` | `2000001000000` | `hot` | dense i2f + DenseBin (S3 peek) |
| `examples/perf/stride_store_iv.hy` | `1` | `hot` | stride `DenseStoreIndex` + IV bump (S5 peek) |
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

**mandelbrot** (`DenseBin` / `DenseBin2` / `DenseBinJmpf`, fewer `DenseMove` after S6, `DenseCast`×3):

- y/x headers: `DenseCast ; DenseBin2 ; DenseBin ; DenseMove* ; BinSlotSlotJmpf` (table/hotmatch peeks the bin after `DenseCast`)
- inner: `BinSlotSlotJmpf GT ; DenseBin2 ; DenseBinJmpf (mag + GTF) ; JMP(break) ; DenseBin2×2 ; DenseBin2 ; JMP(latch)` (`tr` sunk so `zr` dest-overwrites; no latch `DenseMove`; table/hotmatch peeks the latch `JMP`)

**nsieve:** p-loop `DenseIndexJmpf` (`DenseIndex` + `BinSlotImmJmpf EQ`); k-loop
`DenseMove ; DenseStoreIndex ; DenseBin ; JMP` (table/hotmatch peeks store + bin + latch JMP).

**fib:** `BinSlotImmJmpt ; BinSlotImm ; CALL ; BinSlotImm ; CALL ; BinReturn ; ConstReturnImm`.

**fib:** `BinSlotImmJmpt ; BinSlotImm ; CALL ; BinSlotImm ; CALL ; BinReturn ; ConstReturnImm`.

**item_check:** `JumpIfMatch ; Unpack ; packed STORE ; LOAD ; CALL ; … ; BinSlotImm ADD ; LOAD ; BinReturn` (COI-384: `CONST; LOAD; ADD` fused).

**bottom_up:** `MakeEnumReturn` twice (Leaf arity-0, Node arity-2; COI-388 X3).

**sum/scan tail:** `DenseConst ; DenseBin ; JMP` (COI-386 X1 remainder; COI-387 table peek).

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

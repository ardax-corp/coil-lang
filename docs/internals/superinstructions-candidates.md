# Superinstruction / fusion candidates (investigation)

Docs-only. Tip `cf7a193c` (`main`). No compiler or VM behavior change in this
PR. Goal: enough tables that an architect can file a Linear epic + small
tickets without re-dissecting.

Doctrine this note obeys:

- One spelling per construct; do **not** revive mandelbrot-shaped
  `FloatChainStore` / `BinSlotSlotConstJmpf` ([optimization-roadmap.md](optimization-roadmap.md)
  “Historical (tombstoned float fuses)”).
- MIR is the default; do not grow fuse-only peeps whose only job is to
  outscore a dense reconstruct ([opt-generalization.md](opt-generalization.md)
  principle 1).
- New ISA variants are append-only + archive **minor**. Prefer no-ISA
  interpreter coalescing or emit hygiene when that already cuts dispatches.

Related: [opcodes.md](opcodes.md), [specialize-refuse.md](specialize-refuse.md),
[mir-dense-leftovers.md](mir-dense-leftovers.md), [pipeline.md](pipeline.md),
`compiler/src/il/lower.rs` (`fuse_select`), `machine/src/fused.rs`.

## Method

Release `coil` / `coil-dissect`, `COIL_AUTO_PAR=0`, stdlib
`--root .deps/coil-stdlib/src`. There is **no** `coil dissect file.hyc`;
dissect compiles `.hy` in memory on the same `Pipeline` as `coil compile`.
Archives were still produced and executed (checksums below). `--opt-stats`
is IL-pass counters only (no fuse / MIR keep-rate).

Trip weights for n-grams are **order-of-magnitude loop bounds**, not
`vm_profile` dispatch histograms. Inner-loop dumps are the primary
evidence; n-grams are in `artifacts/superinstructions/ngrams.txt`.

| Bench | `.hy` | Checksum (`coil run` `.hyc`) | Hot body | Path after specialize |
|-------|-------|------------------------------|----------|------------------------|
| mandelbrot | `examples/perf/mandelbrot.hy` | `625885` | `mandelbrot` | dense + fused `*Jmpf` |
| fib | `examples/perf/fib.hy` | `2178309` | `fib` | fuse-IL + B2 CALL convoy |
| tak | `examples/perf/tak.hy` | `7` | `tak` | fuse-IL + packed `LOAD` + `TailCall` |
| nsieve | `examples/perf/nsieve.hy` | `1900` | `nsieve` | dense-native Index/Store + `*Jmpf` |
| binary_trees | `examples/perf/binary_trees.hy` | `135854` | `item_check`, `bottom_up` | fuse-IL (match / `MakeEnum`) |
| for_in_sum | `examples/perf/for_in_sum.hy` | `12884115456` | `sum` | `VLoad`/`VReduce` + scalar tail |
| vec_scan | `examples/perf/vec_scan.hy` | `536739840` | `scan`, `fill` | `V*` + scalar tail |

Reproduce:

```bash
export COIL_AUTO_PAR=0
cargo build --release --bin coil --bin coil-dissect
BIN=./target/release/coil
DISSECT=./target/release/coil-dissect
R=(--root .deps/coil-stdlib/src)
$BIN compile "${R[@]}" examples/perf/mandelbrot.hy -o /tmp/mandelbrot.hyc --opt-stats
$DISSECT "${R[@]}" examples/perf/mandelbrot.hy --fn mandelbrot
$BIN run /tmp/mandelbrot.hyc
```

Dumps: `artifacts/superinstructions/dissect/*.fn.txt`.

---

## 1. Existing fused / packed / dense ops

### 1.1 Fuse-select (`compiler/src/il/lower.rs`)

One named pass after concat (`fuse_select` → PC assign). Residual
`IlOp::Byte` is `Slot::Cold` — any window that includes it is refused.
Labels / `JoinLabel` / `FuseHint` are hard barriers. Per-function fuse is
not production.

| Opcode | Window | Emit |
|--------|--------|------|
| `BinSlotImmJmpf` / `BinSlotImmJmpt` | `LOAD; CONST; cmp; JMPF/T` or `BinSlotImm; JMPF/T` | fuse-select |
| `BinSlotSlotJmpf` / `BinSlotSlotJmpt` | `LOAD; LOAD; cmp; JMPF/T` or `BinSlotSlot; JMPF/T` | fuse-select |
| `CmpJmpf` / `CmpJmpt` | stack `cmp; JMPF/T` | fuse-select |
| `LogNotJmpf` / `LogNotJmpt` | `LogNot; JMPF/T` | fuse-select |
| `BinSlotImmStore` | `LOAD; CONST; int-bin; STORE` or `BinSlotImm; STORE` | fuse-select (`is_int_bin_op` only) |
| `BinSlotSlotStore` | `LOAD; LOAD; bin; STORE` or `BinSlotSlot; STORE` | fuse-select (`is_bin_op`, int+float) |
| `BinSlotImm` | `LOAD; CONST; int-bin` | fuse-select (**int only**; float imm uses pool → no fuse) |
| `BinSlotSlot` | `LOAD; LOAD\|DUP; bin` | fuse-select (int+float) |
| `LoadReturnSlot` | `LOAD; RETURN` (one-word) | fuse-select + convoy (`il/opt/convoy.rs`) |
| `ConstReturnImm` | inline `CONST; RETURN` (one-word) | fuse-select + convoy |
| `BinReturn` | `bin\|BinSlot*; RETURN` (one-word) | fuse-select + convoy |
| packed `LOAD` n=2/3 | adjacent `LOAD` | fuse-select (`try_fuse_packed_loads`) |
| packed `STORE` n=2/3 | adjacent `STORE` | fuse-select |

`invert_branch_over_jump` (`il/opt/cfg.rs`) turns `JMPF A; JMP B; A:` into
`JMPT B`; fuse-select emits the `*Jmpt` twin. **Loop headers stay `*Jmpf`**
(COI-87, [limitations.md](limitations.md)).

### 1.2 Bounds / pin (not fuse-select)

| Opcode | Where |
|--------|--------|
| `IndexUnchecked` / `StoreIndexUnchecked` | `il/opt` `loop_bounds` |
| `ArrayPin` / `IndexPin*` / `StoreIndexPin*` | `loop_bounds` after Unchecked ([array-pin.md](array-pin.md)) |

On the measured flagships, **dense-native heap ops replaced pins** in
`nsieve` / `sum` / `scan` / `fill` (`DenseIndex` / `DenseStoreIndex` /
`DenseArrayLen`). Pin ops remain the fuse-IL path.

### 1.3 MIR specialize / LIR / SIMD

| Opcode | Where | Notes |
|--------|--------|--------|
| `DenseBin` / `DenseCmp` / `DenseConst` / `DenseMove` / `DenseUnary` / `DenseCast` | `mir/emit.rs` `emit_dense` | 3-address; stack-neutral |
| `DenseIndex` / `DenseStoreIndex` / `DenseArrayLen` / `DenseMake` / `DensePush` | dense A2 | |
| `DenseArrayPush` | B6 | |
| `DenseFieldLoad` / `DenseFieldStore` / `DenseMakeObject` | D2 | |
| `VLoad` / `VStore` / `VBin` / `VMove` / `VReduce` / `VFma` | `mir/vectorize.rs` | counted stride-1 |
| HostInvoke `simd_axpy_reduce` **136** | `mir/pack.rs` | saxpy pack, not a fuse opcode |

**Important:** `DenseCmp` is emitted for SSA `Cmp` *values*
(`emit.rs` `MirInst::Cmp`), but **branchy compares go through
`emit_br_cond`**, which reconstructs stack `LOAD`/`BinSlotImm`/`Bin` +
`Jump` so fuse-select can make `BinSlotSlotJmpf` / `BinSlotImmJmpf`.
Measured dense loops (`mandelbrot`, `nsieve`, `sum`) contain **zero
`DenseCmp`**. A new `DenseBr` would not beat an already-fused
`BinSlotSlotJmpf` (both are one dispatch, no stack traffic).

### 1.4 Tombstones / not selected

| Opcode | Status |
|--------|--------|
| `FloatChainStore` | not emitted; VM panics (`vm.rs`) |
| `BinSlotSlotConstJmpf` | not emitted; VM panics |
| `BinSlotSlotConstJmpt` | **handler still live**; fuse-select comment: not selected |
| `HostInvokeNiche` / `OptionNicheToHeap` / `HeapOptionToNiche` / `PairToHeap` / `HeapToPair` / `ReturnPair` | archive major 4 tombstones |

Do not treat tombstones as recently landed AOT.

### 1.5 Other packing (not “superinstructions”)

`CALL`/`TailCall` bit 31 two-slot return; `INC`/`DEC` packed slot+prefix+float;
`HostInvoke` arg count + enum layout bits. Codegen `TailCall` (#316) is ABI
reuse, not fuse-select.

---

## 2. Hottest adjacent sequences that are **not** already one op

Evidence: `artifacts/superinstructions/dissect/*.fn.txt`.

### mandelbrot (dense; inner `while iter < max_iter`)

```
BinSlotSlotJmpf GT     ; DenseBin ; DenseBin ; DenseBin ; BinSlotSlotJmpf GTF
JMP (break)
DenseBin ×6 ; DenseMove ; JMP (latch)
```

| Sequence | Why unfused | Est. trips | Dispatch cut if packed |
|----------|-------------|------------|------------------------|
| `DenseBin ; DenseBin` (× many) | no 2-wide dense pack; `FloatChainStore` tombstoned | `160²×50` inner | 2→1 per pair |
| `DenseBin ; JMP` latch | fuse-select does not pack dense+uncond JMP | inner | 2→1 |
| `DenseBin ; BinSlotSlotJmpf GTF` | `emit_br_cond` uses stack fuse, not `DenseCmp` | inner | already 1 jump; mag is extra `DenseBin`s |
| `DenseMove ; BinSlotSlotJmpf` loop headers | phi moves then fused header | `160²` / `160` | low vs inner |

**Do not** revive `FloatChainStore` for `zr*zr + zi*zi > 4.0`. That is the
tombstoned mandelbrot fuse. A **generic** 2-wide `DenseBin` (or VM peek of
the next `DenseBin`) is the regular form.

### fib (fuse-IL, already tiny)

```
BinSlotImmJmpt LEQ ; BinSlotImm SUB ; CALL ; BinSlotImm SUB ; CALL ; BinReturn ADD ; ConstReturnImm
```

Every producer except `CALL` is already fused. Remaining pairs are
`BinSlotImm ; CALL` and `CALL ; BinSlotImm`. Frame/`CALL` dominates; a
call-prep superinstruction is low leverage and fib-shaped.

### tak (fuse-IL)

```
BinSlotSlotJmpt LEQ ; (BinSlotImm SUB ; packed LOAD n=2 ; CALL)×3 ; TailCall
```

Hottest unfused chain: **`BinSlotImm ; LOAD ; CALL`** (arg prep). Packed
`LOAD` already ate the second load. Same CALL-dominates caveat as fib.

### nsieve (dense)

Inner `k` loop:

```
BinSlotSlotJmpf GT ; DenseMove ; DenseStoreIndex ; DenseBin ; JMP
```

`p` loop: `DenseIndex ; BinSlotImmJmpf EQ` (`flags[p] == 1`).

| Sequence | Note |
|----------|------|
| `DenseStoreIndex ; DenseBin` | store then `k = k + p` |
| `DenseIndex ; BinSlotImmJmpf` | already 2; 3-in-1 is a new opcode |
| fill: `DenseArrayPush ; DenseBin ; JMP` | grow loop |

### binary_trees

`item_check` (fuse-IL, D3 reconstruct lost the cost gate):

```
Seek ; LOAD ; JumpIfMatch ; Unpack ; packed STORE ; LOAD ; CALL ; STORE ;
LOAD ; CALL ; STORE ; CONST ; LOAD ; ADD ; LOAD ; BinReturn ; ConstReturnImm
```

Unfused: `CONST ; LOAD ; ADD` (const-under; canon wants `LOAD ; CONST` but
see refuse table), `JumpIfMatch ; Unpack`, `LOAD ; CALL`.

`bottom_up`: `BinSlotImm ; CALL` (same as fib) and **`MakeEnum ; RETURN`**.

### for_in_sum `sum` / vec_scan `scan` / `fill`

Vector body already tight (`VLoad ; VReduce ; DenseBin ; JMP` or
`VBin×3 ; VStore ; DenseBin ; JMP`). Scalar remainder:

```
CONST imm=1 ; STORE scratch ; DenseBin IADD64 i,i,scratch ; JMP
```

This is **emit hygiene**, not a missing ISA: `mir/vectorize.rs`
`emit_const_i64` pushes stack `IlOp::Const` + `StorePop` instead of
`DenseConst` into the scratch (contrast `emit.rs` `emit_const`).

---

## 3. Refuse / block sites (quote + reason)

| Site | What it blocks | Why (one line) |
|------|----------------|----------------|
| `il/lower.rs` `fuse_select` | any window with `IlOp::Byte` | residual `Byte` is `Slot::Cold`; FORMAT/FFI/`Seek`/packed leftovers |
| `il/lower.rs` `fuse_slots_with_origins` | fuse across labels / abs JMP / uncond join into `*Return` | join would steal stacked arm values; `ConstReturnImm` would drop them |
| `il/op.rs` `FuseHint::nofuse_value_under_jmp` | `cmp; JMPF` fuse | pair-`?` / pair-match keep payload under the cond (`emit_match.rs`, `compiler.rs`) |
| `il/lower.rs` `is_one_word_return` | `*Return` on `RETURN` operand 2 | would drop the second word of a pair |
| `il/lower.rs` `is_int_bin_op` | `BinSlotImm` / `BinSlotImmStore` for float | float imm is pool `CONST`; `const_inline_value` requires inline i32 |
| `il/lower.rs` `try_fuse_slots` comment | `FloatChainStore` / `BinSlotSlotConstJmpf` | “Mandelbrot-shaped … are not emitted”; tests assert absence |
| `il/canon.rs` | `Const; Load; ADD` → `Load; Const` | **Unknown SP**; **float ops**; non-commutative `SUB`/`DIV`/… |
| `il/opt/cfg.rs` / COI-87 | invert loop headers to `*Jmpt` | loop headers stay `*Jmpf` |
| `il/opt/opt_level.rs` `seek_back_edge` | Seek latch on Standard | **off** on Standard; `Aggressive`/`-O3` only (Seek poisons latch height) |
| `mir/entry.rs` `hard_refuse` | MIR→LIR | `Call`/`PrologueJmp` (one-word CALL), `HostInvoke`, heap fields, `BoxValue`, unmapped alloc, unmapped `MakeCoro` |
| `mir/specialize.rs` `try_specialize_body` | keep dense | no float/i64/i32 **arith** (compare-only); unmapped alloc; post-loop-only `return [x]` (COI-87); dropped `StoreIndex`/`Index`; `residual_heap_box`; **cost** `emit_cost(out) > emit_cost(ops)` on straight-line; grown `MakeEnum` |
| `mir/infer.rs` | dense infer | residual `Byte`; `Pow`/`PowF`/`AND`/`OR`; Q9 table `STRING`/`PRINT`/`FORMAT`/`STRINGIFY`; unmapped alloc |
| `mir/string_barrier.rs` | dense string | “Dense specialize still refuses table STRING/PRINT/FORMAT/STRINGIFY (R1)” |
| `mir/emit.rs` | dense emit | Alloc/GcBarrier without S2b maps; untyped HostInvoke; ArrayPush without maps; unboxed Field* (I3 is LIR); I4 print/format |
| `mir/emit.rs` `emit_br_cond` | `DenseCmp` on branches | reconstructs stack cmp so fuse-select can emit `*Jmpf` |
| `mir/vectorize.rs` `try_vectorize` | `V*` | GC/deopt edge; Call/Host/match/field/alloc/string; **not** a single counted loop with `body == latch` and Return-only exit |
| `mir/vectorize.rs` `emit_const_i64` | dense `i += 1` tail | stack `Const`+`StorePop` instead of `DenseConst` |
| `mir/abi.rs` `MAX_MODELED_RET_WORDS = 2` | N>2 CALL/RETURN | infer/lower refuse (C1 leftover / C1b) |
| `opt-generalization.md` | fuse-only peeps | do not add peeps whose only job is beating dense reconstruct |

`lir_eligible` is **not** a fusion gate; it is the leftover MIR→LIR
reconstruct wall after dense misses (`compiler/src/mir/entry.rs`).

---

## 4. Ranked proposals

### Ready-to-kick (sound, no gate, or emit-only)

| # | Ticket | Kind | Hits | Gate | Notes |
|---|--------|------|------|------|-------|
| T1 | **DenseBin coalescing in the VM** (peek next `DenseBin`, cap 2–4) | no ISA | mandelbrot inner, nsieve `DenseBin` | none for semantics; **debugger single-step / `debug_locs`** must still stop per source op or document skip | Preferred over a new 2-wide opcode. Stack-neutral, no GC. Prove on mandelbrot `.hyc` dispatch/`poop`. |
| T2 | **`DenseBin ; JMP` latch coalescing** | no ISA | mandelbrot / nsieve / SIMD tails | same debug caveat | Uncond JMP after dense arith is the counted-loop latch. |
| T3 | **Vectorize remainder: `DenseConst` for `+1`** | emit only | `sum`/`scan`/`fill` scalar tails | replace `emit_const_i64`+`StorePop` with `emit_const(I64(1), scratch)` | 3 dispatches → 2 (`DenseConst ; DenseBin`) with **existing** ops. Tiny, independent of T1. |
| T4 | **`MakeEnum ; RETURN` fuse** (`MakeEnumReturn` or encode in fuse-select) | maybe ISA | `bottom_up` | one-word heap return only (reuse `is_one_word_return`) | Do not eat two-word `RETURN`. |

T1/T2 are interpreter work, not “superinstructions” in the ISA sense.
They cut dispatches on **already-dense** bodies without archive bump.

### Needs a small gate lift

| # | Ticket | Kind | Hits | One-line lift |
|---|--------|------|------|----------------|
| T5 | Canon/fuse `CONST ; LOAD ; ADD` in `item_check` | no ISA | binary_trees | Make SP Known after `JumpIfMatch`/`Unpack`/`STORE` so `canon` can const-to-RHS, then existing `BinSlotImm` | Unknown-SP refuse in `il/canon.rs` |
| T6 | `emit_br_cond` keep `DenseCmp` when fuse cannot win | emit | none on these benches | **Skip unless** a body shows stack cmp that **fails** to fuse (residual `Byte` / `FuseHint`). Today `BinSlotSlotJmpf` is already 1 dispatch. |
| T7 | `BinSlotImm` for **pool** float imm | fuse-select | none on these benches (mandelbrot is dense) | `const_inline_value` / `is_int_bin_op` — only if a fuse-IL float kernel remains |

### Blocked for real reasons (do not file as “just fuse it”)

| Idea | Why blocked |
|------|-------------|
| Revive `FloatChainStore` / `BinSlotSlotConstJmpf` | Tombstone; mandelbrot-shaped; P11/IEEE (no FMA/reassoc). Tests assert not emitted. |
| `CALL`+`BinSlotImm` / `BinSlotImm; LOAD; CALL` superinstruction | Universal-looking but **CALL/frame dominates**; fib/tak-shaped; ABI/arity packing. |
| Fuse across `FuseHint::nofuse_value_under_jmp` | Live payload under cond; invert+convoy would steal CALL args. |
| Fuse `*Return` at uncond value-join | Documented in `fuse_slots_with_origins`; drops stacked arm value. |
| Two-word `RETURN` → `BinReturn` | Silently drops tag/hi word. |
| Residual `Byte` windows (FORMAT, FFI, `Seek`) | Cold by contract (D4). |
| Dense infer `Pow` / `AND`/`OR` | `mir/infer.rs` `apply_bin` hard refuse. |
| LIR reconstruct of HostInvoke / one-word CALL | `LirRefuse::Host` / `Call`; dense may still emit. |
| Compare-only dense specialize | `try_specialize_body` requires arith; I8 LIR or fuse-IL. |
| Scalar IEEE FMA opcode | P11: no FMA contract; `VFma` is mul-then-add two roundings for SIMD only. |
| Token-threaded `become` execute | rustc 1.98: no stable guaranteed tail calls (`machine/src/fused.rs`). |

### Suggested Linear epic + children

Epic: **Interpreter dispatch / residual fusion** (not “more MIR islands”).

1. **T3** vectorize `DenseConst` +1 tail (smallest, no VM match change).
2. **T1** DenseBin coalescing + mandelbrot prove (debug-step policy in the same ticket or a sibling).
3. **T2** DenseBin+JMP latch coalescing (can land with T1).
4. **T5** canon Known-SP after match (binary_trees `item_check` hit bench).
5. **T4** `MakeEnumReturn` fuse-select (binary_trees `bottom_up`; archive minor only if a new discriminant is used).
6. Parking lot: T6/T7 only if a new fuse-IL float/cmp body shows up.

Keep **flagships as controls**. If T1/T2 do not move mandelbrot `poop`, do not
add a 2-wide dense opcode “for the ISA.”

---

## 5. Future musttail / fn-pointer threaded execute

From `machine/src/fused.rs`:

> rustc 1.98 has no stable guaranteed tail calls (`become` is nightly), so
> token-threading the main loop is out. A 256-entry `fn` pointer table was
> measured ~2% **slower** on mandelbrot (indirect call on the float fused
> path). Helpers are `#[inline(always)]` matches so LLVM can emit a jump
> table of straight-line arms.

The production loop is one giant `match` on `Instruction` (`vm.rs`),
`promise!(*bc as u8 <= Instruction::DenseMakeObject as u8)` in release.
Mnemonic table has **161** arms (including tombstones).

Hot vs cold split (for a future threaded interpreter or a nested match):

| Hot (these benches) | Cold (keep off the predicted path) |
|---------------------|------------------------------------|
| `DenseBin` / `DenseMove` / `DenseConst` / `DenseCast` / `DenseIndex` / `DenseStoreIndex` / `DenseArrayLen` / `DenseArrayPush` / `DensePush` | `Dyn*`, `CallIndirect`, `MakePolyFn*`, `MakeFn` |
| `BinSlotSlotJmpf` / `BinSlotImmJmpf` / `BinSlotImm` / `BinReturn` / `*Return` | `Ffi*`, `DeclareFFI`, `HostInvoke` (except math kernels) |
| `CALL` / `TailCall` / `JMP` / packed `LOAD`/`STORE` | coro (`MakeCoro`/`Resume`/`Yield*`/`DoneCoro`) |
| `VLoad`/`VReduce`/`VBin`/`VStore` | `FORMAT`/`STRINGIFY`/`PRINT`/`STRING` |
| `JumpIfMatch` / `Unpack` / `MakeEnum` (`binary_trees`) | tombstones (`FloatChainStore`, niches, `PairToHeap`, …) |
| `Seek` (dense prologue, once per call) | `Panic`, `DictEntries`, statics, `BoxValue` |

Practical tickets **without** `become`:

- **H1** Nested match: outer `bc as u8 <= last_hot` vs cold. No semantics.
  Measure mandelbrot; fn-pointer already lost ~2%.
- **H2** Stop executing tombstone arms on the hot jump table (still panic
  if hit). Shrinks LLVM jump table.
- **H3** When `become` stabilizes: token-thread **only** the hot set
  (`DenseBin`, fused jumps, `CALL`); cold stays a match. Do not thread
  161 arms.

Coalescing (T1/T2) is the same theme as threading: fewer trips through
the match. Do that first; it does not wait on nightly `become`.

---

## 6. What `--opt-stats` does *not* tell you

`--opt-stats` on these compiles shows IL `slot_promote` / `canon` /
`licm` / `copy_prop` hits. It does **not** count fuse-select windows
refused, nor dense keep vs fuse-IL. For keep/refuse see
[specialize-refuse.md](specialize-refuse.md) and dissect of the **final**
bytecode (presence of `Dense*` / `V*` vs `CALL`/`JumpIfMatch`).

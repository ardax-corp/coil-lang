# Threaded execute dispatch (COI-373 G0, COI-374 G1, COI-375 G2)

Can a portable **fn-pointer** interpreter beat the outlined giant `match` in
`Machine::execute` on flagship `.hyc`?

## Mechanism

**Fn-pointer trampoline on stable**, not `become`.

Rust 1.98 has no stable guaranteed tail calls (`become` /
`feature(explicit_tail_calls)` is nightly; the 2026 project goal aims to line
it up for later stabilization). Token-threading that *must* tail-call the next
handler would overflow the native stack on mandelbrot (~hundreds of millions
of dispatches) unless LLVM actually emits `jmp`. That is not a promise on
stable, so production dispatch does not use `become`.

Portable substitute: an **outlined** hot-op loop (`machine/src/dispatch.rs`)
with a 256-entry handler table. Cold ops stay on the existing match.
`#[inline(never)]` handlers keep the trampoline from swallowing the giant
match via fat LTO (same reason `execute` itself is outlined).

A compact **`hotmatch`** control lives in the same outlined function: one small
`match` over the hot subset, still a single polymorphic indirect jump, but a
much smaller I-cache footprint than `execute`.

`fused.rs` already recorded that a 256-entry table for *inner* fused binops was
~2% slower on mandelbrot. This document is about dispatch of `execute` itself.

## Hot subset (G1 default)

G0: `DenseBin`, `DenseCmp`, `DenseConst`, `DenseMove`, `DenseCast`, `JMP` /
`JMPF` / `JMPT`, `BinSlotSlotJmpf` / `BinSlotSlotJmpt`. `DenseBin2` /
`DenseBinJmpf` / `DenseIndexJmpf` are always-hot like `DenseBin` (`.rodata` mask; `unlikely(is_hot)`).
Table/hotmatch peek a trailing `JMP` after `DenseBin` / `DenseBin2` (COI-387 X2) so the latch is one dispatch without a new `Instruction` variant. The giant match does not peek. Table/hotmatch peek a leftover `DenseBin` after `DenseBin2` (COI-389 X4 residue; S2 already packed adjacent pairs); giant match does not. Table/hotmatch also peek a trailing `DenseBin` / `DenseBin2` after `DenseCast` (COI-380 S3); bytecode stays split; giant match does not peek. Table/hotmatch peek `DenseBin`/`DenseBin2` then `JMP` after `DenseStoreIndex` (COI-382 S5); giant match does not.

G1 adds: `DenseUnary`, `DenseIndex` / `DenseStoreIndex` / `DenseArrayLen` /
`DenseFieldLoad` / `DenseFieldStore`, `CmpJmpf` / `CmpJmpt`, `LogNotJmpf` /
`LogNotJmpt`, `BinSlotSlotStore`.

Packed `LOAD` / `STORE` / `Seek` stay on the match by default: threading them
without also threading every interleaved heap op **bounces** nsieve. They are
implemented as shared helpers and used by the giant match.

`DenseMake` / `DensePush` / `DenseArrayPush` / `DenseMakeObject` stay on the
match (alloc / GC). Tombstones stay on the match.

Debugger-attached runs never enter the hot loop (per-op stops).

## Remaining ops (G2)

G2 fills the 256-entry table for every non-kernel opcode. Stack arith, consts,
casts, and `BinSlotSlot` have outlined handlers on the trampoline. Heap / FFI /
host / coro / SIMD / alloc ops share a `rest` slot that bounces to
`Machine::exec_rest` (the old giant-match arms). Archive tombstones
(`HostInvokeNiche`, `OptionNicheToHeap`, …) use that rest path too.

**Not** on the default table (still the inlined `execute` match):

- `CALL` / `TailCall` / `RETURN` / fused returns / `MakeEnumReturn`
- packed `LOAD` / `STORE` / `StorePop` / `Seek`
- imm-slot fuses (`BinSlotImm*`)

`hotmatch` still only consumes the G1 hot subset. Table mode still diverts on
`unlikely(is_hot)` only (G1). Remaining ops in `execute` go through
`_ => exec_rest` so fib does not bounce on stack `ADD` between CALLs. Once a
hot streak is active, non-kernel table slots (arith / rest) run until a kernel
op.

## CALL / RETURN (A/B, default off)

Handlers exist for `CALL` / `TailCall` / `RETURN` / `ConstReturnImm` /
`LoadReturnSlot` / `BinReturn` and the imm-slot fuses (`BinSlotImm`,
`BinSlotImmJmpf` / `Jmpt`, `BinSlotImmStore`). They are **not** in the default
hot table.

Threading only the imm-slot fuses (CALL still on match) **bounces** fib out of
the trampoline on every call and is much slower. Threading CALL without RETURN
is better than that bounce, but still loses to the giant match. Threading
CALL+RETURN+imm together still loses on fib (~1.2× match) and washes
mandelbrot (extra outlined handlers).

Opt in together (process start):

```bash
COIL_THREADED_CALL=1 COIL_THREADED_RETURN=1
```

## A/B

Same release binary. Process-start switch:

| `COIL_THREADED_DISPATCH` | Path |
|---|---|
| `0` / `match` | giant `execute` match (baseline) |
| unset / `1` / `table` | fn-pointer trampoline (default) |
| `2` / `hotmatch` | compact hot match |

Always `COIL_AUTO_PAR=0` for sequential flagship `.hyc`. Compile with
`--root .deps/coil-stdlib/src` when the userland stdlib is not on the default
search path.

```bash
COIL_AUTO_PAR=0 ./target/release/coil compile --root .deps/coil-stdlib/src \
  examples/perf/mandelbrot.hy -o /tmp/m.hyc
COIL_THREADED_DISPATCH=0 COIL_AUTO_PAR=0 ./target/release/coil run /tmp/m.hyc   # expect 625885
COIL_THREADED_DISPATCH=1 COIL_AUTO_PAR=0 ./target/release/coil run /tmp/m.hyc
```

Checksums: mandelbrot `625885`, fib `2178309`, nsieve `1900`, tak `7`.

`poop` panics on this host (`perf_event` / stripped binary); wall A/B used `hyperfine`.

## Measured (release, fat LTO, `COIL_AUTO_PAR=0`, same `.hyc`)

### G0 (COI-373)

| Bench | match | table | hotmatch |
|---|---|---|---|
| mandelbrot | 27.2 ± 0.4 ms | **24.1 ± 1.3 ms** (~1.13× vs match) | 26.1 ± 0.3 ms (~1.04×) |
| fib | 67.3 ± 1.7 ms | 66.3 ± 0.8 ms (wash) | 66.1 ± 0.6 ms (wash) |

### G1 default hot set (CALL/RETURN/LOAD/STORE/Seek off)

Isolated `hyperfine` (~50 min-runs mandelbrot, x86_64, `COIL_AUTO_PAR=0`):

| Bench | match | table | notes |
|---|---|---|---|
| mandelbrot | 23.6 ± 0.4 ms | 23.1 ± 1.9 ms (~1.02×) | holds a wash-to-slight win; G0 was ~1.13× on a noisier run |
| fib | 59.8 ± 2.4 ms | 62.0 ± 3.7 ms | wash / slight match (CALL on match) |
| nsieve | 2.4 ± 0.2 ms | 2.8 ± 0.2 ms | short run; Dense* on table, startup-dominated |
| tak | ~2.6 ms | ~2.6–2.8 ms | CALL-heavy; stays on match |

Checksums identical across match / table / hotmatch.

Appending a hot opcode (`DenseBin2`, COI-381) made LLVM treat `execute`'s
`is_hot` divert as the fall-through and cost ~14–19% on `fib` (identical
bytecode, never hot). `is_hot` is a `.rodata` bitset (not the 2 KiB handler
table); the divert is `unlikely` so the giant match stays the fall-through.

Threading packed `LOAD`/`STORE`/`Seek` **without** `DenseIndex` made nsieve
~1.3× slower (bounce). Threading CALL/RETURN/imm is documented above.

### G1 CALL/RETURN experiment (not default)

| Config | fib | tak | mandelbrot |
|---|---|---|---|
| table, CALL on, RETURN off | 76.6 ± 7.9 ms (loses to match 64.2) | ~1.12× slower vs match | ~1.10× table win |
| table, CALL+RETURN+imm on | 79.5 ± 4.8 ms (~1.23× match) | wash/slight loss | wash vs match |
| table, CALL off, imm on | 102 ms | — | bounce tax |

`COIL_THREADED_CALL=1` vs `=0` on the table path: CALL-on is ~1.39× faster than
CALL-off for fib, but still slower than the giant match.

### G2 remaining coverage (COI-375)

Same host, `COIL_AUTO_PAR=0`, identical `.hyc`:

| Bench | match | table | notes |
|---|---|---|---|
| mandelbrot | 19.1 ± 0.3 ms | 20.4 ± 0.9 ms (~1.06× match) | wash / slight table loss vs G1’s wash-to-win; extra outlined rest/arith handlers |
| fib | 58.5 ± 10.4 ms | 59.3 ± 0.3 ms | **flat** (CALL/RETURN/imm on match; no ADD bounce) |
| nsieve | 2.6 ± 0.2 ms | 2.8 ± 0.2 ms | short; startup-dominated |

Checksums identical: mandelbrot `625885`, fib `2178309`, nsieve `1900`.

Divert stays `unlikely(is_hot)` (G1). Diverting all `!is_kernel` ops made fib ~1.12× slower (stack `ADD` bouncing out of the CALL kernel).

G2 landed remaining coverage without parking CALL/RETURN or packed LOAD/STORE
on the default table. Next is hot/cold I-cache split of handler code, not a
retry of “CALL on the table.”

## I-cache / inlining

- Hot loop is a separate `#[inline(never)]` function from `execute`.
- Table handlers are `#[inline(never)]` so LTO does not paste them into one
  mega-function (and so objdump can show `callq *handler`).
- Shared `#[inline(always)]` helpers keep match-path and table semantics
  identical (no opcode duplication).
- Growing the outlined handler set (CALL+RETURN) washed the mandelbrot win;
  keep the default hot set to ops that actually run in dense kernels.

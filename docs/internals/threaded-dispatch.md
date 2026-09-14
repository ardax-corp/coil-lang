# Threaded execute dispatch (COI-373 G0)

Spike: can a **musttail** or portable **fn-pointer** interpreter beat the
outlined giant `match` in `Machine::execute` on flagship `.hyc`?

## Mechanism

**Fn-pointer trampoline on stable**, not `become`.

Rust 1.98 has no stable guaranteed tail calls (`become` /
`feature(explicit_tail_calls)` is nightly; the 2026 project goal aims to line
it up for later stabilization). Token-threading that *must* tail-call the next
handler would overflow the native stack on mandelbrot (~hundreds of millions
of dispatches) unless LLVM actually emits `jmp`. That is not a promise on
stable, so G0 does not use `become`.

Portable substitute: an **outlined** hot-op loop (`machine/src/dispatch.rs`)
with a 256-entry handler table. Cold ops (including `CALL`) stay on the existing
match. `#[inline(never)]` handlers keep the trampoline from swallowing the
giant match via fat LTO (same reason `execute` itself is outlined).

A compact **`hotmatch`** control lives in the same outlined function: one small
`match` over the hot subset, still a single polymorphic indirect jump, but a
much smaller I-cache footprint than `execute`.

`fused.rs` already recorded that a 256-entry table for *inner* fused binops was
~2% slower on mandelbrot. G0 is a different question: dispatch of `execute`
itself.

## Hot subset

`DenseBin`, `DenseCmp`, `DenseConst`, `DenseMove`, `DenseCast`, `JMP` /
`JMPF` / `JMPT`, `BinSlotSlotJmpf` / `BinSlotSlotJmpt`.

`DenseCmp` is included so a dense numeric kernel does not bounce out of the
hot loop on every compare. `CALL` / `RETURN` / heap ops stay on the match.

Debugger-attached runs never enter the hot loop (per-op stops).

## A/B

Same release binary. Process-start switch:

| `COIL_THREADED_DISPATCH` | Path |
|---|---|
| `0` / `match` | giant `execute` match (baseline) |
| unset / `1` / `table` | fn-pointer trampoline (spike default) |
| `2` / `hotmatch` | compact hot match |

Always `COIL_AUTO_PAR=0` for sequential flagship `.hyc`.

```bash
COIL_AUTO_PAR=0 ./target/release/coil compile examples/perf/mandelbrot.hy -o /tmp/m.hyc
COIL_THREADED_DISPATCH=0 COIL_AUTO_PAR=0 ./target/release/coil run /tmp/m.hyc   # expect 625885
COIL_THREADED_DISPATCH=1 COIL_AUTO_PAR=0 ./target/release/coil run /tmp/m.hyc
poop -d 6000 './target/release/coil run /tmp/m.hyc'   # with the env set per process
```

Checksums: mandelbrot `625885`, fib `2178309`.

## Go / no-go for G1 (COI-374)

Fill after the release A/B in the PR:

- **Go** if `table` (or `hotmatch`) shows a clear wall win on mandelbrot `.hyc`
  with matching checksums and no fib regression that swamps the win.
- **No-go / park G1** if both outlined paths lose or wash vs the giant match.
  Stable fn-ptr trampolines share one call site with the match jump table; the
  musttail *prediction* win is not available without nightly `become`. Do not
  migrate the rest of the ISA onto a losing trampoline.

## I-cache / inlining

- Hot loop is a separate `#[inline(never)]` function from `execute`.
- Table handlers are `#[inline(never)]` so LTO does not paste them into one
  mega-function (and so objdump can show `callq *handler`).
- Shared `#[inline(always)]` helpers keep match-path and table semantics
  identical (no opcode duplication).

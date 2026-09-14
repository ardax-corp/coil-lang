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

`poop` panics on this host (`perf_event` / stripped binary); wall A/B used `hyperfine`.

## Measured (release, fat LTO, `COIL_AUTO_PAR=0`, same `.hyc`)

Confirm run (~30 min-runs, x86_64):

| Bench | match | table | hotmatch |
|---|---|---|---|
| mandelbrot | 27.2 ± 0.4 ms | **24.1 ± 1.3 ms** (~1.13× vs match) | 26.1 ± 0.3 ms (~1.04×) |
| fib | 67.3 ± 1.7 ms | 66.3 ± 0.8 ms (wash) | 66.1 ± 0.6 ms (wash) |

Checksums identical across the three modes.

## Go / no-go for G1 (COI-374)

**Go.** The outlined fn-pointer trampoline is a measurable wall win on
mandelbrot `.hyc` (~13%) with a fib wash (CALL-heavy, as expected). Compact
`hotmatch` also beats the giant match, but loses to `table`.

G1 should grow the hot table (remaining dense heap ops, fused `BinSlot*`,
`CALL`/`RETURN`/`TailCall` only after a dedicated A/B). Keep `become` /
nightly off the production path. Do not treat inner `fused.rs` eval tables
as the same result; that path was ~2% slower.

## I-cache / inlining

- Hot loop is a separate `#[inline(never)]` function from `execute`.
- Table handlers are `#[inline(never)]` so LTO does not paste them into one
  mega-function (and so objdump can show `callq *handler`).
- Shared `#[inline(always)]` helpers keep match-path and table semantics
  identical (no opcode duplication).

# Perf free tickets (investigation, 2026-09-14)

Investigation only. Tip: `cf7a193c` (COI-371, current `main`). No compiler/VM
behavior was changed. This note ranks **sound, no-new-surface** follow-ups
against measurements of the flagship `.hy` benches.

**Verdict on bounds checks:** the gut feeling is **false for flagships and the
index-heavy hit benches we opened**. Hot loops do not emit stack `Index` /
`StoreIndex`. They use MIR `DenseIndex` / `DenseStoreIndex` with
`HEAP_UNCHECKED` (bit 0), or SIMD `VLoad`/`VStore`/`VReduce`, or no heap index
at all (`mandelbrot` / `tak` / `fib` / `binary_trees`). Residual length checks
are not a free win on this set. What *is* left on the index path is
**`find_object_by_addr` on every dense index** (slab probe, not a `HashSet`),
plus interpreter dispatch around it.

## Method

| Item | What we used |
|------|----------------|
| Binary | `cargo build --release` (`opt-level=3`, fat LTO, `panic=abort`). Wall times on the **stripped** `coil` (~6.5 MB). Flamegraphs on a **symbolful** rebuild (`CARGO_PROFILE_RELEASE_STRIP=false`, same opt). |
| Flags | `COIL_AUTO_PAR=0` (fair sequential; no IPA). |
| Modules | CLI does **not** read `[module].roots` from `coil.toml`. All `.hy` compiles used `--root .deps/coil-stdlib/src --root examples/src` (same extra roots tests bind). |
| Modes | `coil compile` → `.hyc`; `coil run file.hyc`; compile-and-run `coil file.hy`; `coil package` → `coil-embed` runner. |
| Wall | `hyperfine` 12 runs, 3 warmup. |
| Flames | `perf record -e cpu-clock -F 997 --call-graph dwarf` (looped short jobs). |
| Opcodes | `coil dissect` + `artifacts/perf-free-tickets/parse_dissect.py`. |
| Host | 4× “Intel Xeon” nested VM, Linux 6.12.94+, rustc 1.98.1. |

**Hardware counters are unavailable** on this VM (`perf list` has software
events only; `cycles` / `instructions` / `branches` / `branch-misses` are
`<not supported>` even with `perf_event_paranoid=0`). `poop` panics here
(`reached unreachable code`) for the same reason. Branch-miss *rates* are
therefore inferred from wall-time additivity and where `cpu-clock` samples
sit, not from PMU.

Raw dumps: [`artifacts/perf-free-tickets/`](../../artifacts/perf-free-tickets/).

## Wall time (ms, mean)

`hy ≈ compile + hyc` within ~1–2 ms on every flagship. Compile-and-run is not
a slower VM; it is **compiler + execute in one process**.

| Bench | compile | `coil run .hyc` | `coil .hy` | packaged `coil-embed` | Checksum |
|-------|--------:|----------------:|-----------:|----------------------:|----------|
| `mandelbrot` | 14.7 | 23.8 | 37.5 | 23.2 | 625885 |
| `tak` | 13.0 | 2.2 | 14.0 | 3.1 | 7 |
| `nsieve` | 14.2 | 2.3 | 15.1 | 3.2 | 1900 |
| `binary_trees` | 14.9 | 12.9 | 26.3 | 13.8 | 135854 |
| `fib` | 12.5 | 53.6 | 64.8 | 47.6 | 2178309 |
| `for_in_sum` | — | 3.9 | — | — | 12884115456 |
| `vec_scan` | — | 2.5 | — | — | 536739840 |

Packaged ≈ `.hyc` on CPU-long jobs (`mandelbrot`, `binary_trees`). On
`fib`, `coil-embed` is ~11% faster than full `coil run` (no compiler linked).
On 2 ms jobs (`tak`, `nsieve`) packaged looks slightly slower: process
start / trailer, not a different interpreter.

Historical `perf_matrix` (2026-08) had `mandelbrot` ~31 ms / `nsieve` ~2.5 ms
on a different machine; this host is in the same ballpark for `nsieve` /
`binary_trees` / `tak` and faster on `mandelbrot` (dense float MIR).

## Index vs Unchecked (static)

Hot function only (stdlib `write_all` always has one cold `ArrayLen`).

| Function | `Index` | `IndexUnchecked` | `IndexPin*` | `DenseIndex` | `DenseStoreIndex` | Unchecked bit | Other |
|----------|--------:|------------------:|------------:|-------------:|------------------:|---------------|-------|
| `mandelbrot` | 0 | 0 | 0 | 0 | 0 | — | `DenseBin`×18, `DenseCast`, `BinSlotSlotJmpf` |
| `tak` | 0 | 0 | 0 | 0 | 0 | — | 3×`CALL` + 1×`TailCall` |
| `nsieve` | 0 | 0 | 0 | 1 | 1 | **yes** (`flags=1`) | `DenseArrayPush` fill; 3×`BinSlotSlotJmpf` |
| `binary_trees::bottom_up` | 0 | 0 | 0 | 0 | 0 | — | 2×`CALL` + `MakeEnum` |
| `binary_trees::item_check` | 0 | 0 | 0 | 0 | 0 | — | `JumpIfMatch` + `Unpack` + 2×`CALL` |
| `fib` | 0 | 0 | 0 | 0 | 0 | — | 2×`CALL` + `BinReturn ADD` |
| `sum` (`for_in_sum`) | 0 | 0 | 0 | 1 | 0 | **yes** | `VLoad`+`VReduce` main trip; dense remainder |
| `scan` / `fill` (`vec_scan`) | 0 | 0 | 0 | 1 | 1 | **yes** | `VLoad`/`VStore`/`VBin`; dense remainder |
| `indexed_sum::sum` | 0 | 0 | 0 | 1 | 0 | **yes** | same shape as `scan` |

`loop_bounds` still reports work (`nsieve` `--opt-stats-json`:
`loop_bounds` `ops_delta=6`). Those proofs are consumed by MIR lower: the
pin opcodes from archive minor 13 are **not** what flagship `.hyc` execute
on the hot path anymore. `DenseIndex` does not consult the pin table.

Relevant VM code (unchanged):

- `read_indexed(..., unchecked)` skips the `0 <= i < len` test and uses
  `get_unchecked` when the bit is set (`machine/src/vm.rs`).
- `Instruction::DenseIndex` still does `find_object_by_addr` every time,
  then `read_indexed` (`HEAP_UNCHECKED` only kills the length check).
- Stack `IndexPin*` skip the slab probe via `frame_pins`. Dense native does
  not.

`array_mut.hy` is a useful negative: `[T; 8]` + `i % 8` becomes **slot SROA**
(compare-and-`LOAD`/`STORE` ladder), zero heap index.

## Branch misses: what is actually missing

PMU branch-misses were **not collected**. The compile-and-run vs `.hyc`
story is still clear:

1. **Wall time:** `hy` = compile + execute. For `tak` / `nsieve`, compile is
   ~85–90% of `hy`. A process that “misses more branches” in that mode is
   mostly **not in the VM**.
2. **`cpu-clock` attribution** (dwarf; overlapping names can exceed 100%):

| Recording | `compiler::`/`parser::`/`chumsky::` | `machine::` |
|-----------|------------------------------------:|------------:|
| `compile` mandelbrot | 98.1% | 0.0% |
| `coil .hy` tak | 60.8% | 7.2% |
| `coil .hy` mandelbrot | 38.0% | 59.5% |
| `coil run` mandelbrot / fib | ~0% | ~99% |
| `coil run` nsieve | ~0% | 74% (rest libc/kernel: malloc, faults) |
| packaged fib | 0% | 95% |

3. **Compile-phase samples** are parser (`chumsky`/`Pratt`), HM
   (`Ty::clone`, `subst`, `unify`, `HashMap<NodeId, Ty>`), codegen
   (`CodeBuf::append`, `pad_debug_locs`), then MIR specialize. That is a
   branchy Rust compiler, not `BinSlotSlotJmpf`.
4. **Execute-phase samples** collapse into
   `<machine::vm::Machine<256>>::execute` because the opcode `match` is
   inlined. That **is** a large indirect-ish branch (one `match` on
   `Instruction`). Flagship inner loops still take many distinct ops
   (`DenseBin` / `DenseMove` / `Jmpf` / `CALL`), so the predictor sees a
   repeating mix, not a single hot arm.

**Improvable (execute):** interpreter dispatch (computed goto / tail-call
threading) — structural, not a one-line free ticket. **Improvable
(compile-and-run only):** debug loc padding, `Ty` traffic, chumsky — DX, not
`.hyc` / package.

`.hyc` and packaged runs skip the compiler; they should show the *lower*
branch-miss total because they never run that match soup. Nothing is “missing”
from the archive relative to compile-and-run except the compile itself.

## Other bottlenecks (flames + dissect)

### `mandelbrot` (~24 ms `.hyc`)

Hot body is dense float: `DenseCast` / `DenseBin` / `DenseMove` /
`BinSlotSlotJmpf` / `JMP`. Flame: ~98% `Machine::execute`. No index, no
`HostInvoke` in the kernel. Gap vs Lua/Node is **numeric interpreter
dispatch**, not bounds checks. `Seek slot=37` is once per call (dense frame),
not the inner loop.

### `tak` / `fib`

`tak`: three `CALL` + one `TailCall`. `fib`: two `CALL` + `BinReturn ADD`
(cannot tail both). Flames: ~99% `execute` (`fib`); `tak` also memcpy from
frame setup. Free-ish leftovers: peel more `LOAD` packing (already
`LOAD s0,s1`), not bounds. Larger: calling convention / frame, or JIT.

### `nsieve` (~2.3 ms `.hyc`)

Hot: `DenseIndex` + `BinSlotImmJmpf EQ` + stride `DenseStoreIndex`, all
unchecked. Fill uses `DenseArrayPush`. Samples outside inlined `execute`:
`array_push_value` (~5%), `values_eq` (the `== 1` after index), slab alloc /
`_int_malloc` on grow. **Not** `Index`. A still-paid heap tax is identity
lookup inside each dense index (inlined, so it does not appear as its own
frame).

### `binary_trees` (~13 ms `.hyc`)

`MakeEnum` + recursive `CALL` + `JumpIfMatch`/`Unpack`. Flame: `Heap::dealloc`,
`immortal_unit_enum`, `Slab::alloc`, `gc_mark_slice` / `mark_references`. This
is GC + boxed tree nodes, not indexing.

### `for_in_sum` / `vec_scan`

Main trip is SIMD (`VLoad`/`VReduce` or `VStore`). Remainder is
`DenseIndex`/`DenseStoreIndex` with `HEAP_UNCHECKED` for `< LANES` tails.
Not a flagship-sized bounds leak.

### `main` I/O

Every bench ends with `format("%i", …)` + `to_bytes` + `io::sync::write_all`
(`HostInvoke` `write_from`, `JumpIfMatch` on `Result`). Cold for these
checksums (one short write). Do not optimize `write_all` for mandelbrot.

## Ranked tickets

### Free / near-free (semantics unchanged, no new language surface)

| Rank | Ticket | Evidence | Why not “just bounds” |
|------|--------|----------|------------------------|
| 1 | **Cache `Object` across proven `DenseIndex`/`DenseStoreIndex` loops** (reuse pin table or a dense-side handle; no new opcode if `ArrayPin` facts already exist) | Flagship `nsieve` + hit `vec_scan`/`for_in_sum` are already `HEAP_UNCHECKED` but still `find_object_by_addr` per op. Pins were built exactly to skip that probe on stack `IndexPin*`. | Length check already gone. |
| 2 | **Keep proving counted loops; do not expect more Unchecked on flagships** | Static counts: zero stack `Index` on this set. `LEQ`/`GEQ` still refused ([optimization-roadmap.md](optimization-roadmap.md) `loop_bounds` ceiling) — none of these kernels use that header. | Further Index→Unchecked on `mandelbrot`/`tak`/`fib` is a no-op. |
| 3 | **`coil-embed` / `.hyc` as the perf path** | `hy` is compile+run; `fib` package already beats `coil run`. Measuring “runtime” via `coil file.hy` mixes compiler noise (and would inflate branch misses). | Process choice, not an opt pass. |
| 4 | **Compile DX only:** `pad_debug_locs`, `Ty::clone` | Dominates `coil compile` flames. `coil package --strip-debug` already exists for artifacts. | Zero effect on `.hyc` execute. |

### Larger / structural (still sound, not “free”)

| Rank | Work | Evidence |
|------|------|----------|
| A | Interpreter dispatch (threaded / computed-goto `execute`, or split the mega-`match`) | ~99% of `.hyc` samples in `Machine::execute` for mandelbrot/fib; many live opcodes in the inner loop. |
| B | CALL / frame for `fib` / `tak` | Bytecode is already packed (`BinSlotImm` + `CALL` / `TailCall`). Next step is ABI or JIT, not Unchecked. |
| C | `binary_trees` allocation / GC | `MakeEnum` per node; mark/sweep shows up. Unboxed nodes or bump nursery would be a layout project ([heap-identity.md](heap-identity.md) already dropped the live HashSet). |
| D | `nsieve` fill grow + `values_eq` | `DenseArrayPush` / malloc on init; `== 1` goes through `Value` equality. Bit-vector / `byte` flags would be a bench rewrite — out of scope. |
| E | Typed dense arrays (`int` payload, not `Value`) | Would remove tag traffic on `DenseIndex` and `values_eq`. ABI/GC work. |
| F | JIT | Still the roadmap ceiling vs Lua on mandelbrot; not a free ticket. **No PGO.** |

### Discarded

- “Flagships still pay checked `Index`.” **Disproved** (dissect).
- “Pin opcodes are the hot path.” **Stale** for these MIR-kept bodies; pins
  remain the product for *stack* proven loops that **lose** the dense cost
  gate.
- “Format/`write_all` is a mandelbrot bottleneck.” **Cold**.
- “Packaged is a different VM.” Same `execute`; `coil-embed` is a smaller
  loader.

## Repro

```bash
export COIL_AUTO_PAR=0
cargo build --release --bin coil --bin coil-embed --bin coil-dissect
R=(--root .deps/coil-stdlib/src --root examples/src)
./target/release/coil dissect "${R[@]}" examples/perf/nsieve.hy --fn nsieve
./target/release/coil compile "${R[@]}" examples/perf/nsieve.hy -o /tmp/nsieve.hyc
hyperfine --warmup 3 --runs 12 \
  "./target/release/coil compile ${R[*]} examples/perf/nsieve.hy -o /tmp/nsieve.hyc" \
  "./target/release/coil run /tmp/nsieve.hyc" \
  "./target/release/coil ${R[*]} examples/perf/nsieve.hy"
```

Helpers: `artifacts/perf-free-tickets/collect.sh`, `parse_dissect.py`.
Flame SVGs: `artifacts/perf-free-tickets/flames/*.svg`.

## Related

- [array-pin.md](array-pin.md), [heap-identity.md](heap-identity.md)
- [optimization-roadmap.md](optimization-roadmap.md), [mir-dense-leftovers.md](mir-dense-leftovers.md)
- `compiler/tests/perf_metrics.rs` still asserts nsieve pin *rewrites* on IL;
  production `.hyc` after specialize is dense-unchecked, which is the stronger
  form.

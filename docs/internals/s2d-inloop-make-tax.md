# S2d / S2e in-loop Make* dense tax

S2d ([PR #365](https://github.com/ardax-corp/coil-lang/pull/365)) kept
**in-loop** `Make*` off dense after an ~18% regress. S2e
([COI-316](https://linear.app/ardax/issue/COI-316)) removes per-residual
`Seek` restore. In-loop Make* stays **refused**: Seek cut is real
(pack `Seek` 3→1, bump 5→1) but dense is still 8–17% slower than
fuse-IL. Historical A/B below; S2e board at the end.

`COIL_S2D_DENSE_INLOOP=1` forces in-loop dense at compile time (A/B).

## Where the 18% came from

Not the landed `examples/perf/looping_makearray.hy` preheader `bump`. PR #365 /
agent `bc-488328c4` first put an in-loop `pack` in that path, N=`2000000`:

```hy
fn pack(int n) -> int {
    let i = 0;
    let s = 0;
    while i < n {
        let xs = [i, i + 1, i + 2];
        s = s + xs[i % 3];
        i = i + 1;
    }
    return s;
}
```

Computed `i % 3` stops mem_fwd from DCE'ing the alloc. Original hyperfine
(fixed current `coil run`): parent fuse-IL **288.9 ± 9.9 ms**, dense
**340.3 ± 5.2 ms** (**1.18×**), checksum **`2000000999999`**. After the
regress they rewrote the file to preheader `bump` (~+11%).

This branch restores `pack` as `examples/perf/s2d_inloop_pack.hy`. Dense
in-loop is off unless `COIL_S2D_DENSE_INLOOP=1` at **compile** time.

Commands (this host, tip `cursor/s2d-inloop-make-tax-6ad5`, parent compiler
`a5cd908` / S3b):

```bash
PARENT=/tmp/coil-s3b/target/release/coil   # a5cd908
CUR=./target/release/coil
R=(--root .deps/coil-stdlib/src)
"$PARENT" compile "${R[@]}" examples/perf/s2d_inloop_pack.hy -o pack.parent.hyc
COIL_S2D_DENSE_INLOOP=1 "$CUR" compile "${R[@]}" examples/perf/s2d_inloop_pack.hy -o pack.dense.hyc
# tip without the env == parent archive (true fuse-IL; LIR cost gate misses)
"$CUR" compile "${R[@]}" examples/perf/s2d_inloop_pack.hy -o pack.tip.hyc
hyperfine -w 2 -r 8 "$CUR run pack.parent.hyc" "$CUR run pack.tip.hyc" "$CUR run pack.dense.hyc"
```

## Reproduced numbers (same VM = current release `coil run`)

| Body | Checksum | parent / tip fuse-IL | dense in-loop | dense vs fuse |
|------|----------|----------------------|---------------|---------------|
| `s2d_inloop_pack` N=2e6 arity-3 Index | `2000000999999` | 271.8 ± 2.1 ms | 319.2 ± 1.8 ms | **1.17× slower** |
| `s2d_inloop_pack_arith` extra `*3+1` | `6000004999997` | 294.4 ± 1.9 ms | 338.8 ± 3.0 ms | **1.15×** |
| `s2d_inloop_pack_wide` N=5e5 arity-8 | `125001500000` | 89.6 ± 0.5 ms | 105.5 ± 1.9 ms | **1.18×** |
| `s2d_inloop_pack_store` Make+StoreIndex+Index | `1999999000000` | 487.4 ± 3.4 ms | 516.9 ± 0.9 ms | **1.06×** |
| `s2d_preheader_bump` (landed hit) | `200000` (raise) | 8.5 ± 0.3 ms | 7.4 ± 0.2 ms | **1.14× faster** |

Tip vs parent archives are **byte-identical** on every in-loop kernel
(`sha256` match). LIR does not replace fuse-IL for these bodies
(`lir_emit_cost` + Seek/StorePop weights). Preheader `bump` tip == dense.

Callgrind N=50k `pack` (stripped `coil`): fuse-IL **136.3M** Ir vs dense
**151.7M** Ir (**+11%** instructions). `malloc` / `_int_free` Ir counts are
**identical**. Extra work is interpreter dispatch / stack residuals, not a
second heap path.

## Instruction mix (`coil-dissect --fn pack|bump`)

Static counts on the specialized fn:

| Body | path | nops | Seek | MakeArray | Index | StoreIndex | DenseBin |
|------|------|------|------|-----------|-------|------------|----------|
| pack | fuse | 18 | 0 | 1 | 1 | 0 | 0 |
| pack | dense | 25 | **3** | 1 | 1 | 0 | 4 |
| pack_wide | fuse | 24 | 0 | 1 | 1 | 0 | 0 |
| pack_wide | dense | 37 | **3** | 1 | 1 | 0 | 9 |
| pack_store | fuse | 30 | 0 | **2** | 1 | 1 | 0 |
| pack_store | dense | 32 | **5** | **2** | 1 | 1 | 3 |
| bump | fuse | 41 | 0 | 2 | 1 | 1 | 0 |
| bump | dense | 35 | 5 | 2 | 1 | 1 | 3 |

Dense pack loop (hot, N=2e6):

- prologue `Seek` frame **22**
- per trip: `MakeArray` → `STORE` → **`Seek 22`** (GcBarrier copy `LOAD`/`STORE`) → `Index` → `STORE` → **`Seek 22`**
- `restore_dense_tell` in `compiler/src/mir/emit.rs` after every stack residual (`Alloc`, `Index`, `StoreIndex`, `Call`, `HostInvoke`)

Fuse-IL pack loop: invert+fuse `BinSlotImmStore` / `BinSlotImm` / `BinSlotSlotJmpf`, **no Seek**, MakeArray elems stay on the eval stack.

Both paths allocate **once per trip** (same `MakeArray` fast path). Fuse also
builds `[i, i+1, i+2]` with two fused slot stores; dense materializes those as
`DenseBin` then `LOAD`s them for `MakeArray`.

`pack_store` is a worse SSA reconstruct: **two** `MakeArray`s per trip on
**both** fuse and dense (StoreIndex result is a new object; the later Index
rebuilds `[0,0,0]` instead of reading the stored array). Dense then pays **four
loop Seeks**. Alloc dominates, so the Seek tax is a smaller fraction (**+6%**).

## Cause ranking

1. **Structural: Seek-restore + spill around every dense residual** (dominant extra). Dense frame is 22–47 slots; each Make*/Index/StoreIndex drops tell to the eval stack then `Seek`s it back. Fuse never does this.
2. **Shared: heap `MakeArray` + GC after alloc.** Same malloc/free Ir. Not the 18% delta. Still the majority of *absolute* time on in-loop alloc kernels.
3. **Fuse-IL shape bonus (real, secondary).** `BinSlotImmStore` for `i+1`/`i+2` and fused MOD/ADD vs DenseBin+DenseMove+LOAD. The original kernel is a light-arith / alloc-heavy loop, so fuse’s convoy is visible. Extra integer work only trims 17% → 15%; arity-8 stays 18%.
4. **Not SROA / specialize refuse / missing maps.** Maps bind; dense is sound. Const-index SROA would DCE the alloc (not this computed-index kernel). `nsieve` / `binary_trees` / `array_mut` `main` never take this path (`Vec.push` / I4 `write_all` in `main`).
5. **Not LIR winning on tip.** In-loop numeric Make* stays fuse-IL; compare-only leftovers may LIR.

## Answers

**(a) Fuse-biased microbench?** Partly. The original `pack` is invert+fuse’s
home turf (tiny counted loop, fused slot ops, one cheap Index). It is still a
fair “in-loop MakeArray + computed index” kernel, and **wider / more-arith /
store** variants keep a dense loss (6–18%). The 18% is **not** a one-shape
fluke, and **not** only fuse winning: callgrind shows extra Ir with identical
malloc.

**(b) Dominant cost of dense in-loop Make*?** Per-residual **Seek +
LOAD/STORE boxing** onto a large dense frame, on top of the shared alloc.
Two Seeks per Index+Make trip on `pack`.

**(c) Fix directions (discussion, not this PR)**

| Direction | Upside | Risk |
|-----------|--------|------|
| Emit Make*/Index **without** `restore_dense_tell` (tell overlay / pin-less dense heap ops) | Removes 2 Seeks/trip on `pack`; likely most of the 11–18% Ir gap | Frame high-water / GC maps / leftover stack ops must still be sound |
| Coalesce one Seek per basic block | Smaller win if several residuals in a row | pack already has Seek immediately after each residual |
| Skip GcBarrier dest copy when regs alias | Drops LOAD/STORE pair after MakeArray | Must keep maps if dest ≠ obj |
| Dense cost gate (like LIR) | Avoids shipping a slower body | Does not make dense *faster* |
| MIR→LIR instead of dense | Unlikely: cost gate already prefers fuse | Same spill pattern in `emit_lir` if it won |
| Escape / stack SROA of `[i,i+1,i+2]` | Could delete the alloc (huge) | Known miscompile on `vec_array.hy`; computed index |
| Hoist/batch Make outside loop | Only if semantics allow (not this kernel) | Different program |
| Flagship Make* | `nsieve` is `Vec.push`, not Make* | Do not twist flagships for the bench |

## S2e ABI (COI-316)

Dense still opens with one prologue `Seek` to `max_reg+1`. Stack residuals
(`Make*` / `Index` / `StoreIndex` / `ArrayLen` / `CALL` / HostInvoke) box
through `LOAD` + residual + `StorePop`. `StorePop` is `DeltaThenFloor(-1,
dest+1)` from residual height 1, so tell lands back on the frame
high-water. `restore_dense_tell` was hybrid ABI glue, not a GC
requirement — maps already root live heap slots. S2e deletes those Seeks.

Pins stay fuse-IL: pin keys still do not survive the **prologue** Seek.

In-loop mapped Make* stays **off** by default. S2f (COI-315) SROAs
non-escaping `[T; N]` computed-index load/store so `pack_store` does not
rebuild `[0,0,0]` after `StoreIndex`.

## S2e board (coil-embed packaged, `COIL_AUTO_PAR=0`)

Same runner (`target/release/coil-embed`). Fuse = tip compiler +
`COIL_S2D_DENSE_INLOOP=0`. Dense = tip + `COIL_S2D_DENSE_INLOOP=1`.
Checksums match parent: pack `2000000999999`, arith `6000004999997`,
wide `125001500000`, store `1999999000000`, bump `200000`.

| Body | fuse-IL | Seek-less dense | dense vs fuse | Seek (dense) |
|------|---------|-----------------|---------------|--------------|
| pack N=2e6 | 263.5 ± 3.9 ms | 297.8 ± 2.2 ms | **1.13× slower** | 1 (was 3) |
| pack_arith | 279.5 ± 1.9 ms | 319.0 ± 3.0 ms | **1.14×** | 1 |
| pack_wide N=5e5 | 86.5 ± 0.6 ms | 101.0 ± 0.4 ms | **1.17×** | 1 |
| pack_store | 470.5 ± 2.8 ms | 507.5 ± 18.0 ms | **1.08×** | 1 (was 5) |
| preheader bump | (already dense) | 7.4 ± 0.2 ms | — | 1 (was 5) |

Flagships vs parent `35d98291`: **byte-identical** archives
(`mandelbrot` / `tak` / `nsieve` / `binary_trees` / `fib`). Checksums
625885 / 7 / 1900 / 135854 / 2178309.

**Decision:** keep in-loop Make* refused. Remaining tax is residual
`LOAD`/`STORE` boxing onto the dense frame vs invert+fuse
`BinSlotImmStore`, not Seek. Preheader / S3b index bodies keep the
Seek-less win (prologue `Seek` only).

## Flagships

`nsieve` / `binary_trees` / `array_mut` / `gc_churn` do not take in-loop
Make* dense (`Vec.push` / I4 / classes).

## S2f SROA / StoreIndex reuse (COI-315)

Codegen already exploded `[T; N]` locals into slots for **const** index.
Computed `xs[i % 3] = i; s += xs[i % 3]` boxed twice per trip (`MakeArray`
from stale zeros, then `Index` of a *new* `[0,0,0]`). VM `StoreIndex`
mutates in place; the rematerialized array never saw the store — so
`pack_store` returned `0` (the old raise used a >i32 literal and did not
fire).

S2f:

- **Landed:** slot-select SROA for non-escaping `[T; N]` computed index
  (load / store / `+=` / `++`). `i % N` is treated as in-range.
  MIR `sroa` rewrites a second `Alloc` of the same elems after
  `StoreIndex` to the mutated array. MIR LICM may hoist invariant
  `Alloc`/`GcBarrier` when the loop has no `StoreIndex` / `CALL`.
- **Refused:** escaping / returned / call-arg / `ArrayPush` / field /
  host arrays; observed escape (`vec_array.hy`); arity > 32; named
  class SROA; negative `i % N` (last slot, not OOB); in-loop Make*
  **dense** (S2e boxing tax). Non-escaping computed *elements*
  (`[i,i+1,i+2]`) still SROA into slots.

`pack` / `pack_arith` / `pack_wide` / `pack_store` SROA when the local is
`[T; N]` and the only uses are computed-index load/store (`i % N`).
Computed *element* values (`i`, `i+1`) still materialize into slots;
they do not stay heap. Escape / `vec_array.hy` still heap.

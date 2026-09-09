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

`pack_store` **was** a worse SSA reconstruct: **two** `MakeArray`s per trip
on both fuse and dense (Index rebuilt `[0,0,0]`). S2f SROAs that body
(0 `MakeArray`). Historical mix below is parent / S2e.

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

- **Landed:** slot-select SROA for non-escaping `[T; N]` when the index
  is proven (`i % N` or sidecar in-bounds). Binop lhs is spilled
  (`expr_may_clobber`); select diamonds stay fuse-IL (dense reconstruct
  drops last-arm stores). MIR `sroa` reuses the StoreIndex array.
  MIR LICM may hoist invariant `Alloc`/`GcBarrier` when the loop has
  no `StoreIndex` / `CALL`. Use `panic` (not `raise`) for checksums.
- **Refused:** growing `ArrayPush` dest; private use after an escape;
  unproven `xs[k]` as a raw slot; slot-SROA of observed computed elems
  (`vec_array.hy`); arity > 32; named
  class SROA; negative `i % N` (last slot, not OOB); in-loop Make*
  **dense** (S2e boxing tax). Non-escaping computed *elements*
  (`[i,i+1,i+2]`) still SROA into slots.
- **S2g (COI-317):** a named escape (return, call-arg, `ArrayPush` *value*,
  field store, HostInvoke / print) keeps slots in the private region and
  emits one `MakeArray` at the edge. Multiple snapshot boxes are allowed
  when no private use follows the first escape. Identity is not preserved
  across edges (each box is a fresh heap object).
- **S2h (COI-318):** unproven `xs[k]` is never a raw slot. Codegen `[T; N]`
  locals take a weaker bound (`i % m` with `m <= N`, sidecar in-bounds, or
  a runtime `0 <= k < N` check) and still SROA; the cold arm is heap
  `Index` / `StoreIndex` so OOB panics. Leftover IL `MakeArray` + `xs[k]`
  stays a heap object (checked Index).
- **S2i (COI-319):** observed/escape + computed elems (`vec_array.hy`) stay
  a heap object with sound `Index` / `StoreIndex`. Zip/broadcast operands
  that are literals or stack-array locals load slots (S2f); the result is
  not slot-SROA'd. Remaining refuse: grow dest, private after escape,
  arity > 32, named class SROA, negative remainder, slot-SROA of computed
  elems.

`pack` / `pack_arith` / `pack_wide` / `pack_store` SROA when the local is
`[T; N]` and the only uses are computed-index load/store (`i % N`).
Computed *element* values (`i`, `i+1`) still materialize into slots
when the local does not escape. Observed zip/broadcast results stay heap.

Prove vs parent `3bcaf9e8` (same tip `coil run`, `COIL_AUTO_PAR=0`,
hyperfine -w 2 -r 8). `coil-dissect --fn pack` / `bump`:

| kernel | parent MakeArray | tip MakeArray | parent StoreIndex | tip StoreIndex |
|---|---|---|---|---|
| pack_store | **2** | **0** | 1 | 0 |
| pack / pack_arith / pack_wide | 1 | 0 | 0 | 0 |
| bump | 2 (dense) | 0 | 1 | 0 |

`COIL_S2D_DENSE_INLOOP=1` matches tip (select bodies skip dense).

| kernel | parent | tip | ratio |
|---|---|---|---|
| pack_store N=2e6 | 493.6 ± 1.4 ms | 121.6 ± 1.0 ms | **4.06×** |
| pack N=2e6 | 286.1 ± 1.2 ms | 77.6 ± 1.0 ms | **3.69×** |
| pack_arith | 306.9 ± 0.4 ms | 95.6 ± 1.0 ms | **3.21×** |
| pack_wide N=5e5 | 92.8 ± 0.4 ms | 46.7 ± 0.3 ms | **1.99×** |
| bump N=2e5 (time-only) | 7.6 ± 1.2 ms | 12.9 ± 0.2 ms | tip **1.71×** slower |

Parent `pack_store` / `bump` return **0** (Index rematerializes zeros;
`raise` hid it). Tip: `pack(6)==15`, `pack(2e6)==1999999000000`,
`bump()==200000`. Do not use coil `n*(n-1)/2` at this magnitude.

Flagship `.hyc` sha256 identical: `mandelbrot` / `tak` / `nsieve` /
`binary_trees` / `fib`.

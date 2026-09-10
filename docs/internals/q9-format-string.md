# Q9 — Full format / string on MIR (reopen I4)

Linear: [COI-332](https://linear.app/ardax/issue/COI-332/q9-full-formatstring-on-mir-reopen-i4).
Spec: [language-quirks.md](language-quirks.md) Q9. Island: [mir-islands.md](mir-islands.md) I4.

I4 (#345) closed `FORMAT` / `STRING` / `STRINGIFY` / `PRINT` as a **hard
MIR barrier**. Q9 reopens that island as a **delivery ladder**, not a
forever refuse. Full format/string on MIR is the target. This note is
the phased design so later rungs do not invent a second Format lowering.

## Choice

**Reuse the shipped opcodes.** SSA names the same `STRING` / `PRINT` /
`FORMAT` / `STRINGIFY` and `string::{from_bytes,to_bytes}` HostInvoke the
VM already runs. Reconstruct table ops on MIR→LIR. Densify byte hosts at
the I6 box edge (R2). Do **not** add a half Format compiler, a second
specifier walker, or vanity string benches.

Dense numeric / array paths stay off table `STRING` / `FORMAT`. R2 opens
the bytes HostInvoke edge. R3 maps `FORMAT` / `STRINGIFY` like I5 alloc.
Flagships that never print stay identical. Keep/refuse is checksum +
cost gate. No env toggle. No PGO.

## Ladder

| Rung | What enters MIR | Emit | Still refuse |
|------|-----------------|------|--------------|
| **R1** | Table `STRING`, `PRINT`, `FORMAT`, `STRINGIFY` as SSA (`HeapRef` / IO token) | MIR→LIR reconstruct of the same IL. Dense infer still refuses so numeric specialize is unchanged | unicode / regex; format-in-loop dense |
| **R2** | `string::{from_bytes,to_bytes}` as I6 HostInvoke on dense when maps/effects allow | Dense box at the host edge (same as other I6) | unicode / regex |
| **R3 (this PR)** | Live-heap maps across `FORMAT` / `STRINGIFY` (I5-style roots) so a format in a mapped loop can stay SSA | LIR when maps bind and cost ≤ fuse. Dense infer still refuses table ops | unicode / regex |
| **R4** | Unicode / regex — only if a later island says they belong in SSA | TBD | — |

Post-quirks rank: R2 is **B4**. **R3** is **B9**
([opt-generalization.md](opt-generalization.md) B0). R4 stays leftover
(no island asked for unicode / regex in SSA).

R1 is the reopen: I4 is no longer a hard LIR wall. A prove body
(`STRING` + `PRINT`, or `format("%i", n)` + `RETURN`) can lower. The
`IlModule` cost gate may still keep fuse-IL when reconstruct is
heavier (`Seek` / `STORE`). That is a lose, not a barrier.

## SSA shape (R1)

| IL | MIR | Effects |
|----|-----|---------|
| `IlOp::String` / `STRING` | `String { dest, idx }` → `HeapRef` | Pure (LICM may hoist) |
| `IlOp::Print` / `PRINT` | `Print { dest, src }` — dest is a unit token | IO barrier; never hoist; never DCE |
| `FORMAT n` | `Format { dest, fmt, args }` → `HeapRef` | Allocating / impure; not an I5 `Alloc` (no map requirement on R1) |
| `STRINGIFY` | `Stringify { dest, src }` → `HeapRef` | Same as Format |

`fmt` is the format-string `HeapRef` already on the stack. Args stay
typed word lanes (`i64` / `f64` / `HeapRef`). Reconstruct pops them in
the same order as fuse-IL (`FORMAT` operand is arity).

## Prove (R1)

- Unit: `try_lower_abi_body` on `STRING`+`RETURN`, `STRING`+`PRINT`,
  and `STRING`+`LOAD`+`FORMAT 1`+`RETURN` succeeds and emits the same
  opcodes (no new Format IR).
- `wrap_res` (`Result.Err("miss")`) is no longer a hard string refuse
  (two-slot `Result<int, string>` may still lose LIR verify until a
  later rung).
- `pipeline_format_loop_stays_fuse_il`: a `format` + i64 add loop still
  has `FORMAT` and **no** `DenseBin` (dense infer still refuses table I4).
- Flagships (`mandelbrot` / `tak` / `nsieve` / `binary_trees` / `fib`):
  identical archives or flat.

## Prove (R2)

- `dense_host_ok` is true for `from_bytes` / `to_bytes` (I6 word edge;
  still off the W4 float allowlist). Impure IO — LICM does not hoist.
- Unit: HostInvoke + `LOAD` reconstructs on dense emit; LIR emit still
  refuses HostInvoke (same as other I6). `from_bytes` keeps
  `HOST_ENUM_LAYOUT_RESULT_NICHE` on the reconstructed operand.
- `pipeline_to_bytes_loop_takes_dense` / `pipeline_from_bytes_loop_takes_dense`:
  a bytes-host + i64 add loop emits `HostInvoke` + `DenseBin`. STRING
  literals stay in `main`. Format loops stay fuse-IL.
- Cost gate unchanged. Flagships identical or flat. No env toggle. No PGO.

## Prove (R3)

- `FORMAT` / `STRINGIFY` pair `GcBarrier` and encode S2b drafts. Bind
  safepoints on those opcodes (same maps as I5 alloc / B6 grow).
- A format + i64 add loop still has `FORMAT` and **no** `DenseBin`.
- A live heap string across `format` is in the FORMAT slot map.
- `try_lower_abi_body` on `STRING`+`LOAD`+`FORMAT 1` still reconstructs
  the shipped opcode (maps required; no second Format IR).
- Flagships identical or flat. Cost gate unchanged. No env toggle. No PGO.

## Leftover after R3

- Dense specialize of table `STRING` / `PRINT` / `FORMAT` / `STRINGIFY`
  (only if cost wins; do not stall numeric / array specialize)
- Unicode / regex in SSA (**R4**) — no island has asked for that yet
- LIR reconstruct of HostInvoke
- Score-chasing string microbenches

## Non-goals (R3)

- New opcodes or a specifier interpreter in MIR
- A half second Format lowering
- Unicode / regex HostInvoke or SSA (R4)
- Env toggles / PGO

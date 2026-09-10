# Q9 — Full format / string on MIR (reopen I4)

Linear: [COI-332](https://linear.app/ardax/issue/COI-332/q9-full-formatstring-on-mir-reopen-i4).
Spec: [language-quirks.md](language-quirks.md) Q9. Island: [mir-islands.md](mir-islands.md) I4.

I4 (#345) closed `FORMAT` / `STRING` / `STRINGIFY` / `PRINT` as a **hard
MIR barrier**. Q9 reopens that island as a **delivery ladder**, not a
forever refuse. Full format/string on MIR is the target. This note is
the phased design so later rungs do not invent a second Format lowering.

## Choice

**Reuse the shipped opcodes.** SSA names the same `STRING` / `PRINT` /
`FORMAT` / `STRINGIFY` (and later `string::{from_bytes,to_bytes}`) the
VM already runs. Reconstruct them on MIR→LIR. Do **not** add a half
Format compiler, a second specifier walker, or vanity string benches.

Dense numeric / array paths stay off this island until a later rung
explicitly densifies a string edge. Flagships that never print stay
identical. Keep/refuse is checksum + cost gate. No env toggle. No PGO.

## Ladder

| Rung | What enters MIR | Emit | Still refuse |
|------|-----------------|------|--------------|
| **R1 (this PR)** | Table `STRING`, `PRINT`, `FORMAT`, `STRINGIFY` as SSA (`HeapRef` / IO token) | MIR→LIR reconstruct of the same IL. Dense infer still refuses so numeric specialize is unchanged | `from_bytes` / `to_bytes` dense HostInvoke; unicode / regex; format-in-loop dense |
| **R2** | `string::{from_bytes,to_bytes}` as I6 HostInvoke on dense when maps/effects allow | Dense box at the host edge (same as other I6) | unicode / regex |
| **R3** | Live-heap maps across `FORMAT` / `STRINGIFY` (I5-style roots) so a format in a mapped loop can stay SSA | LIR (then dense only if cost wins) | unicode / regex |
| **R4** | Unicode / regex — only if a later island says they belong in SSA | TBD | — |

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

## Prove

- Unit: `try_lower_abi_body` on `STRING`+`RETURN`, `STRING`+`PRINT`,
  and `STRING`+`LOAD`+`FORMAT 1`+`RETURN` succeeds and emits the same
  opcodes (no new Format IR).
- `wrap_res` (`Result.Err("miss")`) is no longer a hard string refuse
  (two-slot `Result<int, string>` may still lose LIR verify until a
  later rung).
- `pipeline_format_loop_stays_fuse_il`: a `format` + i64 add loop still
  has `FORMAT` and **no** `DenseBin` (dense infer still refuses I4).
- Flagships (`mandelbrot` / `tak` / `nsieve` / `binary_trees` / `fib`):
  identical archives or flat.

## Non-goals (this PR)

- New opcodes or a specifier interpreter in MIR
- Dense specialize of format/print loops
- Unicode / regex in SSA
- Score-chasing string microbenches

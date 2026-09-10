# MIR debugger / deopt boundaries (I7)

I7 ([COI-299](https://linear.app/ardax/issue/COI-299/i7-debugger-deopt-boundaries-on-mir))
names **stop** and **deopt** edges on the SSA sidecar so a later native
tier can leave safely. **B8** ([COI-346](https://linear.app/ardax/issue/COI-346/b8-i7-debuggerdeopt-mir-wall))
ladders the blanket fuse-IL refuse: debugger-attached and `-Og` may
dense / MIR→LIR. The VM debugger steps **reconstructed bytecode**. There
is no source-level MIR stepping rewrite.

## What exists today

- `MirInst::Deopt` (`deopt.stop` / `deopt.deopt`) plus implicit leave
  edges on `Call`, impure `HostInvoke`, `Alloc`, and `GcBarrier`
  (`MirInst::deopt_kind`).
- `LowerHints::allow_deopt` inserts explicit `Deopt` at those IL edges
  (and at `Return` / `Jump` / known-loc `StorePop`). Text form
  round-trips the kind; `DebugLoc` is carried on the inst.
- Dense emit and MIR→LIR **skip** explicit `Deopt` insts. They are
  sidecar markers — not encoded in the archive, and they do not refuse
  the body. Production `try_specialize_body` does **not** set
  `allow_deopt` (so vectorize / cost are not taxed by stop tokens).
- Debugger-attached compiles (`Pipeline::set_debugger_attached`, used by
  `coil debug`) and `-Og` / `OptLevel::Debug` **keep**
  `OptimizeOptions::mir_specialize`. `-Og` still uses Basic IL cleanup
  only (no slot promote, escape SROA, unroll, or GVN). Stops stay on
  the reconstruct (fuse-IL, LIR, or dense).

## Doctrine

| Kind | Meaning |
|------|---------|
| `Stop` | Interpreter may pause (line / `stepi`). Resume is the next bytecode PC. |
| `Deopt` | Leave a later native tier; resume the interpreter at this edge. |

Do not treat a `Deopt` dest (`bool` token) as a program value. Do not
encode these edges in the archive. Cranelift (P5) stays parked — a
future native tier must deopt or refuse at every `deopt_kind` edge and
must not run while a debug controller is attached.

## Remaining walls (B8)

- **No resume maps.** Skipping `Deopt` at emit is not a deopt map.
  Native leave is still unsound until maps exist. Do not set
  `allow_deopt` in production specialize.
- **Named locals after SSA remap.** `fn_debug_locals` is recorded
  pre-MIR. `print n` on params often still works (slot 0); remapped
  `let` slots can be stale. `print $N` stays slot-accurate.
- **Line table on reconstruct.** Dense / LIR emit still uses
  `DebugLoc::unknown()` for most insts. Function breakpoints and
  `stepi` work; line breakpoints stay sparse (same MVP as fuse-IL).
- Vectorize still refuses a body that already has explicit `Deopt`
  insts (`allow_deopt` on).

## Non-goals (this island / B8)

- Debugger UX / DAP / line-table rewrite
- Full MIR stepping
- Archive / opcode bump
- Growing HostInvoke **id** allowlists (hoist is purity bits)
- Native deopt maps / P5

See [mir-islands.md](mir-islands.md), [specialize-refuse.md](specialize-refuse.md),
and [debugger.md](debugger.md).

# MIR debugger / deopt boundaries (I7)

I7 ([COI-299](https://linear.app/ardax/issue/COI-299/i7-debugger-deopt-boundaries-on-mir))
names **stop** and **deopt** edges on the SSA sidecar so a later native or
denser tier can leave safely. The **VM debugger on fuse-IL bytecode** stays
the v1 stop engine. There is no source-level MIR stepping rewrite.

## What exists today

- `MirInst::Deopt` (`deopt.stop` / `deopt.deopt`) plus implicit leave
  edges on `Call`, impure `HostInvoke`, `Alloc`, and `GcBarrier`
  (`MirInst::deopt_kind`).
- `LowerHints::allow_deopt` inserts explicit `Deopt` at those IL edges
  (and at `Return` / `Jump` / known-loc `StorePop`). Text form
  round-trips the kind; `DebugLoc` is carried on the inst.
- Dense emit and MIR→LIR **refuse** a body that has an explicit deopt
  inst. Production `try_specialize_body` does **not** set `allow_deopt`.
- Debugger-attached compiles (`Pipeline::set_debugger_attached`, used by
  `coil debug`) and `-Og` / `OptLevel::Debug` set
  `OptimizeOptions::mir_specialize = false`, so `IlModule` skips dense
  specialize and MIR→LIR replace. Stepping stays on fuse-IL.

## Doctrine

| Kind | Meaning |
|------|---------|
| `Stop` | Interpreter may pause (line / `stepi`). Resume is the next fuse-IL PC. |
| `Deopt` | Leave specialized or native code; resume the interpreter at this edge. |

Do not treat a `Deopt` dest (`bool` token) as a program value. Do not
encode these edges in the archive. Cranelift (P5) stays parked — a
future native tier must deopt or refuse at every `deopt_kind` edge and
must not run while a debug controller is attached.

## Non-goals (this island)

- Debugger UX / DAP / line-table changes
- Full MIR stepping
- Archive / opcode bump
- Growing HostInvoke **id** allowlists (hoist is purity bits)
- I8 entry (see [mir-islands.md](mir-islands.md); this island only names edges)

See [mir-islands.md](mir-islands.md), [specialize-refuse.md](specialize-refuse.md),
and [debugger.md](debugger.md).

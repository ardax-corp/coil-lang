# MIR debugger / deopt boundaries (I7)

I7 ([COI-299](https://linear.app/ardax/issue/COI-299/i7-debugger-deopt-boundaries-on-mir))
names **stop** and **deopt** edges on the SSA sidecar so a later native
tier can leave safely. **B8** ([COI-346](https://linear.app/ardax/issue/COI-346/b8-i7-debuggerdeopt-mir-wall))
ladders the blanket fuse-IL refuse: debugger-attached and `-Og` may
dense / MIR→LIR. **C3** ([COI-350](https://linear.app/ardax/issue/COI-350/c3-native-deopt-maps-named-let-slots-sparse-debugloc))
adds compiler-internal resume maps, remaps named `let` slots after SSA
register assign, and forwards known `DebugLoc` through dense / LIR emit.
The VM debugger still steps **reconstructed bytecode**. There is no
source-level MIR stepping rewrite.

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
- **C3 resume maps** (`DraftDeoptMap`) record live IL slots and assigned
  reconstruct regs at every implicit leave edge (`Call`, impure
  `HostInvoke`, alloc / format / print) and at `Return` / `Jump` Stop
  sites. Maps are compiler-internal (like S2b drafts before bind). They
  are **not** written to `.hyc`. `complete` is false when a live SSA
  local has no reconstruct slot (stack-only / CALL convoy TOS). A future
  native tier must refuse incomplete maps — do not resume from them.
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

## Remaining walls (after C3)

- **No native resume.** Maps exist; P5 / Cranelift is still parked.
  The interpreter does not consume `DraftDeoptMap`. Do not set
  `allow_deopt` in production specialize.
- **Incomplete maps.** Stack-only / convoy TOS values have no register.
  `complete` is false; native must refuse those edges (over-approx slots
  is safe; under-approx is not).
- **PC-accurate named locals.** Remap is one name → one reconstruct
  slot (last / return-block def). Loop-carried `let`s that share a φ
  are the usual debugger approximation — not a per-PC live map.
  `print $N` stays slot-accurate.
- **Line table is still sparse.** C3 forwards known IL locs through
  MIR emit. Codegen sites that still emit `DebugLoc::unknown()`, fused
  first-slot-only spans, and opt-inserted insts stay unmapped.
  Function breakpoints and `stepi` work.
- Vectorize still refuses a body that already has explicit `Deopt`
  insts (`allow_deopt` on).

## Non-goals (this island / C3)

- Debugger UX / DAP / full line-table rewrite
- Full MIR stepping
- Archive / opcode bump
- Growing HostInvoke **id** allowlists (hoist is purity bits)
- P5 native resume / Cranelift

See [mir-islands.md](mir-islands.md), [specialize-refuse.md](specialize-refuse.md),
and [debugger.md](debugger.md).

# VM opcodes (builtins-related)

User code does not name these directly; the compiler emits them:

| Opcode | Role |
|--------|------|
| `PRINT` | Write string to output |
| `FORMAT` | Build formatted string from specifiers |
| `FfiLoad` | `dload` |
| `DeclareFFI` | `declare` |
| `FfiInvoke` | `invoke` |
| `HostInvoke` | Host-registered closure |
| `HostInvokeNiche` | Allocation-free niche `Option<T>` Vec native |
| `OptionNicheToHeap` / `HeapOptionToNiche` | Cross a pointer-niche `Option<T>` boundary |
| `PairToHeap` / `HeapToPair` | Box or unbox a unary `[payload, tag]` pair |
| `ReturnPair` | Return a unary pair without changing `Value` |
| `Panic` | Abort after writing `panic: <msg>` |
| `FloatChainStore` | Execute two or three source-ordered float stages and store (slots and/or const-pool operands; no FMA/reassoc) |
| `BinSlotSlotConstJmpf` | `BinSlotSlot` float-arith + pool `CONST` + float `CmpJmpf` in one dispatch (e.g. mandelbrot `|z|² > 4`) |
| `CmpJmpt` / `BinSlotImmJmpt` / `LogNotJmpt` / `BinSlotSlotJmpt` / `BinSlotSlotConstJmpt` | Jump-if-true twins of the `*Jmpf` family (same packing; fused invert of `*Jmpf; JMP`) |
| `IndexUnchecked` / `StoreIndexUnchecked` | Bounds-proofed array access from `il::bounds` counted-loop analysis |
| `NEGF` | Float unary negate (IEEE sign-bit flip); replaces `CONST -1; MULF` |
| `InitTyped` | Allocate a class instance stamped with a compile-time type id (operand); `INIT` remains for untyped bags / old archives |

---

## Related

- [pipeline](pipeline.md)
- [debug-info](debug-info.md)

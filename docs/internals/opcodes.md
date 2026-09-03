# VM opcodes (builtins-related)

User code does not name these directly; the compiler emits them:

| Opcode | Role |
|--------|------|
| `PRINT` | Write string to output |
| `FORMAT` | Build formatted string from specifiers |
| `FfiLoad` | `dload` |
| `DeclareFFI` | `declare` |
| `FfiInvoke` | `invoke` |
| `HostInvoke` | Host-registered closure. Operand `[15:0]` is arg count; stack is `fn_id, arg0, …, argN-1` (no args tuple). Standard table in `machine/src/host_natives.rs`. **120** = `stream_attach`, **121** = `stream_park`. See [io-reactor.md](io-reactor.md). |
| `HostInvokeNiche` | Tombstone (archive major 4); panics if executed |
| `OptionNicheToHeap` / `HeapOptionToNiche` | Tombstone (archive major 4); panics if executed |
| `PairToHeap` / `HeapToPair` | Tombstone (archive major 4); panics if executed |
| `ReturnPair` | Tombstone (archive major 4); panics if executed |
| `Panic` | Abort after writing `panic: <msg>` |
| `FloatChainStore` | Tombstone (not emitted; panics on major 4) |
| `BinSlotSlotConstJmpf` | Tombstone (not emitted; panics on major 4) |
| `CmpJmpt` / `BinSlotImmJmpt` / `LogNotJmpt` / `BinSlotSlotJmpt` / `BinSlotSlotConstJmpt` | Jump-if-true twins of the `*Jmpf` family (same packing; fused invert of `*Jmpf; JMP`) |
| `IndexUnchecked` / `StoreIndexUnchecked` | Bounds-proofed array access from `il::bounds` counted-loop analysis |
| `ArrayPin` / `IndexPin*` / `StoreIndexPin*` | Pinned array indexing: `ArrayPin` caches the array in the frame pin table; `IndexPin*` / `StoreIndexPin*` skip per-site `find_object_by_addr`. Layout: [array-pin.md](array-pin.md) |
| `NEGF` | Float unary negate (IEEE sign-bit flip); replaces `CONST -1; MULF` |
| `InitTyped` | Allocate a class instance. Operand `[31:16] field_count`, `[15:0] type_id`. Non-zero field count pre-sizes dense slots; `INIT` remains for untyped bags / old archives. Typed field get/set uses `LoadField` / indexed `SetField` (bit 31). |

---

## Related

- [pipeline](pipeline.md)
- [array-pin.md](array-pin.md)
- [debug-info](debug-info.md)

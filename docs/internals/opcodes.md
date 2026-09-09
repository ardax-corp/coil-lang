# VM opcodes (builtins-related)

User code does not name these directly; the compiler emits them:

| Opcode | Role |
|--------|------|
| `PRINT` | Write string to output |
| `FORMAT` | Build formatted string from specifiers |
| `FfiLoad` | `dload` |
| `DeclareFFI` | `declare` |
| `FfiInvoke` | `invoke` |
| `HostInvoke` | Host-registered closure. Operand `[15:0]` is arg count; bits `[17:16]` are the Option/Result host-edge layout (`0` boxed `ObjEnum`, `1` Option pointer-niche / `Result<(), E>` with heap `E`, `2` heap-heap Result). Stack is `fn_id, arg0, …, argN-1` (no args tuple). Natives construct that layout once via `machine::host_enum`. Standard table in `machine/src/host_natives.rs`. **119** = `stream_attach`, **120** = `stream_park`, **121** = `clock_wall_nanos`, **122** = `clock_mono_nanos`, **123** = `clock_sleep_ms`, **124** = `result_unit_probe`. Archive **major 4 / minor 5** appends M1 `prelude::math` **125–135**: `math_atan`, `math_atan2`, `math_asin`, `math_acos`, `math_log10`, `math_log2`, `math_cbrt`, `math_rem`, `math_sinh`, `math_cosh`, `math_tanh`. Frozen `math_sin` … `math_pow` stay **102–110**. Archive **major 4 / minor 7** appends compiler-only `simd_axpy_reduce` (**136**) for MIR saxpy-reduce packs. `PI` / `E` / `TAU` are coil-stdlib `num` constants, not natives. Leftover `time_*` slots stay panic stubs. See [io-reactor.md](io-reactor.md). |
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
| `CALL` (bit 31) | Two-slot return width. Bit 31 set (`Byte::CALL_RET2_BIT`) means the callee leaves two words instead of one boxed word; arity moves to `[30:24]` (0..=127) to make room. Enums use `[payload, tag]`; arity-2 immediate products use `[a, b]` (second on top). Clear (old archives, or any `with_call_packed` caller) means one word — no archive bump. See [limitations.md](limitations.md) two-slot direct CALL/RETURN. |
| `RETURN` (operand) | `0` (default; old archives) is one word. `2` pops/pushes two words (second on top) instead of one value. |
| `DenseBin` / `DenseCmp` | MIR 3-address numeric op / compare. Operand `[31:24] kind`, `[23:16] dest`, `[15:8] lhs`, `[7:0] rhs`. Stack-neutral; writes a typed frame slot. Archive **minor 6**. |
| `DenseConst` | Typed slot const. Bit 31 = pool; `[30:24]` ty, `[23:16]` dest, `[15:0]` imm or pool index. |
| `DenseMove` | Copy slot to slot (`[15:8]` dest, `[7:0]` src). Used for φ edge copies. |
| `DenseUnary` / `DenseCast` | `[31:24]` kind, `[15:8]` dest, `[7:0]` src (`neg` / `fneg` / `not`; `i2f` / `sext`). |
| `VLoad` / `VStore` | Compiler-only 8-lane numeric heap access (COI-310). `[31:24]` ty (`TY_I64` / `TY_F64`), `[23:16]` vreg, `[15:8]` array slot, `[7:0]` index slot. No heap refs in vregs. Archive **minor 8**. |
| `VBin` / `VMove` | 8-lane zip / splat / iota / neg via `coil-simd` (`VBin` packing matches `DenseBin`; splat `a` is a frame slot). `VMove` copies vregs. HostInvoke packs (P12) stay. |
| `VReduce` / `VFma` | S5b V1 (archive **minor 9**). `VReduce`: `[31:24]` ty, `[23:16]` dest slot, `[15:8]` vsrc — `dest = fold_left_add(dest, lanes)` (float sequential). `VFma`: dest/a/b vregs — `v[dest] = v[a]*v[b] + v[dest]` mul-then-add (no hardware contract). |

---

## Related

- [pipeline](pipeline.md)
- [array-pin.md](array-pin.md)
- [limitations.md](limitations.md) — COI-92 niches / two-slot `CALL`/`RETURN`
- [heap-identity.md](heap-identity.md) — slab + poison for `find_object_by_addr`
- [debug-info](debug-info.md)

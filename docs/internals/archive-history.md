# Archive version history

The current archive version is `ARCHIVE_MAJOR.ARCHIVE_MINOR` in
[`common/src/archive.rs`](../../common/src/archive.rs), which also carries the
entries from minor 24 on. That file is the ledger: bump the minor there (and
note what it adds) for an append-only change; an incompatible change bumps
the major and resets the minor. This page keeps the older notes that used to
live in AGENTS.md and [pipeline.md](pipeline.md) (#1874).

## Major 4, minors 5 to 37

- Minor 37 adds FFI type tag 16 (`ffi::types::ConstBytes`, a `string` or `Vec<byte>` passed as a read-only `const uint8_t *`; strings are lent in place).
- Minor 36 adds FFI type tag 15 (`ffi::types::Bytes`, a `Vec<byte>` passed as `uint8_t *`) and appends `ffi_read_ints` (**160**, `ffi::read_ints(lib, ptr, count)`).
- Minor 35 appends `contract_fail` (**159**, `HostOp::Task`: a failed `requires` panics naming the caller; see `docs/internals/contracts.md`).
- Minor 34 appends the wait-condition natives `task_cond_new` / `task_cond_wait` / `task_cond_notify` (**156–158**, `HostOp::Task`, behind `task::channel`).
- Minor 33 persists `cleanup_ranges` (per-function `defer` cleanup pads the VM unwinder runs on a panic or a task cancel; older archives load with none) and appends `unwind_resume` (**152**, `HostOp::Unwind`) and `task_cancel` / `task_shield_enter` / `task_shield_exit` (**153–155**, `HostOp::Task`).
- Minor 32 appends the task scheduler natives `task_scope_open` … `task_yield` (**144–151**, `HostOp::Task`, run by the VM; see `docs/internals/tasks.md`).
- Minor 31 appends byte-offset `string` natives (**139–143**).
- Minor 30 persists `debug_lines` (line/column per debug loc, resolved at compile time, so packaged binaries never read their sources; older archives fall back to the source files).
- Minor 29 persists `static_word_kinds` (per static slot word kind: scalar statics are no roots, pointer statics are precise roots evacuation rewrites).
- Minor 28 appends `MakeArrayK` (array literal with the pointer element kind).
- Minor 27 appends `TagArrayKind` (array element word kind stamped on an existing array; no longer emitted — `Vec::{new,with_capacity}$ptr` / `$bytes` thunks make the array with `MakeArrayK`, `Vec::from` keeps its source's kind — but the VM still runs it).
- Minor 26 flags precise frame-map slots that definitely hold a pointer (`PRECISE_SLOT_MUST`, bit 15; strip with `precise_slot_index`).
- Minor 25 appends `MakeEnumK` / `MakeEnumReturnK` / `MakeTupleK` / `DenseMakeK` (construction-site payload / element word kinds, 2 bits × first four words).
- Minor 24 persists `ClassWordKinds` (per-class field word kinds: scalar / pointer / unknown; marking skips scalar fields, pointer fields are precise references).
- Minor 23 appends `TagEnumType` (enum `fn drop()`, COI-26).
- Minor 22 adds `PreciseFrameMap::frame_words` (frame extent for allocating dense bodies) and `DenseCast` kind `CAST_F2I`.
- Minor 21 persists precise frame maps (`PreciseFrameMap`; heap-free frames skip the conservative stack scan).
- Minor 20 appends HostInvoke `stream_fd` (**138**) for `Stream.fd()`.
- Minor 19 appends `MakeEnumReturn` (COI-388 X3).
- Minor 18 appends `DenseIndexJmpf` (COI-379 S4).
- Minor 17 appends `DenseBinJmpf` (COI-377 S1).
- Minor 16 appends `DenseBin2` (COI-381 S2).
- Minor 15 appends HostInvoke `thread_spawn_shared` (**137**, COI-365 E6).
- Minor 14 persists S2b stack maps (COI-359 E1).
- Minor 13 persists `operand_stack_slots` (COI-358 E0).
- Minor 12 appends `DenseFieldLoad` / `DenseFieldStore` / `DenseMakeObject` (COI-356 D2).
- Minor 11 appends `DenseArrayPush` (COI-344 B6).
- Minor 10 appends dense-native heap ops (`DenseIndex` / `DenseStoreIndex` / `DenseArrayLen` / `DenseMake` / `DensePush`).
- Minor 9 appends compiler-only `VReduce` / `VFma` (S5b V1).
- Minor 8 appends compiler-only SIMD opcodes (`VLoad` / `VStore` / `VBin` / `VMove`; eight numeric lanes via `coil-simd`).
- Minor 7 appends HostInvoke `simd_axpy_reduce` (**136**) for MIR saxpy-reduce packs.
- Minor 6 appends MIR dense numeric opcodes (`DenseBin` … `DenseCast`).
- Minor 5 appends M1 `prelude::math` after `result_unit_probe` (**124**): `math_atan` / `atan2` / `asin` / `acos` / `log10` / `log2` / `cbrt` / `rem` / `sinh` / `cosh` / `tanh` are **125–135**. Frozen `math_sin` … `math_pow` stay **102–110**. `PI` / `E` / `TAU` live in coil-stdlib `num`, not here.

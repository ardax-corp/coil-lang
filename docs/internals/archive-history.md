# Archive version history

The current archive version is `ARCHIVE_MAJOR.ARCHIVE_MINOR` in
[`common/src/archive.rs`](../../common/src/archive.rs). Bump the minor there for an
additive change; any other change to the format bumps the major and resets the
minor. A runtime loads an archive of its own major and a minor it knows, and no
other: there are no legacy decoders. The format of the MIR section has a version of
its own, `FORMAT` in `compiler/src/mir/archive.rs`.

## Major 8

Major 8 minor 0 drops what no engine reads or makes any more. It is the one
format break of the 2026-11 cleanup: feature/native is not merged to `main`
(#2265), so no 8.x archive has shipped, and later PRs of the same wave that
change the format again stay on major 8 minor 0 and list their change here.
A runtime refuses a major 7 archive with its version error, as any other
major.

- MIR section `FORMAT` 14: a body no longer writes `deopt_sites` (always
  empty since `Deopt` went, #2343) or `call_fields` / `slot_fields`
  (unread since `promote_fields` went). Instruction tags 41 / 42
  (`PanicStore` / `Deopt`) stay reserved.
- Heap object kind 8 (a boxed scalar, `ObjBoxed`) and snapshot object
  kind 6 (`SnapObject::Boxed`) are gone; no engine allocated one. Both
  numbers stay free, as heap kind 11 is.
- Host native `values_eq_shaped` (**161**) is a panic stub: the compiler
  stopped emitting it in #2330 (`==` on such a type calls compiler-made
  glue). Its id is kept.
- A tuple is an enum object of `PRODUCT_TAG` (`u32::MAX`): heap kind 5,
  `ALLOC_TUPLE` (1), MIR allocation kind 1, snapshot object kind 3
  (`SnapObject::Tuple`) and snapshot word tag 3 (`SnapWord::EmptyTuple`,
  now `UnitEnum` of `PRODUCT_TAG`) are free. An `Alloc`'s words are in
  declaration order for every kind; engines no longer reverse an enum's.
  MIR section `FORMAT` 15.
- `HeapFieldLoad` is `ProductLoad` (same tag, `I_FIELD`; text `productload`):
  slot `k` of any product object, a constant-index tuple read among them.
  Helpers `coil_native_field` (which read `0` from a wrong object) and
  `coil_native_enum_payload` are gone; `coil_native_product_load` bails.
  MIR section `FORMAT` 16.
- Host native `packed_vec_arith` (**91**): meta bit 28 says the elements
  are `byte`, and the kernel wraps each result modulo 256 (#2401). An
  archive made before this has the bit clear and keeps the int results.
- Byte cells in the linear-algebra kernels (#2403): `packed_dot` (**87**)
  meta bit 17 and `packed_matmul` (**88**) meta bit 27 say `byte`, and the
  result wraps modulo 256. The byte bits `packed_matrix_zip` and
  `packed_matrix_neg` already had (29 and 20) now wrap `+`, `-` and
  arithmetic negation too, not only the bitwise cells.
- A `Cow` takes and gives a heap word again: the `I64` share #2567 added
  (a shared generic body's type parameter word, which may be no object) is
  gone; the body tests its `kinds` word and shares the object instead
  (#2506). The op's encoding never changed: MIR section `FORMAT` stays 16.
- A shared generic free function or inherent method takes one more entry
  word after its dictionaries, its `kinds` mask (#2506, [cow-calls.md](cow-calls.md)):
  every call, a coroutine's `MakeCoro`, a partial application's filled
  words and a `PolyFn` value's captured words pass it. The calling
  convention changed, not the encoding: no new op, MIR section `FORMAT`
  stays 16.
- A trait dictionary holds a `kinds` word after its methods:
  `(methods.., kinds, context.., superclass dictionaries..)` (#2506). A
  trait method with type parameters of its own takes their `kinds` word
  after its own bounds' dictionaries. Layout and convention only: `FORMAT`
  stays 16.
- A call through a `PolyFn` passes, after its dictionaries, one bit per
  type variable of the type it sees, in order of first appearance, and
  their count at bit 56 (`common::KINDS_CALL_COUNT_SHIFT`). A
  `MakePolyFnCapture` word after the `kinds` word with bit 62 set and bit
  63 clear (`common::kinds_from_call`) sets one bit of that `kinds` word
  from the call's (`machine::apply`) and is no argument (#2506, #2621; it
  replaces #2602's word `1`, `KINDS_FROM_CALL`). A shared generic entry
  takes at most 63 type parameters, so its `kinds` word has bit 63 set.
  `FORMAT` stays 16.
- A call through a `PolyFn` passes its constraints' dictionaries in the
  order of the type it sees (`kinds::value_constraint_order`), and a
  `MakePolyFnCapture` dictionary word with bits 61 and 62 set, bit 63 clear
  (`common::dict_from_call(j)`) is the call's dictionary `j` (#2636). A `0`
  word still takes the call's dictionary at its own index. `FORMAT` stays
  16.

## Major 7

Major 7 minor 0 came with the deletion of the bytecode VM (step 8 of
[mir-interp.md](mir-interp.md)). An archive holds MIR and data only:

- `version`, `static_slot_count`, `constants` (the snapshots startup restores) and
  `strings`.
- The debug tables: `source_files`, `debug_locs`, `debug_lines`, `fn_symbols`.
- `struct_layouts`, `class_word_kinds`, `static_word_kinds`.
- `startup` (a list of `common::StartupOp`) and `main`.
- `native_mir`, every body's typed MIR.

Bodies are named by `label.0 + 1`, not by bytecode entry PC. The bytecode, line and
span run tables, frame maps, stack maps, `cleanup_ranges` and
`operand_stack_slots` went, and with them every reader of an older archive.

MIR section `FORMAT` 11 (feature/native) adds `CallIndirect`'s two-word
form (opcode 44: second word and `unpair` body) and each body's
`pair_twin`; the code image `FORMAT` 2 adds `NativeEntry::twin`.

Minor 1 adds class slot kind `WORD_TAGGED` (4): the first word of a flat
two-word field whose payload is a pointer only under some tags. A row with
one has a trailer after its slot kinds, an entry per tagged slot (tag slot
`u16` LE, pointer-tag mask `u64` LE), and the collector resolves the word
by its tag (`common::class_slot_kinds`).

Minor 2 adds snapshot object kind 7, a lane array (`SnapObject::Lanes`): an
array of two-word elements kept in place (`k`, the pointer-tag mask of the
first word, then the words), as a `Vec` of a closed two-word type is
([layout.md](layout.md), "Lane arrays"). MIR section `FORMAT` 13 adds
`LaneIndex` / `LaneStore` / `LanePush` / `LanesToWords` (opcodes 45 to 48,
each store, push and conversion with its three shape masks) and the
`Lanes` allocation kind (9, with its mask).

Minor 3 appends host natives `vec_drain` (**212**: drop elements
`from..to`) and `vec_rotate_in` (**213**: move the last element to `i`),
compiler lowering only, in any array form. Lane code's `pop`, `remove`
and `insert` move a lane array's elements with them.

Minor 4 adds host native `startup_restore` (**214**, `HostOp::Restore`), which an AOT
executable's native `__start` calls where its startup restores a snapshot. Its
payload is data only: `startup` holds no `Init` or `Call`, and `main` names `__start`
(`docs/internals/native.md`, "Native start").

Majors 4 to 6 ran on the VM. The notes below are the record of what minors of
major 4 added; opcodes and frame maps in them no longer exist.

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
- Minor 27 appends `TagArrayKind` (array element word kind stamped on an existing array).
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

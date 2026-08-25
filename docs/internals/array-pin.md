# Array pins

[#192](https://github.com/ardax-corp/coil-lang/pull/192) already shipped the
handle [COI-198](https://linear.app/ardax/issue/COI-198) asked for. Proven
counted / stride loops pin the array once and index through a cached `Object`,
so those sites skip `Heap::find_object_by_addr`. This note is the layout. It is
not a second pin opcode.

This is not [COI-99](https://linear.app/ardax/issue/COI-99) (which helpers may
appear in a length-invariant loop). Named-local class SROA stays a non-goal
([COI-84](https://linear.app/ardax/issue/COI-84)). No JIT.

## Already on main (archive minor 13)

`ARCHIVE_MINOR` is 13. Minor 12 added `IndexUnchecked` / `StoreIndexUnchecked`.
Minor 13 appended, at the end of `Instruction`:

| Opcode | Stack | Operand |
|--------|-------|---------|
| `ArrayPin` | pop array (after `LOAD arr`) | frame slot used as pin-table key |
| `IndexPin` | pop index, push element | pin slot |
| `IndexPinUnchecked` | same, bounds-proofed | pin slot |
| `StoreIndexPin` | pop value, pop index, push value | pin slot |
| `StoreIndexPinUnchecked` | same, bounds-proofed | pin slot |

Last variant is `StoreIndexPinUnchecked`. The release `promise!` ceiling in
`machine/src/vm.rs` and `instruction_from_u8_covers_last_appended_variant`
track that. Definitions: `common/src/opcode.rs`, `common/src/archive.rs`.

Do not append another pin / ArrayPtr opcode. The product is these five.

### Handle shape

Not a `Value` fat pointer, not a generation stamp, not a nursery card.

`Machine` keeps `frame_pins: ArrayVec<HashMap<u32, Object>, S>` in lockstep
with `frames` (`machine/src/vm.rs`). Each call frame owns one map. Keys are
the array's **local slot** (the `ArrayPin` operand). Values are `Object`
(`Copy` `Gc<ObjArray>` — a `NonNull<GcData<ObjArray>>` in
`machine/src/memory/heap.rs`).

`ArrayPin` does the one `find_object_by_addr` for that slot: pop the stack-top
address, and if it is `Object::Array`, insert that `Gc` under the operand
slot. Non-array targets are a silent no-op (later `IndexPin*` yield `-1`).
Tuples are never inserted; the `IndexPin` tuple arm is only reachable from
hand-written bytecode.

`IndexPin*` / `StoreIndexPin*` read or write `gc.as_ref().elements` /
`gc.as_mut().elements` from that cached `Object`. They do not hash the array
address. They still go through the `Gc` each dispatch — they do not cache an
interior `*mut Value` into `elements`.

### Who creates it

`il::bounds::rewrite_array_pins` (`compiler/src/il/bounds.rs`), after
`rewrite_proven_index_ops`. Driven from LICM's `loop_bounds` call.

For each counted loop (`LE` / post-canon `GT` header; unit `+1` or invariant
positive stride) and each length-invariant array slot in `len_arrays`:

1. Proven `LOAD arr; LOAD i; Index` / `IndexUnchecked` rewrite to
   `IndexPin` / `IndexPinUnchecked` and drop the array load.
2. Proven `LOAD arr; LOAD i; <value>; StoreIndex*` rewrite to
   `StoreIndexPin*`.
3. `LOAD arr; ArrayPin slot=arr` is inserted in a fresh preheader.

The compiler therefore emits `IndexPinUnchecked` / `StoreIndexPinUnchecked`
on proven sites (`rewrite_proven_index_ops` runs first). Checked `IndexPin` /
`StoreIndexPin` exist for the VM contract and tests; production rewrite of
nsieve uses the unchecked twins
(`compiler/tests/perf_metrics.rs`, `compiler/src/pipeline.rs`).

The same length-sensitive refusals as Unchecked apply: `ArrayPush`, rebound
array slot, impure `CALL`, host, FFI, `GetField`/`SetField`, `CallIndirect`,
`TailCall`, `YieldCoro` / `YieldFromCoro`, `FORMAT`, `MakeArray`. Pure user
helpers are not a barrier ([COI-99](https://linear.app/ardax/issue/COI-99)).
`LEQ` / `GEQ` headers are not in-bounds proofs
([COI-85](https://linear.app/ardax/issue/COI-85) /
[COI-98](https://linear.app/ardax/issue/COI-98)).

### Who consumes it

Only the four `*Pin*` index/store opcodes. `Index` / `IndexUnchecked` /
`StoreIndex` / `StoreIndexUnchecked` still call `Heap::find_object_by_addr`.
So do `ArrayLen`, `ArrayPush`, `GetField`, `SetField`, and the `Vec` host
natives in `machine/src/vec_ops.rs`.

### Invalidation

There is no generation or pin-token opcode. A pin dies when:

- the frame pops (`RETURN`, `pop_call_frame`, yield unwind);
- `ArrayPin` overwrites the same slot;
- coroutine resume pushes a **fresh empty** map for each restored frame
  (`push_pin_frame` after yield — see yield barrier below).

`CALL` / `CallIndirect` / `call_function` push a new empty map for the
callee; the caller's map stays on the previous frame and is still a GC root.
`TailCall` reuses the current frame, so leftover pins remain until that
frame returns. `TailCall` is already a length-proof barrier, so a proven
loop body does not tail-call.

## GC

The heap is **non-moving mark-and-sweep**. `Heap::alloc` leaks a `Box<GcData<T>>`
onto an intrusive list and records it in `addr_index`. `sweep` unlinks unmarked
cells and drops them; it does not relocate `GcData`. Addresses stored in
`Value` and `Gc` pointers stay valid for the object's lifetime
(`machine/src/memory/heap.rs`).

`collect_vm_root_addrs` walks every `frame_pins` map and pushes `obj.addr()`
next to the operand stack, statics, and coroutine saved stacks. Automatic GC
and `gc::collect` share `gc_collect` → `mark_from_vm_roots`. A live pin keeps
the array marked, so sweep will not free it, and `fn drop()` on an element
does not run while the pin (or any other root) holds the array.

Because objects do not move, the cached `Object` does not need a write barrier
or a post-GC fixup. `IndexPin` re-dereferences `Gc` each time, so an in-place
`elements` realloc from `ArrayPush` would be visible — and `ArrayPush` is
refused in pinned loops anyway.

### Finalizers and weaks

Frame pins are **not** `gc::Root` / `gc::Weak` (`machine/src/gc_handles.rs`).
Those are heap objects the user allocates. Frame pins are VM-internal strong
roots.

- `gc::Root` marks its payload while the `Root` object is reachable.
- `gc::Weak` does not keep the referent alive; upgrade fails after sweep.
- A frame pin keeps the array alive until the map entry is gone, even if the
  local slot is later overwritten. That is extra liveness, not a use-after-free.
- Resurrection from `fn drop()` ([COI-79](https://linear.app/ardax/issue/COI-79))
  is unchanged: drop still runs at most once; a pin that kept the object marked
  simply delays drop until the frame unwinds and a later collection sees it
  unmarked.

No hole found where GC frees a pinned `Gc` while `IndexPin*` still holds it.

## What still pays `find_object_by_addr`

`addr_index` is `HashMap<u64, Object, AddrHashBuilder>` — an O(1) fmix64 probe,
not a list walk (`machine/src/memory/addr_hash.rs`). Unpinned `Index` still
pays that probe plus the in-VM range test.

| Site | Why it still hashes |
|------|---------------------|
| Unproven / dynamic `Index` / `StoreIndex` | `rewrite_array_pins` requires `index_at_proven` / `store_index_at_proven` (induction index + length-invariant array slot) |
| `Index` outside a counted loop | No preheader pin |
| `LEQ` / `GEQ` headers, growing arrays, impure calls, host, FFI, yield | Length proof refuses; Unchecked and pin both stay off |
| `ArrayLen`, `ArrayPush`, `Vec` host natives | Different opcodes; not rewritten |
| Tuple `Index` | `ArrayPin` only inserts `Object::Array` |
| `examples/perf/binary_trees.hy` | Recursive `Tree` alloc + `match`, not array `Index` |

[#192](https://github.com/ardax-corp/coil-lang/pull/192) reported nsieve checked
`Index` → 0 with dispatch count unchanged and poop wall / cycle deltas within
noise; leftover on those sites was the address hash. Minor 13 pins those
sites (`IndexPinUnchecked` / `StoreIndexPinUnchecked` / `ArrayPin` in nsieve
bytecode). This note does not claim a cycle win for pins beyond that opcode
swap — re-run `./scripts/poop_baseline.sh` if a number is needed.

## What a future ArrayPtr would add

A new opcode or a `Value`-level fat pointer would duplicate `ArrayPin`:
resolve once, cache a `Gc`, index without `addr_index`. Extending `IndexPin*`
coverage (checked `IndexPin` on length-invariant but unproven indices, or
pinning `ArrayLen`) is a **compiler rewrite**, not a new handle.

A stack-carried ArrayPtr would only matter if pins had to survive yield,
cross `TailCall`, or be passed into host natives. Those are not the nsieve
path, and they collide with `Value` being an address-or-immediate today.

A generation / moving-GC stamp is unused while sweep is non-moving.

## Yield is a length-proof barrier

`YieldCoro` / `YieldFromCoro` are length-proof barriers
(`op_blocks_length_proof` in `compiler/src/il/pure_call.rs`), same family as
`TailCall`. Resume pushes a **fresh empty** pin map; a pin emitted in a
yielding counted loop would miss after the first suspend (`IndexPin*` → `-1`).
Fail closed: that loop keeps checked `Index` and does not get `ArrayPin` /
`IndexPin*` / `IndexUnchecked`. Pins are not persisted on `ObjCoroutine`.

## Refusals

| Out of scope | Why |
|--------------|-----|
| Second pin / ArrayPtr opcode | Minor 13 already is that handle |
| Named-local class SROA / stack instances | [COI-84](https://linear.app/ardax/issue/COI-84); frame pins are array identity, not scalar replacement |
| JIT | Interpreter compatibility path; see [optimization-roadmap.md](optimization-roadmap.md) |
| Pinning every `Index` in the VM | Unproven / aliased / host paths stay hashed; fail closed |
| Interior pointers into `elements` | `ArrayPush` reallocates the `Vec`; pin holds `Gc`, not a slice |
| Moving GC / nursery | Not the collector |

## Decision

**Defer.** Pins are the product. Close
[COI-198](https://linear.app/ardax/issue/COI-198) after this note lands. Do
not file a Feature for ArrayPtr.

Leave unpinned hashing as an ordinary leftover under the existing bounds
refusals. Yield is a barrier, not a new handle type.

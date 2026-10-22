# Shared-heap epoch write guard

[#1892](https://github.com/ardax-corp/coil-lang/issues/1892). Answers Q2
("runtime freeze bit") of [shared-heap-sendability.md](shared-heap-sendability.md)
for debug and `gc-stress` builds. Release builds are unchanged.

## Why

Inside a Layer A steal epoch several reactor threads run Coil bytecode
against one [`Heap`](../../machine/src/memory/heap.rs)
([`SharedHeapEpoch`](../../machine/src/shared_heap.rs) is `Send + Sync` by
`unsafe impl`). Object headers are `Cell`s, `ObjArray::may_hold_refs` is a
plain byte store, and payload writes go through `&mut T` taken from a shared
`Gc<T>`. Nothing at run time stops two workers from writing the same object:
soundness is the compiler's purity and independence proof
([auto-par.md](auto-par.md)) plus the spawn whitelist (`is_c1_shareable`).
A wrong proof, or a pass that widens admission (`Summary::param_only`,
#1648), is a silent data race. The guard turns that into an abort in every
debug build and in the `gc-stress` CI harness.

## Invariant

While a thread runs a shared-heap job (`thread_spawn_shared`, HostInvoke
**137**: a loop-IPA chunk or an expression-IPA arm), every heap object it
**writes** was allocated in the **same epoch**. Objects from before the epoch
(the spawner's arrays, interned strings, immortal unit enums and `()`,
statics' targets, `Sender` / `Mutex` wrappers) are read-only to it.

This is the C0 contract as it is used today. C0 also allows a *disjoint
write* into a pre-existing array (`out[i]` over partitioned ranges), but no
compiler pass emits that: loop IPA refuses index, field and static stores in
the body (`loop_par` purity gate), expression-IPA arms take only `int`
arguments, and a worker's writes land in objects it built itself (a
`Vec::new()` it returns, a `__dict$N` / `__table$N` it fills into its own
copy of the statics, an enum or tuple it constructs). When a disjoint-write
admission lands, the guard grows a granted range (see *Later*), it does not
get dropped.

The guard does **not** separate two workers of one epoch from each other: an
object one worker allocated is writable by another worker of the same epoch
if it ever reaches it. Today the only way across is the join, which publishes
the result after the producer has finished.

## Mutation paths

Every payload write in the VM goes through
`impl AsMut<T> for GcData<T>` (`Gc::as_mut` through `DerefMut`, and
`Gc::payload_mut`), so one check there covers all of them. The paths a
worker can reach (line numbers at the commit that adds this note):

| Path | Where |
|---|---|
| `StoreIndex` / `StoreIndexUnchecked` | `machine/src/exec_rest.rs:969` → `ObjArray::store_indexed` (`heap.rs`, sets `may_hold_refs` via `note()`) |
| `StoreIndexPin*` | `exec_rest.rs:991` |
| `DenseStoreIndex` | `machine/src/dispatch.rs:550` (`dense_store_index`) |
| `VStore` (SIMD lanes) | `machine/src/vm.rs:1459` (`vstore`, `set_numeric_unchecked`) |
| `ArrayPush` / `DenseArrayPush` | `exec_rest.rs:1126` / `:1098` → `vm.rs:2308` (`array_push_value`: `push`, then `account_array`) |
| `SetField` (slot and named) | `exec_rest.rs:933` (`set_slot`, `ObjInstance::set`) |
| `DenseFieldStore` | `dispatch.rs:662` |
| `MakeDict` / `INIT` / `InitTyped` / `DenseMakeObject` | `exec_rest.rs:882` / `:349` / `:356` / `:1113` (fresh object) |
| `TagEnumType` / `TagArrayKind` | `exec_rest.rs:1521` / `:1530` (the enum path skips unit enums before writing) |
| `Vec` natives (`reserve`, `clear`, `pop`, `insert`, `remove`) | `machine/src/vec_ops.rs` |
| Finalizer claim (`fn drop`) | `vm.rs:2258` (`claim_finalizer`) |
| Coroutine state | `vm.rs:2826` (`with_coroutine_mut`) |
| Stream / FFI / library writes | `machine/src/io.rs`, `machine/src/ffi/call.rs` (impure: never in a worker) |

Heap-structure state is not a payload write and is outside the guard; it is
already serialized or atomic:

| State | How it is kept safe in an epoch |
|---|---|
| Intern table (`intern`, `intern_str`, `intern_ref`) | epoch lock (`Heap::epoch_guard`, `heap.rs:591-612`) |
| Immortal unit enums / `()` | epoch lock (`immortal_unit_enum`, `immortal_empty_tuple`) |
| `alloc_bytes` / `live_count` (`account_resize`, batch fold) | epoch lock; batch counts fold under it (`alloc_from_epoch_batch`, `heap.rs:415`) |
| Slab directory, `find_object_by_addr` | atomics (`slab.rs`) |
| `ObjString` hash | `AtomicU64`, relaxed |
| String append in place (`alloc_concat`) | atomic claim of the shared buffer's tail (`strbuf.rs` `try_append_at`) |
| Header `marked` / `fresh` bits | written only by the collector, and collect is forbidden in an epoch (`vm.rs:1622`, `gc_safepoint` at `vm.rs:1785` aborts it instead) |
| `ObjWeak` `Cell`s | written only by the collector / `gc::upgrade` paths; weaks are not whitelisted |

## The guard

Behind `cfg(any(debug_assertions, feature = "gc-stress"))` only:

1. **Header stamp.** `GcHeader` gets an `epoch: Cell<u8>` after `fresh`.
   The header is `repr(C)` and every payload is 8-aligned, so the byte lands
   in what is padding otherwise; release builds do not have the field at all
   (no layout change there).
2. **Epoch generation.** `Heap::enter_epoch_stw` bumps `Heap::epoch_gen`
   (never 0). `alloc_from_epoch_batch` stamps it on every object a mutator
   allocates in the epoch. Everything else (`alloc_unlocked`, interned and
   immortal objects, objects from before the epoch) keeps 0 or an older
   generation.
3. **Job scope.** `EpochWriteScope` sets a thread-local generation for the
   length of one reactor job: `run_job_on_vm` enters it with the bound
   heap's `epoch_gen` for a shared-heap job and 0 for an isolate job;
   `run_help_job` sets 0 around a help-stolen job (it may belong to another
   heap's epoch, and its helper VM resets its own heap afterwards). Dropping
   the scope restores the outer job's value, so a join that help-steals
   inside a shared job is covered.
4. **Check.** `GcData::as_mut` compares the thread-local generation with the
   object's stamp when the former is non-zero. A mismatch prints the address
   and both generations and **aborts the process**. A panic would be caught
   by the job's `catch_unwind`, turned into a failed join, and the arm would
   rerun sequentially with the right answer: the bug would stay invisible.
   (The machine crate's own unit tests panic instead, so a test can expect
   it.)

The root thread is never inside a scope: it is the single mutator before and
after the epoch and runs its local arms on objects it owns.

### Cost

- Debug / `gc-stress`: one thread-local read and one compare per payload
  write, one byte store per epoch allocation, one byte in the header padding.
  No change to collection, marking or object size.
- Release: nothing. The field, the stamp, the thread-local and the check are
  compiled out; `EpochWriteScope` is a zero-sized type with no `Drop`.

It is off in release because the check sits on the hottest store paths
(`store_indexed`, `push`, `set_slot`) where release spends one or two
instructions, and the compiler proof is what release relies on; the guard's
job is to make CI fail when that proof is wrong.

### What it does not catch

- A write through a raw pointer or a `Cell` that bypasses `as_mut` (the
  collector's mark bits, `ObjWeak`): none of those run in a worker.
- Two workers writing the same **in-epoch** object (see *Invariant*).
- A write to an object stamped exactly 256 epochs earlier on the same heap
  (the `u8` generation wraps). Such an object is a false negative, never a
  false positive.
- Reads of an object another thread is writing. With no admitted write to a
  shared object there is no such read.

Under `gc-stress`, `should_collect` is always true, so an epoch aborts at its
first allocation safepoint and the job falls back to sequential. Allocation
free chunks (`loop_ipa_sum.hy`, `mandelbrot_ipa.hy`, fib-shaped arms) still
run as shared jobs under the guard; the debug-build test suite
(`cargo test`, `coil test`) covers the allocating ones.

## Later

- **Disjoint write admission.** When loop IPA admits `out[i] = …` into a
  spawner's array, the spawn records `(array, lo, hi)` grants for the job;
  the check becomes "stamp matches, or the object is granted and the index
  is in the job's range", which moves the check from `as_mut` into
  `store_indexed` for granted arrays (push / resize stay refused: length is
  stable by contract).
- **Cross-worker writes.** Replace the generation with a per-job id in the
  stamp (the batch already knows its thread) if a pass ever publishes
  in-epoch objects between running workers.
- **Layer B.** A collect during an epoch would make header mark bits a
  shared write; that is Layer B's handshake, not this guard.

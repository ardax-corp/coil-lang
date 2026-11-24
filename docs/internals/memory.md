# Memory: reference counts and the cycle collector

Coil frees objects by reference count. The compiler inserts the count operations into MIR and
elides most of them; a cycle collector runs only for the objects that can close a cycle. Nothing
traces the heap and nothing moves. This replaced the tracing collector (mark, lazy sweep, nursery,
evacuation; the old notes were `gc-incremental.md` and `gc-evacuation.md`, in git history) on
`feature/native` in coil-lang#2889. The plan and its decisions are in the project files
(`gc-free/rc-plan.md`, `gc-free/dropping-the-gc.md`).

## The count

Every heap object starts with a `GcHeader` (`machine/src/memory/heap.rs`): kind, flags,
generation and a 32-bit `count`. The count is the number of places that own the object:

- heap words the runtime and native code store (fields, elements, captures, enum payloads, a
  coroutine's arguments before it starts, task records, FFI roots, statics);
- frames' words, which the compiler counts with `RcInc` / `RcDec` (`Heap::rc_retain` /
  `Heap::rc_drop` at run time).

`COUNT_IMMORTAL` (the saturated value) marks an object that never dies: interned strings, unit
variants, `()`, the comptime snapshot's objects and statics' constant data. Increments saturate
below it, so a count can stick but never wrap.

An object is freed when its count reaches zero (`machine/src/memory/rc.rs`): its children are
decremented in turn by the same rules (`rc_dec_children`, which reads each word's kind; a scalar
word or a leaf object is skipped), its weak handles clear, and its slot goes back to its size
class. A class with `fn drop()` or a suspended closeable generator is not freed on the spot: it goes
on `rc_due` and the next safepoint runs the `drop` (or the close) and then frees it.

## Who owns what: the insertion pass

`compiler/src/mir/rc_insert.rs` runs on MIR after the optimizer, so every engine (native, the MIR
interpreter, wasm) runs the same counted bodies. Its module doc is the reference; in short:

- each value that may hold an object is a *root* or an *alias* of one (a cast, the untagging of a
  niche word, a store's destination);
- a parameter is *borrowed*: the caller keeps it alive across the call;
- a call's result is *owned* (a body returns its result owned), a φ is owned, and any other root (an
  allocation, a load, a host call's result) is incremented where it is defined;
- an owned root is decremented after its last use, or on the edge where it dies, and on every way
  out including panics (unwind homes) and `break` / `continue`; a `return` moves it to the caller.
- a tail call leaves the caller nothing to release after it, so it enters a twin of its callee,
  `name$own` (`owning_tail_twins`), that owns its parameters: an argument that dies at the call
  moves in, one that lives on is incremented. The twin keeps no unwind homes, so the engines still
  reuse the frame. Coroutine bodies own their parameters the same way.

Elision: numbers are never counted (constants, arithmetic, φs of them, a word a number store
writes, scalar array elements), and an inc / dec pair of one word in a block cancels when nothing
between them can release an object. `hir/early_free.rs` frees whole fresh graphs at scope end
before MIR exists, so many objects never reach a count operation at all.

Host natives follow the same convention: they take borrowed arguments and return owned results.
Runtime stores (`set_field`, `Vec` / `HashMap` natives, the key index, string builders) increment
the word they store and decrement the word they overwrite.

## Why frames are no roots

Every word a frame uses is owned by a count, the frame's own or that of a holder that outlives the
use. So the cycle collector needs nothing from frames: an object a frame names already has an
owner outside the heap in its count. This is what let stack maps, root records, the native
`for_each_frame_root` walk and ambiguous-word pinning go. Native code still compares
`NativeCtx::gc_epoch` around calls that may run code and reloads array views when it moved
(`epoch_mark` / `epoch_check` in `coil-native`).

## Copy-on-write from the count

Value types (structs, `Vec`, `HashMap`, strings) copy on write. A share still sets the header's
`shared` flag, but the count ends its stickiness: a write copies only when the object is marked
shared *and* its count is above one (`Heap::cow_is_shared`). The count alone is not enough: a
`use (mut b)` capture holds a count without sharing a value. Pins (`CowPin`) stay a flag. See [cow-calls.md](cow-calls.md) and [view-classes.md](view-classes.md).

## Identity handles and finalizers

- `gc::Weak` is cleared when its target's last owner goes (the heap's weak list).
- `gc::Root` is a strong handle (one count) until `unroot`.
- `fn drop()` runs when the count reaches zero, or at the end of the owning scope; it runs once
  (`claim_finalizer`).
- `gc::collect()` runs the due drops and then the cycle collector.

## Coroutines and tasks

An unstarted coroutine owns its arguments. Once started, its saved words belong to its frame, so
`rc_dec_children` skips them. Never-started generators and tasks are freed by counts (task records
own a count; the scheduler's released list is drained at safepoints).

## Auto-par and threads

Shared-heap epoch workers borrow objects from before the parallel section and never write their
counts: the spawner's frame keeps them alive until the join. Objects born in the epoch count
normally, and their net count changes are applied at the end of the epoch (an object born there
whose count ends at zero is freed). Values sent to another thread are copies, as before.

## Cycles

A write to a shared value copies it, so only an identity object can close a cycle: a `shared` or
`drop` class whose type graph reaches back to itself, or one holding a function or a coroutine. The
compiler registers those classes (`Heap::register_cyclic_type`). `machine/src/memory/cycles.rs` is a
synchronous trial-deletion collector (Bacon and Rajan) that starts only from objects of those
classes: each object's count less the owners other heap objects account for is what owns it from
outside the heap; an object with such an owner, a runtime root (statics, handles, tasks, FFI) or
a kind counts cannot free is live, and so is everything it reaches. The trial counts live in a side
table, so the headers are never touched.

A generator suspended at a `yield` holds words on its own stack. Before each yield the body names
its frame's homes and a mask of the ones that own a count (`MirFunc::counted_homes`: the homes its
cleanup pad loads and releases) in `NativeCtx::gen_homes` / `gen_mask`; `coil_native_yield` hands
them to the resumer, which keeps them on the coroutine (`ObjCoroutine::frame`) until the next
resume or close. The collector reads those words as the generator's children. A garbage cycle
through such a generator is not freed directly: the generator is closed, as a `drop` is run,
which releases its frame's words, and the counts free the rest. A frame with a counted home past
the 64th names nothing, and its generator stays live with what it reaches.

It runs from a safepoint when the live bytes pass a threshold (`Heap::cycles_due`; the threshold
after a run is the live bytes times the growth factor, at least 1 MiB, at most the heap cap), and
on `gc::collect`. A program with no cyclic class pays nothing.

## The allocator

`machine/src/memory/slab.rs`: 64 KiB chunks per `(slot size, align)` class, free lists per class,
and a 4096-entry recent-chunk cache for address lookups (`find_object_by_addr`). Most objects are
bump-allocated in *young* chunks (instances, payload enums, tuples, arrays, closures); native code
allocates and frees there inline (`memory/inline.rs` describes the layout native code reads). A
freed young slot goes on its class's intrusive free list. `promote_young` (`memory/nursery.rs`)
moves the young chunks to the old space where nothing may allocate young: a steal epoch. The
name is historical: nothing is collected young.

Chunks stay mapped. Freed slots are reused, but empty chunks are not given back to the OS yet
(the tracing collector released idle ones after a sweep); RSS after a peak stays flat.

The `region` hooks (host ids 177/178) are no-ops kept so later ids do not move.

## Checks

- **rc-check build** (`coil-test` built with `--features rc-check`, CI's `harness rc-check` cell,
  `--cell rc-check`): the cycle collector runs at every safepoint, and before each run
  `Heap::rc_check` verifies that no pointer word names a freed slot and that each object's count is
  at least the number of heap words that own it. A failure names the object and its holders.
- **`COIL_RC_LEAKS=1`**: at exit the heap prints what is still allocated that no static reaches
  (`coil rc-leaks: N objects, B bytes (kinds)`); `tests/rc_leaks_cli.rs`.
- Debug builds and `COIL_MIR_VERIFY=1` check the count ops like any other MIR op.

## Known gaps

- A cycle through a suspended generator whose frame has a counted home past the 64th leaks.
- A frame that unwinds through a panic past code without unwind homes, and a coroutine abandoned
  before it finishes, can leave objects to the leak report.
- Empty chunks are not returned to the OS.

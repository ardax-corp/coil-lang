# Heap identity (mapped slab)

[COI-200](https://linear.app/ardax/issue/COI-200) asked whether `binary_trees`
is bound by `Heap::alloc` identity work: one `Box<GcData>` per object plus a
then-hot-path `live` HashSet probe in `find_object_by_addr`. That HashSet is
gone. This note is the layout. **Decision: implement slab + header poison**
(this crate, no bytecode change). It is not a second ArrayPtr, not a handle
table, and not a moving GC.

Pins remain the product for proven loops ([array-pin.md](array-pin.md),
[COI-198](https://linear.app/ardax/issue/COI-198)). Unproven `Index` /
`GetField` still go through `find_object_by_addr`; they must not hash.

## Model (non-moving mark-and-sweep)

`Value` stays a raw address. Archive major / opcodes / `Object::from_header`
kind tags (1..=18) are unchanged.

Allocate `GcData<T>` headers from a **mapped slab** (size-class free lists;
64KiB anonymous chunks). Sweep **poisons** `GcHeader.kind = 0` and returns
the slot to the free list; chunks stay mapped. **Idle-chunk release:** a
chunk whose slots sat unused for a whole `RELEASE_WINDOW` (8 sweep cycles,
tracked as each size class's free-list low-water mark) gets its pages back
to the OS with `madvise(MADV_DONTNEED)` and is re-carved before any new chunk
is mapped. It stays mapped — a released page reads as zeros, so every header
in it is poisoned and stale / conservative lookups stay defined. Steady
churn drains its free list each cycle and never releases (no refaults).
Payload `Vec`s (array
elements, interned string bytes) stay ordinary Rust allocs in this cut.
Typed class instances use dense slots
([#287](https://github.com/ardax-corp/coil-lang/pull/287)); small `ObjEnum`
payloads can inline ([#290](https://github.com/ardax-corp/coil-lang/pull/290));
typed instances with ≤4 fields keep those slots in the header
([#299](https://github.com/ardax-corp/coil-lang/pull/299)). Typed slots are
raw `Value` words, not tagged `Member`s: stores skip the heap probe, and
mark / root census resolve each word through the slab like tuple elements.
Enum payloads and tuples are raw words the same way, with up to four inline
before a spill `Vec`. Class fields carry compile-time word kinds (archive
minor 24, `ClassWordKinds` per `type_id`): a scalar field (`int` / `float` /
`bool` / `byte` / scalar enum) is never traced; a pointer field (ground heap
type or niche word) is a precise reference; a generic or unresolved field
stays ambiguous. Enum payloads and tuples carry construction-site kinds
(archive minor 25: `MakeEnumK` / `MakeEnumReturnK` / `MakeTupleK` /
`DenseMakeK`, 2 bits for each of the first four words, kept in the payload's
padding). Codegen classifies constructor arguments by static type and MIR by
`MirTy`. Generic shared bodies and host-built enums stay unknown.
`gc-stress` builds check every declared kind against the slab while
marking.

Frame roots follow the same split (archive minor 26). A precise frame map
lists the slots that may hold heap words; the compiler flags
(`PRECISE_SLOT_MUST`) those whose every reaching definition is an
allocation, a literal `0`, or a copy of one. Parameters and one-word
`CALL` results take their kind from the checked signature (walked for
exactly the entry height; bodies whose scheme lacks dictionary params, and
generic returns, stay unknown). Fork workers (`__coil_par_f`) reuse `f`'s.
The analysis tracks the kinds of the top operands relative to the cursor,
so a push / `STORE` pair keeps its kind at loop heads where the cursor is
only known as a range. `MakeEnumReturn(K)` records its allocation. The VM
reports flagged slots as precise roots and the rest as ambiguous. What
stays ambiguous: slots a loop reads only after rewriting (stale values
from the previous iteration, unwritten on entry; a liveness pass could
drop them), generic call results, and `Vec<T>` elements (no kinds), so
objects held that way would still pin under a moving collector. Named `Table` instances (dicts, `INIT`) still hold `Member`s.
Do not treat any of these as a nursery or a second ArrayPtr.

Traversal walks the slab: every slot of each resident chunk, live iff its
header `kind != 0` (chunks are carved whole and keep their size class;
released chunks are skipped so their zero pages are not refaulted). The
header is `kind` / `marked` / `fresh`, with no intrusive `next` link, which
saves 16 bytes per object. Collection trigger stays
`alloc_bytes` versus `gc_next_threshold` while **idle**. Safepoint mark +
lazy sweep (COI-309 S4) is documented in
[gc-incremental.md](gc-incremental.md). Mark seeds roots via slab lookup;
the list walk is only the sweep cursor.

### Lookup

`find_object_by_addr` / `contains_addr`:

1. Reject `addr == 0`.
2. Reject addresses not in a mapped chunk, or not a slot origin for that
   chunk's size class (alignment + stride).
3. `Object::from_header`; `kind == 0` → `None`.

Stale addresses are defined because the slot is still mapped. A swept object
is `None` because of poison, not because a HashSet forgot the key. Do not keep
a parallel live-set: two sources of truth.

`live_object_count` is a counter (alloc +1, sweep/dealloc −1), not a set size.

## Refuse

- Moving GC / compacting / forwarding pointers.
- Handle-table bytecode change (`Value` as an index).
- A second ArrayPtr opcode (pins already cover proven loops).
- Persisting coro pins across GC.
- A nursery / generational split in this cut.
- Wiring the unused `allocator.rs` sketch (`Rc`, not the live VM).

## Success (vs `main`)

Slab + header poison is **on `main`**. Re-check `binary_trees` malloc count /
heaptrack peak versus the COI-200 baseline (~137k mallocs, ~1.82 MB) and
`./scripts/poop_baseline.sh` with no mandelbrot / tak / nsieve regression.
Valgrind memcheck on debug `coil test` remains the leak gate.

Payload-layout follow-ups already on `main`: dense typed class slots
([#287](https://github.com/ardax-corp/coil-lang/pull/287)), inline-small
`ObjEnum` ([#290](https://github.com/ardax-corp/coil-lang/pull/290)),
inline-small typed instance slots
([#299](https://github.com/ardax-corp/coil-lang/pull/299)). Residual
cost is still payload `Vec`s for arrays/strings and large class/enum
spills — not identity hashing.

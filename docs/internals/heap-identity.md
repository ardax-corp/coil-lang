# Heap identity (mapped slab)

COI-200 asked whether `binary_trees`
is bound by `Heap::alloc` identity work: one `Box<GcData>` per object plus a
then-hot-path `live` HashSet probe in `find_object_by_addr`. That HashSet is
gone. This note is the layout. **Decision: implement slab + header poison**.
It is not a handle table, and objects never move: memory is freed by
reference counts ([memory.md](memory.md)).

Whatever resolves an address goes through `find_object_by_addr`; it must not
hash.

## Model (a slab, freed by counts)

`Value` stays a raw address. `Object::from_header` reads the header's kind tag.

Allocate `GcData<T>` headers from a **mapped slab** (size-class free lists;
64KiB anonymous chunks). A free **poisons** `GcHeader.kind = 0` and returns
the slot to the free list; chunks stay mapped, so stale lookups stay defined.
(The tracing collector also gave idle chunks' pages back with
`madvise(MADV_DONTNEED)` after its sweeps; counts do not yet.)
Payload `Vec`s (array
elements, interned string bytes) stay ordinary Rust allocs in this cut.
Typed class instances use dense slots
([#287](https://github.com/ardax-corp/coil-lang/pull/287)); small `ObjEnum`
payloads can inline ([#290](https://github.com/ardax-corp/coil-lang/pull/290));
typed instances with ≤4 fields keep those slots in the header
([#299](https://github.com/ardax-corp/coil-lang/pull/299)). Typed slots are
raw `Value` words, not tagged `Member`s: stores skip the heap probe, and
the count walks (`rc_dec_children`) and the cycle collector read each word by its kind.
Enum payloads and tuples are raw words the same way, with up to four inline
before a spill `Vec`. Class fields carry compile-time word kinds (archive
minor 24, `ClassWordKinds` per `type_id`): a scalar field (`int` / `float` /
`bool` / `byte` / scalar enum) is never counted; a pointer field (ground heap
type or niche word) is a precise reference; a generic or unresolved field
stays ambiguous. Enum payloads and tuples carry construction-site kinds
(`kinds` on MIR `Alloc`, 2 bits for each of the first four words, kept in the
payload's padding), from the source types or the words' `MirTy`. Generic shared
bodies and host-built enums stay unknown.
`rc-check` builds check that no pointer word names a freed slot.

Frames are no roots: every word a frame uses is owned by a count
([memory.md](memory.md#why-frames-are-no-roots)), so there are no stack maps or
root records. `compiler/src/mir/frame_words.rs` holds the word-kind analyses
the count insertion uses.

Statics carry a word kind per slot (archive minor 29), from their declared
type. A scalar static is no root, and a pointer static owns a count of its
target. Generic and compiler-allocated statics (FFI library / fn-id handles)
stay ambiguous: the cycle collector looks them up by address.

Arrays carry an element kind, set when the array is made. `Vec::new` /
`with_capacity` calls whose static element type is a ground pointer
(class, string, niche `Option` / `Result`, nested `Vec` or fixed array)
call `Vec::{name}$ptr` thunks, which make the empty array with
the element kind on MIR `Alloc`, as array literals of such elements are.
`Vec::from` copies its source's storage and kind. Counts then treat
every element as `0` or an object: precise, no classification. Only the
pointer kind is ever kept; a scalar stamp would be unsound because generic
shared bodies may store boxed values. Arrays from other natives and generic
code stay ambiguous. `rc-check` checks every element of a
pointer-kind array. Named `Table` instances (dicts, `INIT`) still hold `Member`s.

Traversal walks the slab: every slot of each resident chunk, live iff its
header `kind != 0` (chunks are carved whole and keep their size class;
released chunks are skipped). The
header is one word: the `kind` byte, a flags byte (a walk's visited mark, shared, finalized,
pinned, weak target), the generation bits, a debug byte and a 32-bit reference
count (`COUNT_IMMORTAL` never changes), with no intrusive `next` link, which saves 16 bytes per object. The cycle collector
runs when `alloc_bytes` passes `gc_next_threshold` ([memory.md](memory.md));
it resolves runtime roots via slab lookup.

### Lookup

`find_object_by_addr` / `contains_addr`:

1. Reject `addr == 0`.
2. Reject addresses not in a mapped chunk, or not a slot origin for that
   chunk's size class (alignment + stride).
3. `Object::from_header`; `kind == 0` → `None`.

Stale addresses are defined because the slot is still mapped. A freed object
is `None` because of poison, not because a HashSet forgot the key. Do not keep
a parallel live-set: two sources of truth.

`live_object_count` is a counter (alloc +1, free −1), not a set size.

## Refuse

- A handle table (`Value` as an index).
- Wiring the unused `allocator.rs` sketch (`Rc`, not the live heap).

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

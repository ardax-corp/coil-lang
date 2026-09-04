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
the slot to the free list; chunks stay mapped. Payload `Vec`s (array
elements, interned string bytes) stay ordinary Rust allocs in this cut.
Typed class instances use dense slots
([#287](https://github.com/ardax-corp/coil-lang/pull/287)); small `ObjEnum`
payloads can inline ([#290](https://github.com/ardax-corp/coil-lang/pull/290));
typed instances with ≤2 fields keep those slots in the header
([#299](https://github.com/ardax-corp/coil-lang/pull/299)).
Do not treat any of these as a nursery or a second ArrayPtr.

Traversal stays the intrusive `head` list. Collection trigger stays
`alloc_bytes` versus `gc_next_threshold`.

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

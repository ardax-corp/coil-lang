# GC evacuation

Mostly-copying (Bartlett) compaction of sparse slab chunks, on by default.
Set `COIL_GC_COMPACT=0` to turn it off (debugging a suspected move bug).
Mark / lazy sweep are unchanged ([gc-incremental.md](gc-incremental.md)).
Evacuation runs **between** cycles, once a sweep has finished.

## What may move

An object moves only when every reference to it is **precise**, meaning
one of:

- a pointer-kind word: a class field (`ClassWordKinds`), a payload / tuple
  word (`…K` construction kinds), or an element of a pointer-kind array
  (`TagArrayKind`);
- a `Member::Object` (dict tables, boxes, roots, poly-fn dictionaries);
- a masked coroutine slot;
- a must-pointer frame slot (`PRECISE_SLOT_MUST`).

Only instances, payload enums, boxes, tuples and arrays move. Strings,
closures, coroutines, streams, threads and immortal unit enums stay put.

These **pin** the object they reach:

- every ambiguous word (unknown kinds, closure captures, `Member::Value`,
  conservative frames, non-must slots, MIR stack-map slots);
- statics of unknown kind (generic or compiler-allocated) and the steal join
  root; pointer-kind statics are precise and rewritten, scalar ones are no
  roots (archive minor 29);
- `frame_pins` (Rust-held handles; the `dense_obj` index cache is not a root
  and is cleared at every collection);
- FFI library keys;
- the targets of `Root` (FFI may hold the address) and `Weak` handles.

## Steps

1. `Heap::evacuate_plan(pins, everything)`:
   - Classify every live object.
   - Per size class, pick the sparsest chunks whose objects can all move
     and still fit in the free slots of the chunks that stay.
   - Park the candidates' free slots so copies land elsewhere, then copy
     each movable object bitwise into a new slot of the same class. Spill
     `Vec`s move with their owner; the old copy is never dropped.
   - Rewrite every precise heap reference through the forwarding map. A
     `Result` `Err` word keeps its tag bit 0.
2. The VM rewrites its must-pointer stack slots.
3. `gc-stress` only: no heap word and no VM root may still name an old
   address (`Heap::verify_evacuation`).
4. `Heap::evacuate_finish` poisons and frees the old slots and returns the
   parked slots. It releases emptied chunks at once, so their pages go
   back to the OS. They stay mapped and are re-carved before a new chunk is
   mapped.

## When it runs

- **Where:** after a finished sweep (the lazy sweep's last quantum, or
  `gc::collect`).
- **When it skips:**
  - while more than one `execute` is active (a host frame such as a native
    re-entry or a finalizer may hold raw handles);
  - in a shared-heap epoch;
  - under the debugger.
- **How often:** outside stress mode, one attempt every 8 collections.
  Unproductive attempts back off up to every 256 collections.
- **Worth-it bars:**
  - before walking the heap, free slots must be at least 1 MiB and at least
    a quarter of the **resident** slab;
  - the whole plan must empty that much too.
- **Step size:** one step moves at most 64 chunks (4 MiB). A capped plan
  continues at the next collection.
- **Stress mode:** `gc-stress` builds collect at every allocation and move
  every movable object each time. CI runs the language harness (`coil-test`) that way.

## Numbers (release, `examples/perf`)

| Bench | Without | With evacuation |
|-------|------:|-------------:|
| `gc_frag` final RSS (400k list thinned to 100k, then churn) | 35.0 MB | **21.5 MB** (slab 21.9 → 5.5 MB resident; live 5.4 MB) |
| `gc_frag` wall | 294 ms | 302 ms |
| `gc_churn` / `class_wide_live` / `binary_trees` / `gc_shrink` wall | — | within noise |

On by default. Instruction counts are
unchanged on programs that never collect (fib, tak, nsieve, mandelbrot) and
+0.4–0.6% on collection-heavy ones (the analysis walk). Wall-time
differences on non-collecting programs are code layout, since the
instruction counts are equal.

## Not yet

- Liveness in precise frame maps. Stale loop slots stay ambiguous and pin
  what they hit.
- A nursery with bump allocation. That needs a write barrier, a separate
  decision.

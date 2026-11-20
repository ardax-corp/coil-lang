# GC evacuation

Mostly-copying (Bartlett) compaction of sparse slab chunks, on by default.
In `machine`'s unit tests, `COIL_GC_COMPACT=0` turns it off (debugging a
suspected move bug); other builds always evacuate.
Mark / lazy sweep are unchanged ([gc-incremental.md](gc-incremental.md)).
Evacuation runs **between** cycles, once a sweep has finished.

## What may move

An object moves only when every reference to it is **precise**, meaning
one of:

- a pointer-kind word: a class field (`ClassWordKinds`; a tagged flat
  field's first word while its tag says pointer), a payload / tuple
  word, or an element of a pointer-kind array (construction kinds, `kinds` on
  MIR `Alloc`);
- a `Member::Object` (dict tables, boxes, roots, poly-fn dictionaries);
- a masked coroutine slot.

Only instances, payload enums, boxes, tuples, arrays and closures move.
Strings, coroutines, streams, threads, immortal unit enums and the shared
empty tuple `()` stay put.

These **pin** the object they reach:

- every ambiguous word (unknown kinds, closure captures, `Member::Value`);
- every word of a native frame or an interpreter activation
  (`Runtime::for_each_native_root`), and the words of parked tasks;
- statics of unknown kind (generic or compiler-allocated) and the steal join
  root; pointer-kind statics are precise and rewritten, scalar ones are no
  roots (archive minor 29);
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
2. Pointer-kind statics are rewritten.
3. `gc-stress` only: no heap word and no root may still name an old
   address (`Heap::verify_evacuation`).
4. `Heap::evacuate_finish` poisons and frees the old slots and returns the
   parked slots. It releases emptied chunks at once, so their pages go
   back to the OS. They stay mapped and are re-carved before a new chunk is
   mapped.

## When it runs

- **Where:** after a finished sweep (the lazy sweep's last quantum, or
  `gc::collect`).
- **When it skips:**
  - in a shared-heap epoch, or on a borrowed heap;
  - while a collection cycle is open.
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

## Numbers (release, `examples/perf`, on the bytecode VM, since deleted)

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

# Escape facts and early freeing

Coil keeps its tracing GC, but the compiler frees memory itself wherever it
can see when an object dies. The GC is the backstop for data whose lifetime
is decided at run time (map entries, graph nodes, anything stored in a
mutable field). Nothing here needs annotations: every fact is inferred.
Stating a fact on a function is optional and only checked.

The work happens on the feature branch `feature/escape-memory`, one PR per
step. Assessment and measurements: the "Coil memory management: can we drop
the GC?" doc (2026-10-26).

## What exists before this work

- Scalar replacement inside one body, after inlining: class field-SROA
  (Q2, `hir/lower.rs` `class_boxes`), `hir/readonly_sroa.rs`,
  `hir/tuple_sroa.rs`, `hir/enum_sroa.rs`, `[T; N]` slots (Q1).
- Value representation: immediates, pointer niches, two-slot
  `Option`/`Result`/pairs, packed `Vec<byte>`/`Vec<bool>` ([layout.md](layout.md)).
- Fresh, private and owned locals per body for purity (`hir/effects.rs`
  `BodyFacts`).

Missing: facts across calls ("this parameter never escapes", "this result is
fresh"), freeing a variable-size object before a collection, and knowing a
value's last use.

## Where the allocations go

Each remaining allocation falls in one class (main `35e14ea`, release,
`COIL_AUTO_PAR=0`):

| Class | Example | Freed by |
|-------|---------|----------|
| A. Local temporary | `let b = to_bytes(s);` in `csv::field_needs_quote` | Step 2 |
| B. Dead on reassignment | `s = s + piece`, `join`'s `out = out + parts[i]` | Step 3 |
| C. Converted, then dropped | buffer passed to `from_bytes` | Step 3 |
| D. Fresh result used by the caller | `item_check(bottom_up(d))` | Steps 1 and 2 |
| E. Stored in a container | map keys, CSV rows, JSON values | GC |

## Baseline

Heap objects per run and GC safepoint time (instrumented build, best of 3):

| Program | Objects | GC share | Classes |
|---------|---------|----------|---------|
| binary_trees (14 / 16) | 1,600,190 | 34% | D |
| b_csv (1 MB) | 545,341 | 4% | A 47%, C 24%, E 29% |
| b_json (1.1 MB) | 348,579 | 1% | A 37%, C 32%, E 31% |
| strings (50,000 / 200,000) | 300,033 | 9% | B 67%, A/E 33% |
| byte_scan (16,384 / 131,072) | 32,787 | 10% | B 100% |
| hashmap (200,000) | 40,179 | under 1% | E 100% |
| result_heap_churn | 10,000,005 | 14% | D 100% |

## Steps

| Step | What | Status |
|------|------|--------|
| 0 | This plan | done |
| 1 | Escape summaries: per function, which parameters may escape and whether the result is fresh; whole-program fixed point next to `ProgramEffects`; shown by `coil dissect --effects` | done |
| 1b | Optional stated facts (`#[noescape(x)]`, `#[fresh]`), checked against the inferred summary (E0139) | done |
| 2 | Free at scope end and after the call for non-escaping fresh objects; check mode, gc-stress | done |
| 3 | Static uniqueness: free a string's old value when it is rebuilt in place, free fresh concatenation operands and variable-size match scrutinees; zero-copy `to_bytes`/`from_bytes`; enum SROA dropped | done |
| 4 | Copying nursery for young objects: write barrier, minor collections, pretenuring, held pins | done |
| 5 | Opt-in checked `region` block | |
| 6 | Re-measure; decide whether anything else is worth doing | |

## Escape facts (step 1)

`hir/escape.rs` adds three facts to each body's effects `Summary`, so the
effects fixed point solves them across calls and modules:

- `escapes`: bit `i` when the call may keep what parameter `i` reaches
  after it returns, other than through its result. Keeping means a store
  into an object that is not a fresh object of this body, a static, a
  closure or `defer` capture, a `yield`, or passing it where a callee keeps
  it. A coroutine keeps all its parameters.
- `returns`: bit `i` when the result may reach what parameter `i` reaches.
- `shared_ret`: the result may reach an object the call did not allocate
  (a parameter, a static, a callee's shared result), or
  a fresh object something else also reaches. When it is `false`, a caller
  that drops the result may free everything it reaches.

The walk is flow-insensitive: a local holds every value assigned to it, and
a field, element or pattern binding reaches whatever its source reaches.
Allocation sites (`Make`, string concatenation, a call with a fresh
result, a lambda) are tracked separately, and a store into one of them is
held there instead of escaping, so a builder that fills a new `Vec` and
returns it keeps nothing. Values of scalar types reach nothing.

Callees the analysis cannot see (function values, unresolved methods,
trait declarations, natives) keep everything and return nothing fresh.
Pure natives and the IO writers keep nothing; `to_bytes`, `from_bytes`,
`format`, the string slices and `Vec::with_capacity` return a fresh
object (checked against the VM for step 2).

`coil dissect --effects` prints one `;; escape NAME: …` line per function:
`keeps nothing`, `keeps `t``, `returns `self``, `result fresh`. In
binary_trees, `bottom_up` has a fresh result and `item_check` keeps
nothing, which is what step 2 needs to free each tree after its check.

A function may state its facts, and never has to: `#[noescape(a, b)]`
says the call neither keeps nor hands back those parameters, `#[fresh]`
says its result is fresh. A stated fact is checked against the inferred
one and is E0139 when it does not hold (or names no parameter); it is
never trusted in place of the analysis.

A string literal reaches nothing: it is interned, immutable, and the VM
never frees an interned string (step 3; step 1 counted it as shared, so an
`Err` holding a literal made a whole `Result` shared). Archives are byte-identical to main and
compile time is unchanged (b_json 0.30 s, b_csv 0.10 s).

`fn drop()` may run at the point an object is freed early instead of at a
collection (decided 2026-10-26).

## Early freeing (step 2)

`hir/early_free.rs` rewrites a body after the HIR optimizer (inlining,
scalar replacement, unrolling, folding) has run, so it frees only objects
that still exist as objects. The escape walk of step 1 runs again on the
optimized body, with callee summaries from `ProgramEffects`
(`ProgramEffects::early_frees`); a callee it cannot resolve there keeps
everything. Two shapes are freed:

- **Call temporaries.** In `f(…, g(x), …)`, when `g`'s result is fresh and
  `f` neither keeps nor returns that argument, the argument is bound to a
  temporary and freed after the call. When `f` does not change what the
  argument reaches either, the free is deep: the VM frees every object it
  reaches through precise pointer words. `item_check(bottom_up(d))` frees
  each tree after its check.
- **Block-end locals.** A `let` local whose every value is an object this
  body allocated, that no other local, store, capture, `yield` or result
  reaches, and that is read only inside its block, is freed (that object
  only, not what it holds) where the block ends: at the clear typed
  inlining put after its last read, or after the block's tail, and before
  every `return` after its `let`, nested in a loop or `if` or not (a final
  `return` often sits in the block's tail). `break`, `continue` and
  panics leave it to the collector.

Each free is `Builtin::FreeEarly { deep }` of the local and a `Clear` of
its frame word. It lowers to MIR `FreeEarly` and to the `FreeEarly`
instruction (archive 6.1), which pops the word; the VM runs it in its main
dispatch loop. (Step 2 first used a host call; that left the fast loop on
every free, and a `join` with a free per part ran 25% slower.) The VM frees nothing while a collection cycle or a
steal epoch is open, and skips what it cannot free safely: interned
strings, the shared unit variants and `()`, objects of a type with a
`drop` (while any is registered), handles, and arrays an `ArrayPin` holds
for its frame. A dense index's pin is only a cache of its register's
object: the free drops it, so a later object in the same slot is never
read through the stale entry. A freed slot goes back to the slab like a
swept one, and the dense object cache forgets it.

A dead frame word (a call's argument copy, a callee's frame left above the
stack top) may still hold a freed address. Conservative and precise root
scans already resolve every word through the slab, so such a word marks
nothing, or the object now in that slot. Once something was freed early,
typed `Member::Object` handles are resolved the same way before marking,
and gc-stress builds accept a freed slot in a pointer word.

Bodies that stay as they are: coroutines, bodies with `defer`, bodies
spliced from another module, and any body whose rewritten form does not
plan. `let _ = …` locals and locals a lambda or `defer` captures are never
freed.

Two holes the corpus found in the step 1 facts are closed here: a callee
that changes what an argument reaches may store anything there, so a fresh
object passed to one now holds "anything" (its graph is no longer fresh);
and a call whose result is a function may be a partial application whose
closure keeps the arguments, so it is treated as an unknown callee.

Switches: `COIL_EARLY_FREE=0` compiles no frees and frees nothing at run
time. `COIL_EARLY_FREE=check` compiles them, frees nothing, and at the next
collection marks again from the roots that are certainly live (precise
frame slots, statics, VM handles; not words a conservative frame scan
finds), aborting if that reaches an object a free gave up. The test corpus
passes in both modes and with frees on.

### Measured (step 2)

`COIL_EARLY_FREE=0` against `=1` on the same build (`COIL_AUTO_PAR=0`,
2026-10-27). Allocations, frees and collections come from the local
allocation-profiling build. Time is the median of 7 runs and peak RSS the
largest, both from the plain release build. The machine is noisy: time
differences under about 10% are within run-to-run spread.

| bench | objects allocated | freed early | collections | GC time | time | peak RSS |
|---|---|---|---|---|---|---|
| binary_trees 16 | 7,449,278 | 7,383,732 (99%) | 71 → 2 | 578 → 2 ms | 1703 → 1522 ms | 28 → 18 MB |
| b_csv (1 MB) | 545,341 | 384,531 (71%) | 4 → 3 | 20 → 4 ms | 525 → 550 ms | 44 → 23 MB |
| b_json | 348,579 | 223,998 (64%) | 3 → 2 | 5 → 0 ms | 695 → 683 ms | 38 → 23 MB |
| strings | 300,033 | 4 | 9 → 9 | 11 → 12 ms | 126 → 124 ms | 38 → 38 MB |
| hashmap | 40,179 | 4 | 4 → 4 | 3 → 3 ms | 374 → 400 ms | 25 → 25 MB |
| b_coll | 200,192 | 1 | 5 → 5 | 10 → 10 ms | 3723 → 3763 ms | 146 → 148 MB |
| byte_scan | 32,787 | 3 | 5 → 5 | 1 → 1 ms | 12 → 13 ms | 12 → 12 MB |

What is left is what the plan expected later steps to take: strings
rebuilt in place (`s = s + x`, step 3), container contents (step 4 and
the GC), and the byte copies kept by `to_bytes` / `from_bytes` pairs
(step 3 zero-copy). These numbers were taken while each free was a host
call; the `FreeEarly` instruction replaced it (see step 3's numbers).

## Uniqueness (step 3)

A *linear* local is one the escape walk can follow by value: a string or a
vector of plain values, bound once by its `let` with no compound
assignment, whose every value is fresh (a literal, a concatenation, a block
ending in one, or a call whose summary returns a fresh, unshared result),
and whose every read uses the value without keeping it (an operand, an
index or field read of a plain value, a `return`, an argument a callee does
not keep, the receiver of a vector method). For such a local:

- `x = v` frees the old value after `v` is computed: `s = s + piece` in a
  loop leaves one live string instead of one per pass.
- It is freed at the end of its block like any step 2 local, including
  when a callee appends to it (a vector of plain values given to a callee
  that changes it still reaches nothing the callee could have stored).

A concatenation whose operands include fresh call results (`a + f(x) + b`)
binds those results to temporaries and frees them once the result is
built; the chain stays one chain, so `FORMAT` fusion still applies.

Freeing now has its own instruction (`FreeEarly`, archive 6.1) instead of
a host call: a host call left the VM's fast dispatch loop on every free,
and a `join` with one free per part ran 25% slower; with the instruction
`join` and `append` time the same with frees on and off.

Later in step 3:

- **Variable-size match scrutinees.** `match f(x) { .. }` binds a fresh
  scrutinee to a temporary and frees it after the match when no arm binding
  or the match value keeps it. Only strings, vectors and options of them
  qualify: freeing small fixed-size objects one at a time costs more than
  the sweep that would reclaim them (result_heap_churn ran 17 to 25% slower
  with every scrutinee freed, and still 14% with the free made cheaper).
- **Vector reassignment.** A linear vector local whose elements are fresh
  heap values (`list = rebuild(list)`) frees the old vector deeply when it
  is reassigned, provided no other local still shares its elements.
- **Literals count as fresh.** String literals are interned and never freed
  by the VM, so a value that may be a literal is no longer treated as
  shared.
- **Zero-copy `to_bytes`.** `let b = to_bytes(s)` whose only reads are
  `b[i]` and `b.len()` reads `s` directly (string indexing yields the same
  byte); no buffer is built.
- **Zero-copy `from_bytes`.** `return from_bytes(buf)` where `buf` is a
  local vector allocated in this body and not kept anywhere calls
  `from_bytes_take`, which moves the bytes into the string instead of
  copying them (and puts them back if they are not valid UTF-8). b_csv's
  per-field `bytes_to_s(out)` is not covered yet: the buffer arrives as a
  parameter of a call that is not inlined.

Both zero-copy rewrites measured as a 1 to 2% instruction win under
callgrind; they go in anyway because larger programs convert far more text
than these benchmarks. Enum SROA on a `match` of an inlined call was
dropped: b_json and b_csv allocate under 600 enums each.

### Measured (step 3)

Same method as step 2 (5 runs, 2026-10-27), all of step 3 in. b_csv and
b_json allocate fewer objects with frees on because of the zero-copy
rewrites.

| bench | objects allocated | freed early | collections | GC time | time | peak RSS |
|---|---|---|---|---|---|---|
| binary_trees 16 | 7,449,278 | 7,383,731 (99%) | 71 → 2 | 670 → 2 ms | 2078 → 1550 ms | 28 → 17 MB |
| strings | 300,033 | 200,001 (67%) | 9 → 2 | 12 → 1 ms | 132 → 129 ms | 37 → 38 MB |
| byte_scan | 32,787 | 32,769 (99%) | 5 → 1 | 1 → 0 ms | 16 → 14 ms | 12 → 12 MB |
| b_csv (1 MB) | 545,341 → 417,145 | 256,335 (61%) | 4 → 3 | 25 → 4 ms | 583 → 608 ms | 44 → 22 MB |
| b_json | 348,579 → 228,578 | 111,999 (49%) | 3 → 2 | 5 → 0 ms | 738 → 694 ms | 38 → 24 MB |
| gc_rebuild | 3,600,006 | 3,300,002 (92%) | 15 → 3 | 357 → 9 ms | 732 → 575 ms | 67 → 47 MB |
| gc_shrink | 3,353,130 | 400,001 (12%) | 169 → 142 | 224 → 226 ms | 498 → 492 ms | 63 → 66 MB |
| result_heap_churn | 10,000,005 | 1 | 686 → 686 | 189 → 166 ms | 949 → 939 ms | 12 → 12 MB |
| hashmap | 40,179 | 3 | 4 → 4 | 4 → 4 ms | 426 → 442 ms | 25 → 25 MB |
| b_coll | 200,192 | 1 | 5 → 5 | 13 → 14 ms | 4141 → 4154 ms | 148 → 147 MB |

The machine is noisy; differences under about 5% in time are not
meaningful. b_csv's time is within that band.

What b_csv and b_json still allocate and do not free is their output: the
parsed field strings and row vectors (b_csv), and the decoded strings and
store (b_json). Those are live until the program drops the result, which
is the GC's job (or step 4's).

## Nursery (step 4)

After step 3, collections cost little marking: gc_shrink keeps 11 live
objects and result_heap_churn about as few. What is left is reclaiming
dead objects one at a time. The sweep visits each one, drops its payload
and pushes its slot onto a free list, about 160 instructions per object.
gc_shrink spends about 45% of its time there and result_heap_churn 18%;
every other benchmark spends under 3%. Freeing small fixed-size objects
early made this worse (step 3), so the fix is to stop paying per dead
object at all.

Design (copying nursery, decided 2026-10-27):

- **Young chunks.** Instances, payload enums, boxes, tuples and arrays,
  the kinds evacuation may move, are bump-allocated into *young* slab
  chunks, one current chunk per size class. Every other kind (strings,
  closures, coroutines, handles) and every allocation in a shared-heap
  epoch goes to the old space as before. The nursery holds a fixed number
  of chunks; filling it asks for a minor collection at the next
  safepoint.
- **Write barrier.** An old object is *remembered* when its payload is
  written: every payload write goes through `GcData::as_mut`, which tests
  one header bit and, the first time, records the object's address. An
  old object allocated with references (a closure, a coroutine) is
  remembered when it is born. A prototype of the barrier measured within
  noise (0 to 3%).
- **Minor collection.** Trace young objects from the VM roots and the
  remembered objects, never into old ones. A young object reached through
  an ambiguous word (a conservative frame word, an unknown-kind field, a
  `Root` or `Weak` target, an FFI argument) is *pinned*. Every other live
  young object is copied bitwise into an old slot of its size class, and
  precise words naming it are rewritten: must-pointer frame slots, pointer
  statics, the remembered objects, and the survivors themselves. This is
  the evacuation machinery of [gc-evacuation.md](gc-evacuation.md),
  limited to the nursery.
- **Reset.** A young chunk without pinned objects is reused whole: each
  dead object only has its payload dropped (a no-op for inline payloads)
  and its header poisoned. A chunk with a pinned object is *held*: the
  pinned objects stay young where they are, later minors treat them as
  roots and never move them, and only a major cycle frees them (its mark
  sees every young object). The chunk is reused once a major empties it;
  enough held chunks ask for a major. (Promoting such a chunk instead
  left a 64 KiB chunk per stale stack word: result_heap_churn grew from
  12 to 31 MB.)
- **Early frees** of young objects (step 2) put the slot on a per-class
  list that the bump allocator hands out first, so a nursery does not
  undo early freeing's reuse.
- **Pretenuring.** A minor where most of the nursery survived (a large
  structure being built) copied for nothing: the next nursery's worth of
  allocations goes old, twice as long for each such minor in a row (up to
  32 nurseries), and the nursery is tried again after.
- **Quiet barrier.** While no young object exists (right after a minor,
  while pretenuring), a write cannot store one: the barrier logs the
  object into a *quiet* set instead of the remembered set and stops
  firing for it, and the set's objects are re-armed when the nursery
  starts again. Without it gc_rebuild remembered every pretenured node
  (900k entries per minor).
- **Major collections** stay mark-and-sweep. A minor collection runs first,
  so the major mark sees an empty nursery, and the sweep skips young
  chunks.
- **Where it cannot move,** it promotes instead: while a host frame is
  active (a nested `execute`), under the debugger, and with
  `COIL_NURSERY=0`.
- **Not in the nursery:** objects of a type with `drop`. A young object
  that becomes finalizable later (an enum tagged after it is built)
  survives every minor as a root, so its `drop` runs from a major.
- The slab's address-to-chunk cache went from 8 to 64 entries: young
  chunks spread the working set, and 8 entries missed on hashmap and
  b_coll (+13% and +10% instructions). With 64 both configurations miss
  almost never.

### Measured (step 4)

`COIL_NURSERY=0` against `=1`, early freeing on in both, median of 7 runs,
2026-10-27. Instructions are callgrind totals (deterministic); wall time
on this machine moves about 5% between runs.

| bench | instructions | time | peak RSS |
|---|---|---|---|
| gc_shrink | | 516 → 383 ms | 66 → 43 MB |
| result_heap_churn | | 1066 → 1003 ms | 12 → 12 MB |
| b_csv (1 MB) | | 603 → 598 ms | 22 → 24 MB |
| b_json | | 751 → 802 ms | 24 → 25 MB |
| strings | | 137 → 136 ms | 38 → 38 MB |
| b_coll | 45.09 → 44.98 G | 4089 → 4178 ms | 147 → 123 MB |
| binary_trees 16 | 17.27 → 17.36 G | 1597 → 1714 ms | 17 → 20 MB |
| hashmap | 4.73 → 4.84 G | 455 → 487 ms | 25 → 31 MB |
| gc_rebuild | 6.55 → 7.10 G | 595 → 664 ms | 47 → 57 MB |

The nursery pays where step 3 left per-object reclaiming: gc_shrink is
26% faster with a third less memory. Where early freeing already reclaims
nearly everything (binary_trees, gc_rebuild) or little garbage exists
(hashmap, b_coll), it costs up to 8% in instructions: gc_rebuild copies
every node of the list it is building once before pretenuring kicks in,
and binary_trees and hashmap run within 2.5% of the instructions but touch more
memory. Whether it stays on by default is decided with step 6.

# Constant lookup tables

A `const` set to a `readonly` array literal is a lookup table
([coil-lang#1561](https://github.com/ardax-corp/coil-lang/issues/1561)):

```coil
const CRC_TABLE = readonly [0, 1996959894, 3993919788, /* … */];

fn popcount8(int x) -> int {
    const BITS = readonly [0, 1, 1, 2, 1, 2, 2, 3, 1, 2, 2, 3, 2, 3, 3, 4];
    return BITS[x & 15] + BITS[(x >> 4) & 15];
}
```

Born `readonly` with no `readonly T` to `T` conversion (#1558), the array
has no mutable alias, so no code can change an element, and array `==` is
structural, so sharing one copy is not seen. The compiler uses that three
ways.

## Built once

- **Module table.** A module `const` whose initializer does not fold is a
  static slot set once at startup (`const_fold::lift_startup_consts`,
  #1558), so a module table already had one array. Nothing new here.
- **Local table.** A function-local `const t = readonly [lit, …]` whose
  elements are all literals (int, float, bool, string) and whose reads are
  all `readonly` is built into a compiler-made static slot (`__table$N`,
  `Checker::alloc_synthetic_static_slot`) the first time it runs, and
  loaded after that: `LoadStatic`; if zero, build and `StoreStatic`; the
  join is the table (`mir::from_hir` `frozen_array`, the pattern of the
  ground dictionaries in [generic-specialization](generic-specialization.md)).
  Tables of the same literals share a slot. The slots start zeroed, so the
  prologue does not run for them (`prologue_jmp_target`). A table inside a
  loop (a callee's, spliced into its caller's loop) is left an array per
  evaluation: dense LICM moves that allocation to the loop's preheader,
  which is cheaper than a first-use check per iteration and keeps the loop
  eligible for auto-par.

`Compiler::hir_frozen_arrays` finds the local tables (`hir::tables::local_table`,
`sealed_locals`, `lets_in_loops`); `Compiler::register_frozen_tables`
records the module ones per slot, with each element's value when it folds,
ahead of the functions that read them.

## Reads at a constant index fold

`hir::tables::fold_tables` runs with the HIR folds (at every level, before
`hir::fold` so the literals cascade): `T[k]` with a literal `k` in range is
element `k` when it folds (module table: int, float or bool checked against
the read's type; local table: the element literal), and `T.len()` is the
count. An index out of range stays and traps at run time. A local table
whose every read folded goes, with its array. A module table read from
another module folds the same way: the table is keyed by its slot, which
is program-wide.

## Reads with a variable index are invariant

`mir::mark_frozen_arrays` marks, in `MirFunc::invariant_loads`, the
`LoadStatic` of a module table's slot, the join of a local table, a φ of
only those, and every `Index` and `ArrayLen` of one of them. LICM moves a
marked `LoadStatic` and length freely (a dense body only: LIR keeps heap
words in place) and a marked element read behind the loop test whatever
the loop stores or calls; local CSE never kills them. In the CRC loop the
table's slot is now loaded once ahead of the loop, which reads one element
per byte.

## Auto-par workers

A shared-heap job (auto-par loop chunks, `thread_spawn_shared`) used to
start with no statics at all, so a startup `const` read in a parallel loop
was a `LoadStatic` past the end of an empty array (a crash: auto-par counts
those reads as pure, #1558). A shared-heap job now starts with a copy of
the spawning VM's statics (`ThreadSpawnContext::statics`,
`Machine::load_shared_statics`); the words name the one shared heap, and
nothing collects during the epoch. A local table a worker builds lands in
its own copy, which the job drops when it ends. Isolate jobs still start
with zeroed statics ([shared-heap sendability](shared-heap-sendability.md)).

## Measurements

`examples/perf/const_table_crc.hy` (CRC-32 over a 4 KiB buffer, 2000
times, through a 256-entry module table) and
`examples/perf/const_table_local.hy` (a 16-entry local S-box in a function
called 4M times). Release build, `COIL_AUTO_PAR=0`, wall time of
`coil run` on a compiled archive, best of 21 on a shared 4-core machine:

| Bench | Before | After |
|-------|--------|-------|
| `const_table_crc` | 724 ms | 530 ms |
| `const_table_local` | 1119 ms | 601 ms |

The CRC loop loses its per-byte `LoadStatic; STORE` (7 ops per byte, now
5). The S-box function no longer allocates its 16-element array per call:
a thousand calls allocate it once (`perf_metrics::local_const_table_is_built_once`).
The flagships (`mandelbrot`, `tak`, `nsieve`, `binary_trees`, `fib`) and
every other `examples/perf` bench compile to the same bytecode.

Tests: `tests/collections/const_tables.hy`, `tests/modules/const_table_import.hy`,
`tests/codegen/const_table_auto_par.hy`, `tests/codegen/perf/const_table_*.hy`;
`compiler/tests/perf_metrics.rs` (`const_table_constant_reads_fold`,
`const_table_slot_load_leaves_the_loop`, `local_const_table_is_built_once`);
`compiler/src/hir/tables.tests.rs`.

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
| 1 | Escape summaries: per function, which parameters may escape and whether the result is fresh; whole-program fixed point next to `ProgramEffects`; shown by `coil dissect --effects` | |
| 1b | Optional stated facts (`noescape` on a parameter), checked against the inferred summary | |
| 2 | Free at scope end and per-statement regions for non-escaping fresh objects; poison-on-free debug mode, gc-stress | |
| 3 | Static uniqueness: append in place, zero-copy `to_bytes`/`from_bytes`, enum SROA for a `match` on an inlined call result | |
| 4 | Nursery for young objects | |
| 5 | Opt-in checked `region` block | |
| 6 | Re-measure; decide whether anything else is worth doing | |

`fn drop()` may run at the point an object is freed early instead of at a
collection (decided 2026-10-26).

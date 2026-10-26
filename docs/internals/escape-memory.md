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
| 2 | Free at scope end and per-statement regions for non-escaping fresh objects; poison-on-free debug mode, gc-stress | |
| 3 | Static uniqueness: append in place, zero-copy `to_bytes`/`from_bytes`, enum SROA for a `match` on an inlined call result | |
| 4 | Nursery for young objects | |
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
  (a parameter, a static, a string literal, a callee's shared result), or
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
`format`, the byte and string slices and `Vec::with_capacity` return a
fresh object (to be confirmed against the VM before step 2 frees them).

`coil dissect --effects` prints one `;; escape NAME: …` line per function:
`keeps nothing`, `keeps `t``, `returns `self``, `result fresh`. In
binary_trees, `bottom_up` has a fresh result and `item_check` keeps
nothing, which is what step 2 needs to free each tree after its check.

A function may state its facts, and never has to: `#[noescape(a, b)]`
says the call neither keeps nor hands back those parameters, `#[fresh]`
says its result is fresh. A stated fact is checked against the inferred
one and is E0139 when it does not hold (or names no parameter); it is
never trusted in place of the analysis.

Known gaps: the facts do not tell variants apart, so an `Err` that holds a
string literal makes a `Result` result shared even on the `Ok` path
(`csv::Parser::parse_unquoted`). Archives are byte-identical to main and
compile time is unchanged (b_json 0.30 s, b_csv 0.10 s).

`fn drop()` may run at the point an object is freed early instead of at a
collection (decided 2026-10-26).

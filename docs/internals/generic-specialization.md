# Per-type clones and ground dictionaries

GitHub: #1416, #1417, #1419. Status: **implemented**.

A generic body shared by every instance takes its type arguments boxed and
its trait bounds as dictionaries (tuples of method code pointers). In
`HashMap<int, int>` and `sort(Vec<int>)` that meant a box per argument and a
fresh dictionary tuple on every call. Three changes take that away for
ground calls.

## Ground dictionaries once (#1417)

A dictionary made only of method pointers (and such dictionaries) is built
once, on first use, into a synthetic static `__dict$N` (one per distinct
dictionary) and loaded after that: load, compare with 0, build and store.
It is built lazily because worker VMs start with zeroed statics.

- MIR bodies: `intern_dict_ops` (`codegen/emit_hir_mir.rs`) replaces each
  maximal ground `Tuple` subtree with `HirDictOp::Interned`.
- IL thunks: `append_dicts_interned` emits the same pattern from the
  dictionary plan into a fragment `CodeBuf` and `append`s it, so its
  `CodePtr`s are rewritten to entries; a reserved entry is emitted as a
  root entry, which `append` keeps.
- These statics need no setup, so `prologue_jmp_target` does not count them.

Test: `perf_metrics::ground_dictionary_is_built_once`.

## Clones call their siblings' clones (#1419)

A clone (`Bag::twice$inst$int`) has no dictionary slots. A bounded sibling
call in its generic body (`self.add(..)`) forwarded a dictionary, which the
clone dropped. In a clone such a call is a ground call site: `self` takes the
clone's type (`hir_clone_self_ty`), the callee's parameters bind from it, and
the call goes to the sibling's clone (`hir_clone_drops_forwarded`).

Test: `tests/generics/method_clone_sibling_dicts.hy`.

## Clones of another module's generics

A ground call to a generic class method (`HashMap::insert`) or a generic
free function (`collections::sort`) declared in an earlier module gets a
per-type clone compiled from that module's own body, so arguments stay in
their own layout and bounds resolve statically.

- **Module context.** `check_program` clears the checker's per-module facts,
  and spans and node ids collide across files, so each module that declares
  clone sources keeps a `ModuleCtx` (its checker, HIR, namespace, mono plan
  and the other per-body state) at the end of its compile
  (`take_module_ctx`). Only the program-wide checker state (substitution,
  type-variable counter, static slots) is shared, swapped in with
  `swap_program_state`.
- **Queue.** `emit_instance_clones` sends another module's clone request to
  `foreign_clone_queue`; after each module the pipeline calls
  `emit_foreign_clones`, which swaps that module's context in and compiles
  the clones from its AST (`ast_cache`) and HIR.
- **Free functions.** A generic free function is a clone source like a
  static method with its own type parameters (`note_fn_clone_src`). Its own
  module's calls keep their mono clones; calls from another module or from
  a clone use the clone, so `sort`'s call to `merge_range` is cloned too.
- **Gates.** A clone falls back to the shared body (through a boxing thunk)
  when its types mean something else to the old module (`knows_tys`: a
  class declared later is not in its checker), or when the generic body has
  an enum over its type parameters (`layout_safe`: `Option<T>` in a shared
  field would be stored in the clone's layout and read in the shared one,
  as `task::Channel` did).

Tests: `perf_metrics::foreign_method_clone_is_specialized`,
`perf_metrics::foreign_fn_clone_is_specialized`.

## How many clones

A ground call clones, at any number of types, with two exceptions that
take the shared body instead (warning E0804):

- **Polymorphic recursion.** Each clone carries the functions on its
  chain of clones (the clone whose body asked for it, that clone's, …). A
  call to one of them at new types (`f<T>` calling `g<(T, int)>` calling
  `f<((T, int), int)>`, or `Nest<T>::depth` calling
  `Nest<Nest<T>>::depth`) would grow without end, so it keeps the shared
  body. Chains are no longer than the number of generic functions, so
  planning terminates. Free functions: `collect_transitive`
  (`monomorphize.rs`); instance and foreign clones: `InstClone::chain`,
  `Compiler::inst_clone_recursive`. A recursive call at types another
  path clones anyway calls that clone.
- **Budget.** `--clone-budget N` (default 4096,
  `monomorphize::MAX_TOTAL_SPECIALIZATIONS`) bounds one module's plan and,
  separately, a program's instance and foreign clones, against fan-out
  through many functions at distinct types. Tests that need the shared
  body set it low (`// @flags --clone-budget 8`).

Until 2026-10 these were counts (8 clones per function or method, 64 per
module, 256 instance clones per program), which real code did not reach
except coil-tui's component methods, and which were the only thing that
stopped polymorphic recursion. With the counts raised to 64/1024 the
examples, `examples/perf`, spool and coil-http compiled to byte-identical
archives; a file calling three generics at 60 types each compiled 45%
slower into a 22% larger archive; and two mutually recursive generics
took 1.7 s and 346 MB to compile (without the counts, forever).

## Measurements

2026-10-17, one run each, `--auto-par off`, `vm_profile` builds (counting
allocations slows both): `d1688ee` (before #1416) vs the branch with all of
the above. Managed allocations from `machine::alloc_count`.

| bench | before | after |
|---|---:|---:|
| `b_coll` allocations | 8,800,193 | 200,192 |
| `b_coll` GCs | 18 | 5 |
| `b_coll` wall | 13.6 s | 8.0 s |
| `map_int_insert1M` | 3693 ms | 2674 ms |
| `map_int_contains1M` | 2163 ms | 1016 ms |
| `map_str_insert200k` | 764 ms | 652 ms |
| `sort1M` | 6476 ms | 3261 ms |
| `binary_trees` allocations | 67,252 | 67,252 |

Plain release builds of the branch: `map_int_insert1M` ~1550 ms,
`map_int_contains1M` ~570 ms, `sort1M` ~1665 ms (from 2318 / 984 / 3140 ms
before the clones). `binary_trees` is unchanged: its allocations are the
tree nodes themselves.

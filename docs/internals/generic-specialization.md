# Per-type clones and ground dictionaries

GitHub: #1416, #1417, #1419. Status: **implemented**.

A generic body shared by every instance takes its trait bounds as
dictionaries (tuples of method code pointers). Until 2026-11 it also took
its type arguments boxed; now they cross in their own word (layout.md,
"No boxes at generic edges"). In `HashMap<int, int>` and `sort(Vec<int>)`
that meant a box per argument and a fresh dictionary tuple on every call.
Three changes take the dictionary cost away for ground calls.

An element-wise operator whose element is a type parameter (`[T; N] + [T; N]`,
`-Vec<T>`, `(T, T) * T`, #2398) follows the same split. A clone names the
element's word and lowers primitive ops or the packed kernel
(`query::aggregate_info`); at a user type it calls the type's instance
method once per element (`Lower::inst_elem`, #2414). An element is its
boxed word, as an array holds it: where the instance's entry does not take
and return that (a two-word class's result is two words), the clone calls
the method's `$dict` adapter, the entry a dictionary holds, directly
(#2418). The shared body calls the bound's dictionary
method once per element (`Lower::bound_elem`), whatever the length: the
packed kernels take primitive words only, so `emit_hir_mir` plans one only
for a ground `int` / `float` / `byte` element.

A bound met at the aggregate itself (`add<T: Num>(T a, T b)` called at
`(int, int)` or `[float; 3]`) is the aggregate's structural instance
(`Checker::ensure_structural_arith`, #2513): `Add` .. `Pow` and `Neg`
(with `Num`, all seven) at a ground homogeneous tuple or array whose
element has them, each method a thunk that runs the element-wise
operator at that type (`emit_structural_thunks`). A clone still lowers
the operator in place; a shared body gets the tuple's `Num` dictionary.
A nested aggregate (`((int, int), (int, int))`, `[[float; 2]; 2]`) has
the instance when its element has (#2544): the element's structural
instance comes first, and the operator calls its thunk once per element
(the element is one word, a pair boxed), as at a user type. A `Vec` (or
`[T]`) has `Neg` only, its thunk the dynamic loop ground `-v` runs
(`aggregate_dynamic`): ground `v + v` does not zip two dynamic lengths
(E0119), so neither `Num` nor `Add` .. `Pow` is met at a `Vec`, and
`add<T: Num>(v, v)` is the checker's "No instance for `Num<Vec<int>>`"
(#2564).

## Ground dictionaries once (#1417)

A dictionary made only of method pointers (and such dictionaries) is built
once, on first use, into a synthetic static `__dict$N` (one per distinct
dictionary) and loaded after that: load, compare with 0, build and store.
It is built lazily because a worker can start with zeroed statics
(`machine/src/worker.rs`).

`intern_dict_ops` (`codegen/emit_hir_mir.rs`) replaces each maximal ground
`Tuple` subtree with `HirDictOp::Interned`, and `interned_dict`
(`mir/from_hir.rs`) lowers that to the load, compare and store.

Bench: `examples/perf/dict_once.coil`.

## Clones call their siblings' clones (#1419)

A clone (`Bag::twice$inst$int`) has no dictionary slots. A bounded sibling
call in its generic body (`self.add(..)`) forwarded a dictionary, which the
clone dropped. In a clone such a call is a ground call site: `self` takes the
clone's type (`hir_clone_self_ty`), the callee's parameters bind from it, and
the call goes to the sibling's clone (`hir_clone_drops_forwarded`).

Test: `tests/generics/method_clone_sibling_dicts.coil`.

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
- **Cross-module inlining.** At `-O2` with heap inlining on, the module is
  compiled in two halves (`compile_prepared_module_begin` / `_finish`).
  After the first half, `queue_early_foreign_clones` requests the method
  clones its plain bodies call and the pipeline compiles them before the
  module's bodies are lowered. A clone that plans keeps its HIR body in
  `hir_resolved_clones` (`hir_resolved_clone_body`: names qualified, types
  closed, its calls turned into `Callee::Resolved`, whose plans live in
  `resolved_plans`). `hir_inline_round` then splices a kept body like a
  same-module callee when the call needs no boxing, keeps the clone's ABI
  and fits `max_cross_module_inline_cost` (100: every node 1, a call 25)
  (`examples/perf/foreign_clone_hot.coil`).
- **Free functions.** A generic free function is a clone source like a
  static method with its own type parameters (`note_fn_clone_src`). Its own
  module's calls keep their mono clones; calls from another module or from
  a clone use the clone, so `sort`'s call to `merge_range` is cloned too.
- **Gates.** A clone falls back to the shared body (through a boxing thunk)
  when its types mean something else to the old module (`knows_tys`: a
  class declared later is not in its checker), or when the generic body has
  an enum over its type parameters (`layout_safe`: `Option<T>` in a shared
  field would be stored in the clone's layout and read in the shared one,
  as `task::Channel` did). A generic enum's method, and a generic whose
  signature holds a nested open enum (clone-only, `fn_sig_split`), are
  compiled from their body anyway: no ground call of the latter reaches
  its shared body, so nothing reads its values in the other layout
  ([layout.md](layout.md), "Open enums", #2547).

Tests: `perf_metrics::foreign_method_clone_is_specialized`,
`perf_metrics::foreign_fn_clone_is_specialized`.

## How many clones

A ground call clones, at any number of types, with two exceptions that
take the shared body instead (warning E0804):

- **Polymorphic recursion.** Each clone carries the functions on its
  chain of clones (the clone whose body asked for it, that clone's, …). A
  call to one of them at new types (`f<T>` calling `g<(T, int)>` calling
  `f<((T, int), int)>`, `f<T>` calling `f<Box<T>>`, or `Nest<T>::depth`
  calling `Nest<Nest<T>>::depth`) would grow without end, so it keeps the
  shared body. A free function with a declared signature calls itself at
  that scheme, as any later caller does, so its body may call itself at
  another type (#2540). Chains are no longer than the number of generic functions, so
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

2026-10-17, on the bytecode VM (since deleted), one run each,
`--auto-par off`, `vm_profile` builds (counting
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

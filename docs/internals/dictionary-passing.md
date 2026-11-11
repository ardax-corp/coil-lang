# Dictionary passing

The calling convention for trait-bounded generic code that is not monomorphized. Moved here from the website's types reference (#1880), which keeps a user-level summary.

Constrained calls that are not monomorphized append one dictionary per trait constraint after the value arguments. Each dictionary is a tuple of method function values (body identities) in declaration order. The callee reserves trailing locals `__dict0`, `__dict1`, …, loads the matching method slot with `Index`, and invokes it with `CallIndirect`. A generic calling another generic with the same open bound forwards its existing dictionary, and so does a generic calling itself: a function whose signature is declared calls itself at that scheme, so the checker discharges the call's bounds as for any callee (#2540). Builtin classes use compiler-generated primitive method thunks through this same ABI; ground monomorphization remains an optimization.

## Shape

A dictionary of instance `I` of class `S` at `X` is one tuple (`plan_instance_dict_steps`, `compiler/src/codegen/compiler.rs`):

```
(methods.., kinds, context.., superclass dictionaries..)
```

- **methods**: `S`'s flattened methods (`TypeClassDef::flattened_methods`): its own, then each superclass's, in declaration order. A slot holds a code pointer: the method's `$dict` adapter, its per-type clone's adapter for a ground goal within the clone budget (past it too for a superclass method that reads its dictionary), or a forwarder (below).
- **kinds** (#2506): one word, bit `i` set when the goal's type `i` is a copy-on-write object, then a bit per type variable of `I`'s head in order of appearance, bound from the goal; [`DICT_BITS`](../../compiler/src/codegen/kinds.rs) wide, with a sentinel above. A default body and a generic-head instance's method read it at entry ([cow-calls.md](cow-calls.md)), so such a method is called with its dictionary on every path (a direct call, a `for` loop's `into_iter` / `next`), and as a superclass's method it is reached through a forwarder or its clone, as a dictionary-reading one is. At an open goal the bits are the building body's own (`KindsBit`).
- **context**: one dictionary per bound of `I`'s head (`impl Show for Box<T: Show>` holds `Show<T>`), instantiated for the goal. `I`'s methods read them at those indices at entry.
- **superclass dictionaries** (#2476): one `C<X>` dictionary, planned like any other, for each superclass `C` with a slot that holds a forwarder (in a niche-head instance's entry dictionary, `C`'s entry dictionary, [generic-instances.md](generic-instances.md)). Empty for every other dictionary, so an instance with no context and no dictionary-reading superclass method keeps the two-part shape (every `int` / `float` / `string` one).

A bound call does not know which kind of slot it calls: it loads the slot and calls it with the arguments, the whole dictionary, then the dictionaries of the method's own bounds (#1471), then, for a method with type parameters of its own, their `kinds` word (#2506). Every entry of such a method takes that last word: its shared entry, its clones (after their arguments: a clone takes no dictionary), its `$dict` adapter and its forwarders.

## A generic function's value

A generic function's value (`PolyFn`, `MakePolyFnCapture`) captures one word per constraint of its function, in the function's order: a dictionary built where the value is made (a ground goal, or one over the making body's own type parameters, looked up by goal as a direct call there does, #2639), or, for a constraint open there (the value is an argument a `forall` parameter takes, or a local bound from the function), `common::dict_from_call(j)`: the call's dictionary `j` (`machine::apply`). A call through such a value passes the dictionaries of the constraints of the type it sees (the `forall`'s, or the function's own for a local) in `kinds::value_constraint_order`: by the places of their type variables in order of first appearance in that type, then by class (#2636). Where the value is made, the checker's record of the `forall` type it is passed to (`Checker::rank_n_arg_at`) gives each open constraint of the function the `forall` constraint it is, and so `j`. The order does not depend on the order either type lists its constraints in, so a value passed on to a `forall` that lists them in another order gets the right ones; a `forall` may have constraints the function does not. A local bound from a generic function and passed to a `forall` parameter is made again there for that `forall`; a `forall` parameter passed on to a less general `forall` is refused (E0801 `forall-relay`, #2637). The `kinds` word follows the same order and travels the same way ([cow-calls.md](cow-calls.md)).

## Forwarders

A superclass's method called with the subclass's dictionary would read the wrong one where it reads its dictionary at all: a default body calls its siblings at its own trait's slot numbers, and a bounded instance's method unpacks its context after its own trait's methods. At a ground goal such a slot takes the method's clone past the clone budget too, a direct call. Where no clone fills it (an open goal, a method on its own clone chain), it holds a forwarder (`Compiler::dict_forwarder`, emitted by `emit_dict_forwarder` with the other reserved thunks):

```
C__m$fwd$<index>$<slot>(args.., D, own..) = D[index][slot](args.., D[index], own..)
```

`index` is where `C`'s dictionary sits in `D`, `slot` the method's slot in it. The forwarder moves its words and never reads them (a scalar parameter keeps its type, anything else is an opaque word, as in a shared generic body), so one forwarder serves every instance whose dictionary has `C`'s at that index. Its one call is a `HirBoundCall` with `forward` set (`mir/from_hir.rs`). The cost is one more indirect call for such a method on the shared path, plus the shared method's own dictionary reads (`examples/perf/dict_super_forward.coil` under `--clone-budget 0`: 310 M `instructions:u` with the clone, 749 M through the forwarder); a method that reads no dictionary is in the slot itself.

## A superclass's dictionary from a bound

A shared body with `T: Ordered` may need `Equal<T>` (the context of `Equal<Cell<T>>`, say). The bound's dictionary `D` holds `Equal`'s methods but is no `Equal` dictionary, so one is projected from it (`plan_super_projection`):

```
(f.., D)        f_i(args.., E) = D[slot of method i in D](args.., D),  D = E[n]
```

a forwarder per method of the superclass, then `D` itself. `Eq` from an `Ord` or `Num` bound is built the same way (`VarEqDict::Project`, `eq_glue.rs`): `(eq, ne, D)`.

Tests: `tests/traits/superclass_dict_methods.coil`, `superclass_dict_open_goal.coil` and their `_shared` twins.

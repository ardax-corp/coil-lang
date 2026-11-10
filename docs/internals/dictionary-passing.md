# Dictionary passing

The calling convention for trait-bounded generic code that is not monomorphized. Moved here from the website's types reference (#1880), which keeps a user-level summary.

Constrained calls that are not monomorphized append one dictionary per trait constraint after the value arguments. Each dictionary is a tuple of method function values (body identities) in declaration order. The callee reserves trailing locals `__dict0`, `__dict1`, …, loads the matching method slot with `Index`, and invokes it with `CallIndirect`. A generic calling another generic with the same open bound forwards its existing dictionary. Builtin classes use compiler-generated primitive method thunks through this same ABI; ground monomorphization remains an optimization.

## Shape

A dictionary of instance `I` of class `S` at `X` is one tuple (`plan_instance_dict_steps`, `compiler/src/codegen/compiler.rs`):

```
(methods.., context.., superclass dictionaries..)
```

- **methods**: `S`'s flattened methods (`TypeClassDef::flattened_methods`): its own, then each superclass's, in declaration order. A slot holds a code pointer: the method's `$dict` adapter, its per-type clone's adapter for a ground goal within the clone budget, or a forwarder (below).
- **context**: one dictionary per bound of `I`'s head (`impl Show for Box<T: Show>` holds `Show<T>`), instantiated for the goal. `I`'s methods read them at those indices at entry.
- **superclass dictionaries** (#2476): one `C<X>` dictionary, planned like any other, for each superclass `C` with a slot that holds a forwarder. Empty for every other dictionary, so an instance with no context and no dictionary-reading superclass method keeps the two-part shape (every `int` / `float` / `string` one).

A bound call does not know which kind of slot it calls: it loads the slot and calls it with the arguments, the whole dictionary, then the dictionaries of the method's own bounds (#1471).

## Forwarders

A superclass's method called with the subclass's dictionary would read the wrong one where it reads its dictionary at all: a default body calls its siblings at its own trait's slot numbers, and a bounded instance's method unpacks its context after its own trait's methods. Such a slot, where no clone fills it, holds a forwarder (`Compiler::dict_forwarder`, emitted by `emit_dict_forwarder` with the other reserved thunks):

```
C__m$fwd$<index>$<slot>(args.., D, own..) = D[index][slot](args.., D[index], own..)
```

`index` is where `C`'s dictionary sits in `D`, `slot` the method's slot in it. The forwarder moves its words and never reads them (a scalar parameter keeps its type, anything else is an opaque word, as in a shared generic body), so one forwarder serves every instance whose dictionary has `C`'s at that index. Its one call is a `HirBoundCall` with `forward` set (`mir/from_hir.rs`). The cost is one more indirect call for such a method on the shared path; a method that reads no dictionary is in the slot itself.

## A superclass's dictionary from a bound

A shared body with `T: Ordered` may need `Equal<T>` (the context of `Equal<Cell<T>>`, say). The bound's dictionary `D` holds `Equal`'s methods but is no `Equal` dictionary, so one is projected from it (`plan_super_projection`):

```
(f.., D)        f_i(args.., E) = D[slot of method i in D](args.., D),  D = E[n]
```

a forwarder per method of the superclass, then `D` itself. `Eq` from an `Ord` or `Num` bound is built the same way (`VarEqDict::Project`, `eq_glue.rs`): `(eq, ne, D)`.

Tests: `tests/traits/superclass_dict_methods.coil`, `superclass_dict_open_goal.coil` and their `_shared` twins.

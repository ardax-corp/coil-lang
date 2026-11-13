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

A generic function's value (`PolyFn`, `MakePolyFnCapture`) captures one word per constraint of its function, in the function's order: a dictionary built where the value is made (a ground goal, or one over the making body's own type parameters, looked up by goal as a direct call there does, #2639), or, for a constraint open there (the value is an argument a `forall` parameter takes, or a local bound from the function), `common::dict_from_call(j)`: the call's dictionary `j` (`machine::apply`). A call through such a value passes the dictionaries of the constraints of the type it sees (the `forall`'s, or the function's own for a local) in `kinds::value_constraint_order`: by the places of their type variables in order of first appearance in that type, then by class (#2636), each built by its goal at the call's types as a direct call there would be: a goal on the calling body's own type parameter (`let f = peek; f(x)` in `fn outer<T: Bump>`, a `forall` parameter called at the body's `V`) is that body's bound, or in a per-type clone the clone's type (#2652). Where the value is made, the checker's record of the `forall` type it is passed to (`Checker::rank_n_arg_at`) gives each open constraint of the function the `forall` constraint it is, and so `j`. The order does not depend on the order either type lists its constraints in, so a value passed on to a `forall` that lists them in another order gets the right ones; a `forall` may have constraints the function does not. A local bound from a generic function and read as a value anywhere but as a call's callee (passed to a `forall` parameter, returned, stored in a field, `Vec`, array, tuple, record or `Option`, passed to a function-typed parameter, sent, yielded, copied to another local) is made again at that read, at the type it is read at (for that `forall`), as the function's name there would be (#2637, #2658); a lambda that captures it captures the value made at the one type the lambda reads it at (E0801 `polyfn-capture` at several): in a per-type clone its adapter at the ground type, in a shared body the value at the body's own open types, which takes and returns the open `Option` / `Result` objects the lambda, a part of that body, reads (#2770); a `forall` parameter passed on to a less general `forall` is refused (E0801 `forall-relay`, #2637). The `kinds` word follows the same order and travels the same way ([cow-calls.md](cow-calls.md)).

Read at a ground function type rather than a `forall` one (a parameter, local, field, `Vec` element or result of that type), a generic function whose signature is not plain words (`classify::plain_sig`: an open `Option` / `Result` in a parameter or the result) is not a `PolyFn` but its adapter at those types, a ground function calling it there, which takes no dictionaries: its call builds them as any ground call does ([layout.md](layout.md), "Shared generic bodies", #2755).

Only a `let` with no annotation, or one annotated with a `forall` type, keeps a generic function's scheme (`let f = wrap;` calls `f(1)` and `f("a")`), and only such a local is a `PolyFn` binding. An annotation of a ground function type fixes the local's type as any annotation does: `let g: fn(int) -> Option<int> = wrap;` is `wrap` at `int`, `g("x")` is E0102, and the local holds the value read at that type above, so it is copied, re-assigned, passed on and captured as a plain closure word (#2760).

A class field of a `forall` type (`g: forall T. T -> T`) takes its value as a `forall` parameter does (#2725): `new C(id)` and a store `c.g = id` check the value against the field's type (`Checker::check_rank_n_argument`) and record it (`rank_n_arg_at`), so the value is made for that `forall`, and the field holds that one `PolyFn` word (`classify::closed_field`; the collector resolves its kind). A local of a `forall` type is a `PolyFn` whatever it was read from (`query::polyfn_callee`), so `let f: forall T. T -> T = c.g;` and passing `c.g` to a `forall` parameter call it as such. So is the field read itself where it is called in place, `(c.g)(x)` or `c.g(x)` (#2762): the call is a `forall` parameter's, with the `forall`'s dictionaries and `kinds` word and its result converted as below, the callee's own type the field's (`query::polyfn_callee_ty`). A class `static` or module `static let` of a `forall` type is the same: its initializer is checked as a `forall` argument, and `K::g(x)` / `G(x)` calls it so.

A `forall` parameter's call runs the function's shared body, so a result that holds the `forall`'s type parameters (`forall T. T -> Option<T>`, `T -> (T, T)`, `T -> T -> Vec<T>`, `A -> B -> (B, A)`) comes back in the shared body's layout. The call converts it to its own types' as a direct call of a shared generic does (`Compiler::hir_forall_ret_conv`, `HirPolyCall::boxed_ret` / `ret_items`, #2723): an open `Option` / `Result` at a niche type from its enum object to the niche word, a tuple or enum holding a bare parameter's value rebuilt with the ground types' word kinds, a `Vec` or array of them (one word per element) as it is. Still refused (E0801 `callee-value`): a parameter that holds a type parameter below its top (`forall T. Vec<T> -> T`), a function result, and an open enum below the result's top that is a niche word at the call (`T -> Vec<Option<T>>` at `string`, #2441).

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

## Comparing trait objects (#2740)

A bare-trait existential is a pack `(value, dictionary)`. `==` / `!=` on two of them need an `Eq` the trait gives its values, a superclass of its parameter (`trait Tr<S: Eq>`, or `Ord`): without one it is E0119, as on a type parameter without the bound. With one, the checker records the slot of `eq` .. `ge` in the trait's flattened methods (`ExistentialMethodCall`), the planner makes it `HirOp::Existential`, and MIR (`Lower::existential_cmp`) calls that slot of the left pack's dictionary on the two packed values when both packs hold the same dictionary, that is the same instance's values. Packs of two instances are never equal (`==` false, `!=` true), and an order between them panics (`` `<` on `Tr` values of different types ``). The test is the dictionary's identity: two packs of one type made where the dictionary is not the same object (an open goal's dictionary, built at each use; another worker's interned static) compare unequal.

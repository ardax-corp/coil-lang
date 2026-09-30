# Trait instances for generic types (plan)

Follow-up to #537's known gap: "Derives on generic types are still refused". Status: **plan**, not implemented.

A derive on a generic type is refused because there is nothing for it to expand to. A trait cannot be implemented for a generic type at all, derived or by hand. The target is:

```hy
class Box<T> {
    pub item: T,
}

impl Show for Box<T: Show> {                 // bound on the instance's parameter
    fn show() -> string {
        return format("Box(%s)", self.item.show());
    }
}

#[derive(Show, Eq)]                          // expands to the same shape
enum Tree<T> {
    Leaf(T),
    Node(Tree<T>, Tree<T>),
}

let b = new Box(new Box(3));
b.show();                                    // Show for Box<Box<int>> via Show for int
fn log<T: Show>(T x) { … }
log(b);                                      // dictionary built from the instance's context
```

## Spelling

Inherent impls already declare their parameters inline in the head, `impl Store<T: Eq> { … }`. The trait form follows that: `impl Trait for Type<T: Bound, U>`. There is no `impl<T>` prefix, so each construct keeps one spelling. The `<T: Bound>` list after the type name is the instance's parameter list, not a type application. A bare `impl Trait for Box<T>` is the unbounded case. A bare single uppercase letter in that position is a parameter, as it is for inherent impls. Anything else there (`Box<int>`, `Box<Point>`) stays a concrete instance.

## Where things stand

These were found on `main` after #537.

| # | Piece | Today |
|---|-------|-------|
| 1 | Parser | `trait_impl_for_block` parses the `for` type with `type_annotation`, which does not accept `T: Bound` ("expected `:`"). `impl_block` (inherent) uses `type_param_list` and does accept it. |
| 2 | Instance head | `pre_register_typeclass_impls` builds `InstanceDef.args` with `ast_instance_head_ty`, which turns `T` into `Ty::Con("T")`, a nominal type named `T`. So `impl Show for Box<T>` registers `Show<Box<T>>` for a type `T` that does not exist. |
| 3 | Method bodies | The body of `impl Describe for Box<T>` does not bind `self` (E0100 "Cannot find value `self`"), and field access on it fails. Concrete instances bind it. |
| 4 | Instance lookup | `find_instance` is exact equality. `find_unifying_instance` unifies but ignores any context, and nothing emits sub-obligations. |
| 5 | Dictionaries | A call under `T: Show` gets one dictionary per bound from `emit_call_site_dicts`, a tuple of `CodePtr`s in `flattened_methods` order. There is no way to build a dictionary for `Show<Box<int>>` that closes over `Show<int>`. |
| 6 | Bounded inherent impls | `impl Store<T: Eq>` works. Methods get the dictionary ABI, and nested receivers stage correctly (`tests/positive/method_dict_nested.hy`). This is the machinery to reuse for instance methods. |
| 7 | Derives | `attrs::expand_class` / `expand_enum` return early for a generic type with E0119 "Cannot derive traits for generic class `…`; write an explicit `impl`". That covers both built-in and user derives: a user derive is never queued as a `PendingMacro`. |
| 8 | Macro model | `TypeDecl.generics: Vec<Ident>` already exists, and the encoder fills it. Bounds are not in the model. |

## Design

### Checker

1. **Head.** Parse the `for` type's inline parameter list into `type_params` on `TypeClassImpl` (new field, empty for concrete impls). In the pre-pass, map each parameter to a fresh rigid variable, so the head is `Box<'a>`, not `Box<Con("T")>`.
2. **`InstanceDef`.** Add `params: Vec<TyVar>` and `context: Vec<(String, Ty)>` (for example `[("Show", 'a)]`). Concrete instances have both empty and behave exactly as today.
3. **Lookup.** `find_instance_relaxed` is used for a ground goal `Show<Box<Box<int>>>`. Unify with the head, apply the substitution to the context, and resolve each context goal recursively. The result is a derivation tree, `Show<Box<Box<int>>> ← Show<Box<int>> ← Show<int>`, not a single `InstanceDef`. A context goal with no instance is an error at the use site: "no `Show` for `Point` (needed by `Show for Box<T: Show>`)".
4. **Coherence.** Two instances whose heads unify are overlapping, as today. `find_overlapping_instance` already unifies, so it has to treat the rigid variables as flexible for this check. Otherwise `Show for Box<T>` and `Show for Box<int>` would both be accepted.
5. **Method bodies.** Check each method like a method of a bounded inherent impl `impl Box<T: Show>`: `self: Box<'a>`, and the context in scope as the method's bounds. Then `self.item.show()` resolves through the bound path (`bound_method_candidates`).

### Codegen

6. **Instance method bodies** compile exactly like bounded inherent methods: a shared body taking one dictionary per context entry, in declaration order. Monomorphized bodies (`specialization_for_call`) need no dictionary.
7. **Ground calls.** `b.show()` with `b: Box<Box<int>>` is a direct `CALL` to `Show__Box_T__show`, plus the dictionaries for its context: `Show<Box<int>>` (itself built from `Show<int>`). Build these the same way `emit_call_site_dicts` does, recursing on the derivation tree.
8. **Dictionaries for a generic instance.** A `Show<Box<int>>` dictionary is a tuple of `CodePtr`s, but each method needs the inner `Show<int>` dictionary as well. Two options:
   - **(a) Closures.** Each slot is a closure over the context dictionaries. This keeps the dictionary ABI exactly as it is (`LOAD dict; CONST slot; Index; CallIndirect`), and the closure pushes the captured dictionaries. It costs an allocation per built dictionary.
   - **(b) Nested tuples.** A dictionary is `(methods…, ctx_dict_0, …)`, and the bound-call lowering passes the tail entries as extra arguments. Nothing is allocated, but every bound-call site changes.

   Recommendation: (a), with dictionaries for ground goals interned as statics (one per distinct derivation, built at first use), so a hot loop allocates nothing. Revisit (b) only if a hit bench shows the `CallIndirect` through a closure matters.
9. **Layouts.** A method returning `Option<T>` / `Result<T, E>` crosses the generic boundary in the boxed layout. The #528 adapter thunks apply unchanged.

### Derives

10. **Built-in derives** (`synth_*_class` / `synth_*_enum`) take the type parameters and emit `impl Trait for Name<T: Trait, …>`. Each parameter gets a `Trait` bound, the same rule Rust's `derive` uses (it over-constrains phantom parameters, which is acceptable here). `Default` and `Deserialize` depend on #524's static methods and stay refused on generic types until that lands.
11. **User derives** get `TypeDecl.generics` (already there) and a new `t.impl_head(trait_name)` helper in `macro.hy`, which returns `Name<T: Trait, U: Trait>` as `Code`, so a macro writes `impl ${trait} for ${t.impl_head("Summary")}`. Drop the early return in `expand_class` / `expand_enum`.

## Phases

One PR each, in order.

1. **Parser + head + bodies.** `TypeClassImpl.type_params`, rigid parameters in the head, `self` and bounds in method bodies, overlap check with flexible parameters, `coil fmt` round trip. Unbounded instances work end to end (`impl Show for Box<T>` whose body does not call `T`'s methods). Tests: `tests/positive/generic_instance_unbounded.hy` plus `compile_fail` for overlap and for a missing context instance.
2. **Contexts and dictionaries.** The derivation tree, ground calls with context dictionaries, dictionaries for generic instances (8a) interned for ground goals, and bound calls that land on a generic instance (`fn log<T: Show>(T x)` called with a `Box<int>`). Tests cover a nested `Box<Box<int>>`, a two-parameter instance, and a recursive enum (`Tree<T>`). Hit bench: `examples/perf/generic_instance_show.hy`, with the flagships as controls.
3. **Built-in derives.** Show, Eq, Ord, Hash, String, Serialize, Send / Sensitive on generic classes and enums. Default and Deserialize stay refused, with a message that points at #524.
4. **User derives.** `impl_head`, remove the refusal, and add a `derive_macro_generic.hy` test. Update `limitations.md`, `macros.md` and the coil-website docs (`references/types.md`, macros page).

## Open questions

- **Bound on the instance vs. on the method.** Phase 1 puts the bound on the instance head only. A method-level bound (`fn eq_by<U: Eq>(…)`) inside a generic instance already works for inherent methods (#523) and should carry over, but it is untested here.
- **Superclass contexts.** `impl Ord for Box<T: Ord>` needs `Eq<Box<T>>` if `Ord` requires `Eq`. The derivation tree handles it if the superclass goal is added to the context. Confirm how `TypeClassDef` records superclasses before phase 2.
- **Orphan rule.** None today for concrete instances. Generic instances make overlap across modules more likely. Keep the current behavior (overlap is an error when both are in the compile) and revisit if packages collide.

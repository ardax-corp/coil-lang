# Trait instances for generic types

GitHub: [#550](https://github.com/ardax-corp/coil-lang/issues/550) (unbounded), [#551](https://github.com/ardax-corp/coil-lang/issues/551) (bounded), [#552](https://github.com/ardax-corp/coil-lang/issues/552) (derives). Status: #550 and #551 **implemented**; #552 open.

```hy
class Box<T> {
    pub item: T,
}

impl Describe for Box<T: Describe> {         // bound on the instance's parameter
    pub fn describe(Box<T> b) -> string {
        return "Box(" + b.item.describe() + ")";
    }
}

new Box(new Box(3)).describe();              // Describe<Box<Box<int>>> ← Describe<Box<int>> ← Describe<int>
fn log<T: Describe>(T x) { … }
log(new Box(3));                             // dictionary built from the instance and its context
```

Tests: `tests/positive/generic_trait_instance.hy`, `tests/positive/bounded_generic_instance.hy`, `tests/compile_fail/generic_instance_overlap.hy`, `tests/compile_fail/bounded_instance_*.hy`; parser: `typeclass_impl_bounded_generic_head`.

## Spelling

One spelling, the one inherent impls use: `impl Trait for Type<T: Bound, U>`. There is no `impl<T>` prefix.

- **Unbounded** (`impl Show for Box<T>`): the head is an ordinary type annotation. Its bare single uppercase letters are the instance's type parameters, the rule inherent `impl Cell<T>` uses. `Box<int>` / `Box<Point>` stay concrete instances.
- **Bounded** (`impl Show for Box<T: Show>`): when any parameter carries a bound, the parser reads the head as a parameter list (`TypeClassImpl.type_params`) and the `for` type becomes `Box<T>`. `coil fmt` and `Display` print it back as written.

Instance methods keep the explicit receiver parameter every trait method has (`fn describe(Box<T> b)`).

## Checker

- **Head.** The impl pre-pass (`pre_register_typeclass_impls`) and `infer_typeclass_impl` map the head's parameters to type variables and scope them over the head and every method, so a method's `Box<T>` parameter and `b.item: T` line up. The codegen FQN names the parameters (`Describe__Box<T>__describe`), in the checker (`instance_head_fqn_part`) and in codegen alike.
- **Context.** `InstanceDef.context` holds the bounds over the head's variables (`[Describe<'t>]`); concrete and unbounded instances leave it empty.
- **Lookup.** `find_unique_instance` instantiates a generic instance's variables afresh for every goal and unifies them *toward* the goal (the fresh variables are bound, never the goal's own), so repeated lookups do not drift a goal's type parameters. An open goal (`Show<β>`) never selects a generic instance. The matched instance is recorded at the goal's types (`args` and `context` substituted), so codegen always sees the goal.
- **Context check.** `check_instance_context` resolves each context goal after a generic instance is discharged. A ground goal resolves recursively (`Show<Box<Box<int>>>` ← `Show<Box<int>>` ← `Show<int>`); a missing one is "No instance for `Describe<Point>` (needed by `Describe<Box<Point>>`)". A goal on a type parameter must be covered by an active bound, else the error has the help "add a `Describe` bound to the type parameter".
- **Method bodies.** Inside a bounded instance the trait's own constraint (`Describe<Box<T>>`) is active as dictionary 0 and the context follows (`Describe<T>` is 1), so `b.item.describe()` is an ordinary bound call through `__dict1`.
- **Coherence.** Two instances whose heads unify overlap, as before: `Box<T>` and `Box<int>` are an error.

## Codegen

- **Dictionary layout.** A bounded instance's dictionary is `(method code pointers…, context dictionaries…)`: `emit_instance_dict` appends the context dictionaries, instantiated for the goal, after the flattened methods. This is the plan's "nested tuple" layout with the existing `LOAD dict; CONST slot; Index; CallIndirect` ABI unchanged: a bound call still passes the dictionary as the trailing argument.
- **Method prologue.** A bounded instance's method takes the instance dictionary as its trailing argument (`__dict0`) and unpacks the context into `__dict1..` (`pending_instance_ctx` in `compile_function_output_with_name`).
- **Direct calls.** Every direct call to an instance method (receiver, function-style, `Owner::m`, mono clone, operator) passes the dictionary when the target is a default body or a bounded instance (`emit_call_instance_dict`). If the dictionary cannot be built there, that is a compile error, not a call without one.
- **Open goals.** A goal that still mentions a type variable is served from scope (`resolve_open_dict_goal`): inside a bounded instance, its own dictionary (`__dict0`, for recursion such as `Tree<T>`) or a context slot; inside a generic function, its bound's `__dictN` (scheme order); in a mono clone, the goal at the clone's concrete types (`mono_var_tys`). A goal like `Describe<Pair<T, int>>` goes through the generic `Pair` instance and resolves its context from scope. An open goal never falls back to a concrete instance.
- **Allocation.** Building a bounded instance's dictionary allocates one tuple per level at each use. Interning dictionaries for ground goals is a possible follow-up if a hit bench shows it matters.

## Not done

- **Derives on generic types (#552).** Built-in derives expand to `impl Trait for Name<T: Trait, …>` with a `Trait` bound on each parameter; user derives get a `t.impl_head(trait)` helper. `Default` needs static trait methods (#524, done).
- **Superclass contexts.** `impl Ord for Box<T: Ord>` needs `Eq<Box<T>>` when `Ord` requires `Eq`; the superclass instance must exist, as for concrete instances.

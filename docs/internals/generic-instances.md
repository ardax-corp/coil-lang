# Trait instances for generic types

GitHub: [#550](https://github.com/ardax-corp/coil-lang/issues/550) (unbounded), [#551](https://github.com/ardax-corp/coil-lang/issues/551) (bounded), [#552](https://github.com/ardax-corp/coil-lang/issues/552) (derives). Status: **implemented** (#550, #551, #552).

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

Tests: `tests/traits/generic_trait_instance.hy`, `tests/traits/bounded_generic_instance.hy`, `tests/traits/compile_fail/generic_instance_overlap.hy`, `tests/traits/compile_fail/bounded_instance_*.hy`; parser: `typeclass_impl_bounded_generic_head`.

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

- **Dictionary layout.** A bounded instance's dictionary is `(method code pointers…, context dictionaries…)`: `plan_instance_dict` appends the context dictionaries, instantiated for the goal, after the flattened methods. This is the plan's "nested tuple" layout with the existing `LOAD dict; CONST slot; Index; CallIndirect` ABI unchanged: a bound call still passes the dictionary as the trailing argument.
- **Method prologue.** A bounded instance's method takes the instance dictionary as its trailing argument (`__dict0`) and unpacks the context into `__dict1..` (`pending_instance_ctx` in `compile_function_output_with_name`).
- **Direct calls.** Every direct call to an instance method (receiver, function-style, `Owner::m`, mono clone, operator) passes the dictionary when the target is a default body or a bounded instance (`plan_call_instance_dict`). If the dictionary cannot be built there, that is a compile error, not a call without one.
- **A method's own bounds (#1471).** A trait method's bounds on its own type parameters (`fn third<W: Show>`) are in its trait scheme after the trait's constraint, so a call discharges them. In the body they are numbered after the trait's own dictionary and the context ones (a context-free instance reserves dictionary 0 with a placeholder constraint no goal matches), and they are parameters after `__dict0` (`pending_own_dicts`), which such a method then always takes: it is never raw and never cloned. Every call (direct, function-style, through a bound's dictionary, the `$dict` adapter) passes them after the instance's dictionary (`HirCall::own_dicts`): a ground goal's instance dictionary, or the frame dictionary the checker forwards for an open one.
- **Open goals.** A goal that still mentions a type variable is served from scope (`resolve_open_dict_goal`): inside a bounded instance, its own dictionary (`__dict0`, for recursion such as `Tree<T>`) or a context slot; inside a generic function, its bound's `__dictN` (scheme order); in a mono clone, the goal at the clone's concrete types (`mono_var_tys`). A goal like `Describe<Pair<T, int>>` goes through the generic `Pair` instance and resolves its context from scope. An open goal never falls back to a concrete instance.
- **One plan.** Every dictionary is planned once as a `DictStep` stack
  program (code pointer, frame `__dictN`, null, tuple, box) from the goal's
  types. Direct MIR runs it as a `HirDictOp` program (`hir_dict_ops`); the
  IL thunks (instance-clone and method-clone calls of a shared body) emit
  it (`append_dicts_interned`).
- **Allocation.** A ground dictionary is built once into a static ([generic-specialization.md](generic-specialization.md)); an open goal's dictionary is still built at each use.

## Derives (#552)

Built-in and user derives on generic types expand to bounded instances: each
type parameter is bounded by the trait the derived body uses (`Show for
Box<T: Show>`, `Ord for Box<T: Ord + Eq>`). `TypeDecl::impl_head(bound)` and
`TypeDecl::self_type()` in the `macro` model write the head and the signature
type; see [macros.md](macros.md). The primitives have built-in `Default`
instances so a derived `Default for Box<T: Default>` applies to `Box<int>`.
A static trait call on a generic owner (`Box::default()`) is `Box<β…>`, fixed
by the expected type. Generic instances are found by the superclass check, the
`%v` check and dispatch, and the operand checks (`find_generic_instance`).

## Not done

- **Superclass contexts.** `impl Ord for Box<T: Ord>` still needs its own
  superclass instances (`Lt<Box<T>>`, …, `Eq<Box<T>>`) declared, as for
  concrete instances; `#[derive(Ord)]` declares the `Lt` … `Ge` ones and
  `Eq` takes its own derive (`#[derive(Eq, Ord)]`, #1716).
- **Dictionary allocation.** A dictionary for an open goal is still built at
  each use; ground ones are interned (#1417).

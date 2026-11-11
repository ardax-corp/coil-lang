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

Tests: `tests/traits/generic_trait_instance.coil`, `tests/traits/bounded_generic_instance.coil`, `tests/traits/compile_fail/generic_instance_overlap.coil`, `tests/traits/compile_fail/bounded_instance_*.coil`; parser: `typeclass_impl_bounded_generic_head`.

## Spelling

One spelling, the one inherent impls use: `impl Trait for Type<T: Bound, U>`. There is no `impl<T>` prefix.

- **Unbounded** (`impl Show for Box<T>`): the head is an ordinary type annotation. Its bare single uppercase letters are the instance's type parameters, the rule inherent `impl Cell<T>` uses. `Box<int>` / `Box<Point>` stay concrete instances.
- **Bounded** (`impl Show for Box<T: Show>`): when any parameter carries a bound, the parser reads the head as a parameter list (`TypeClassImpl.type_params`) and the `for` type becomes `Box<T>`. `coil fmt` and `Display` print it back as written.

Instance methods keep the explicit receiver parameter every trait method has (`fn describe(Box<T> b)`).

## Checker

- **Head.** The impl pre-pass (`pre_register_typeclass_impls`) and `infer_typeclass_impl` map the head's parameters to type variables and scope them over the head and every method, so a method's `Box<T>` parameter and `b.item: T` line up. The codegen FQN names the parameters (`Describe__Box<T>__describe`), in the checker (`instance_head_fqn_part`) and in codegen alike.
- **Stubs.** The pre-pass registers each impl as a stub, so a use earlier in the file finds its instance, and remembers the stub by the impl's module, trait and span (`Checker::impl_stubs`). `infer_typeclass_impl` claims the stub of its own impl by that key, never another impl's of the same trait by its head (#2480: an impl whose pre-pass head differed from the checked one took the next impl's stub and left that impl registered twice, an "Ambiguous instance"). The pre-pass runs again once imported classes are known and rewrites each stub's head in place, so a head naming an imported class (`impl Mine for Cell`, `for Wrap<T>`) or a tuple (`for ()`) is the type the real pass gives it.
- **Context.** `InstanceDef.context` holds the bounds over the head's variables (`[Describe<'t>]`); concrete and unbounded instances leave it empty.
- **Lookup.** `find_unique_instance` instantiates a generic instance's variables afresh for every goal and unifies them *toward* the goal (the fresh variables are bound, never the goal's own), so repeated lookups do not drift a goal's type parameters. An open goal (`Show<β>`) never selects a generic instance. The matched instance is recorded at the goal's types (`args` and `context` substituted), so codegen always sees the goal.
- **Builtin shapes (#2554).** A builtin shape over a type parameter (`Show<Option<T>>`, `Eq<Vec<T>>`, `Hash<(T, int)>`, `Ord<(T, T)>`) has no declared instance. The goal takes the shape's structural instance with open items (`Generics::register_structural_open`, registered the first time a goal asks): its head is the shape over fresh variables (`Option<a>`, `Result<a, b>`, `[a]`, which every fixed array matches), its context one goal per variable (`Show<a>`; `Ord<a>` for a tuple's `Lt` .. `Ge`), so it resolves through the frame's own dictionary as a generic-head instance does. `Show` and `Eq` have one for a tuple, `Option`, `Result`, `Vec` and array, `Hash` for a tuple and array (`Option` / `Result` have prelude instances, a `Vec` has none, as at ground types), `Lt` .. `Ge` / `Ord` for a tuple. A ground goal another instance meets does not take it.
- **Context check.** `check_instance_context` resolves each context goal after a generic instance is discharged. A ground goal resolves recursively (`Show<Box<Box<int>>>` ← `Show<Box<int>>` ← `Show<int>`); a missing one is "No instance for `Describe<Point>` (needed by `Describe<Box<Point>>`)". A goal on a type parameter must be covered by an active bound, else the error has the help "add a `Describe` bound to the type parameter".
- **Method bodies.** Inside a bounded instance the trait's own constraint (`Describe<Box<T>>`) is active as dictionary 0 and the context follows (`Describe<T>` is 1), so `b.item.describe()` is an ordinary bound call through `__dict1`.
- **Coherence.** Two instances whose heads unify overlap, as before: `Box<T>` and `Box<int>` are an error.

## Codegen

- **Dictionary layout.** A bounded instance's dictionary is `(method code pointers…, context dictionaries…)`: `plan_instance_dict` appends the context dictionaries, instantiated for the goal, after the flattened methods. This is the plan's "nested tuple" layout with the existing `LOAD dict; CONST slot; Index; CallIndirect` ABI unchanged: a bound call still passes the dictionary as the trailing argument.
- **Method prologue.** A bounded instance's method takes the instance dictionary as its trailing argument (`__dict0`) and unpacks the context into `__dict1..` (`pending_instance_ctx` in `compile_function_output_with_name`).
- **Direct calls.** Every direct call to an instance method (receiver, `Trait::m`, `Owner::m`, mono clone, operator) passes the dictionary when the target is a default body or a bounded instance (`plan_call_instance_dict`). If the dictionary cannot be built there, that is a compile error, not a call without one.
- **A method's own bounds (#1471).** A trait method's bounds on its own type parameters (`fn third<W: Show>`) are in its trait scheme after the trait's constraint, so a call discharges them. In the body they are numbered after the trait's own dictionary and the context ones (a context-free instance reserves dictionary 0 with a placeholder constraint no goal matches), and they are parameters after `__dict0` (`pending_own_dicts`), which such a method then always takes: it is never raw and never cloned. Every call (direct, trait-qualified, through a bound's dictionary, the `$dict` adapter) passes them after the instance's dictionary (`HirCall::own_dicts`): a ground goal's instance dictionary, or the frame dictionary the checker forwards for an open one.
- **Open goals.** A goal that still mentions a type variable is served from scope (`resolve_open_dict_goal`): inside a bounded instance, its own dictionary (`__dict0`, for recursion such as `Tree<T>`) or a context slot; inside a trait method with bounds of its own, the own bound's slot after those (`Checker::trait_body_dicts`, the frame order the checker numbered the body's goals by: the method's scheme lists its own bounds alone, #2594); inside a generic function, its bound's `__dictN` (scheme order); in a mono clone, the goal at the clone's concrete types (`mono_var_tys`). A goal like `Describe<Pair<T, int>>` goes through the generic `Pair` instance and resolves its context from scope. An open goal never falls back to a concrete instance.
- **Builtin shapes' thunks (#2554).** Each method of a structural instance with open items is one thunk taking the boxed values and the instance dictionary (`emit_structural_open`): `show`, `eq` and `ne` unpack the context into `__dict1..` and show or compare a variable's part through it (`%v`, open `Eq` glue); `hash` and `lt` .. `ge` call the context dictionaries as a forwarder calls its superclass's. In a per-type clone, where the goal is ground, a call takes the ground structural instance instead (`hir_clone_ground_structural`), which takes the value's own word, and so does a `%v` of the shape (`hir_show_ty`, which registers it first, #2613); it is registered at the clone's types when the clone is made (`Checker::ensure_clone_structural_goals`) or when a ground dictionary is planned. A ground structural thunk is emitted after the module's clones, and may ask for one in turn (`Show<Option<Box<int>>>` shows its payload through `Show<Box<int>>`'s clone): the thunks and the clone queue are emitted to a fixed point (`lower_module_bodies`, #2580). A ground tuple's or array's `Hash` takes such an item when the instance's context holds at the item's types (`Checker::ground_instance_holds`): its thunk calls the item's clone, or the shared entry with its dictionary, an item it takes as an open `Option` / `Result` relaid out to the enum object (#2590).
- **One plan.** Every dictionary is planned once as a `DictStep` stack
  program (code pointer, frame `__dictN`, null, tuple, box) from the goal's
  types. Direct MIR runs it as a `HirDictOp` program (`hir_dict_ops`). A
  thunk (an instance-clone or method-clone call of a shared body) is
  built as HIR, its call carrying the plan (`SynthCallee::dicts`).
- **Allocation.** A ground dictionary is built once into a static ([generic-specialization.md](generic-specialization.md)); an open goal's dictionary is still built at each use.

- **Superclass slots (#2476).** A subclass's dictionary (`Ord<X>`, a user `trait Ordered<T: Equal>`) lists its methods and each superclass's, and a bound call passes the whole dictionary to the method it takes from a slot. A superclass method that reads its dictionary (a default body calling a sibling by its own trait's slot number, a bounded instance's method unpacking its context) needs its own instance's dictionary instead. Where its per-type clone is in the slot (a ground goal within the clone budget) it reads none. Otherwise the slot holds a forwarder and the dictionary carries that superclass's own dictionary after its context: shape and forwarders are in [dictionary-passing.md](dictionary-passing.md). One refusal is left (E0801, `dict_unbuilt`): the *entry's own* dictionary of a niche-head instance (`impl Ord for Option<T>`, `plan_entry_dict`) with such a slot, because that dictionary holds the shared entries, which take the value as an enum object, while the superclass's dictionary holds the methods that take the value's own word (#2477).

- **A generic head over `Option` / `Result` at a niche type (#2477).** `impl Sz for Option<T>`'s shared entry holds the value as the enum object; ground code and a bare type parameter hold `Option<string>` as its niche word ([layout.md](layout.md), "Open enums"). So such an instance has two dictionaries at such a type. A *bound's* dictionary (`plan_instance_dict`: a call site's, a context's, an existential's) serves a shared body that holds the value as its own word: each of the instance's methods goes in as its per-type clone's adapter, asked for past `--clone-budget`, and where a method has no clone (its trait signature names `Option<T>`, so `instance_clone` keeps the shared entry) the dictionary is refused with E0801 (`dict_unbuilt`). The *entry's own* dictionary (`plan_entry_dict`: what a direct call passes to the shared entry or to a default body, with the value converted to the object) holds the shared entries, which a default body then calls on that object.

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

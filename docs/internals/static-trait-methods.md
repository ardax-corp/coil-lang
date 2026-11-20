# Static and return-type-dispatched trait methods

GitHub: [#524](https://github.com/ardax-corp/coil-lang/issues/524). Status: **implemented**.

A trait method whose `Self` type appears only in its **return type** is callable. Examples are constructors such as `FromVal::from_val(Val) -> T` and `Default::default() -> T`. This is what typed decode needs in coil-json, coil-toml and coil-msgpack:

```hy
trait FromVal<T> {
    static fn from_val(Val v) -> Result<T, DecodeError> {}
}

impl FromVal for Config {
    pub static fn from_val(Val v) -> Result<Config, DecodeError> { … }
}

let a = Config::from_val(v)?;                     // concrete owner
fn decode_as<T: FromVal>(Val v) -> Result<T, DecodeError> {
    return Result::Ok(T::from_val(v)?);           // bound type parameter
}
let cfg: Config = decode_as(v)?;                  // T chosen by the expected type
```

Runnable example: `examples/static_trait_method.coil`. Tests: `tests/traits/static_trait_methods.coil`, `tests/traits/derive_default_static.coil`, `tests/traits/compile_fail/trait_static_*.coil`, `tests/traits/compile_fail/trait_instance_method_as_static.coil`.

## Surface (one spelling)

- **Declaration:** `static fn name(params) -> R {}` in a `trait` body, the same spelling as an inherent static. An instance implements it with `pub static fn`. Static-ness must match the declaration (error: "method `m` in instance of `Tr` must be static … but it is an instance method", and the converse).
- **Calls:** `Owner::name(args)`, where `Owner` is either
  - a concrete type with an instance (`Config::from_val(v)`, `int::from_val(v)`, `Dir::from_val(v)`), or
  - a type parameter in scope whose bound declares `name` (`T::from_val(v)`), or
  - the trait itself (`FromVal::from_val(v)`), limited to that trait's method: the expected type (or the arguments) choose the instance. See "Trait-qualified calls" below.
- **No bare `from_val(v)`**, a trait's own default body included (E0153, as for any bare trait call): there a sibling is `FromVal::from_val(v)` or `T::from_val(v)`.
- An instance method called as `Owner::m(..)` is an error that points at `obj.m(..)`; a static method called on a value is an ordinary unknown-method error.

## Typechecking

- `TypeClassMethodDef::is_static` records the declaration; `infer_typeclass_impl` checks static-ness per method.
- `Owner::m(args)` parses as `Construct` (or as `Call` with a `QualifiedAccess` name for primitive owners such as `int::m`). Both paths reach `try_infer_trait_static_call` after the enum-variant, static-field and inherent-static lookups miss:
  - **Type parameter in scope:** `bound_method_candidates(m, Some(var))` selects the bound, the scheme's class parameter is unified with the parameter's variable, and a `BoundMethodCall { has_receiver: false, .. }` is recorded (the `Trait::m(args)`-under-a-bound shape).
  - **Concrete owner:** every single-parameter trait that declares `m` and has an instance for the owner is a candidate; more than one is an ambiguity error. The scheme's class parameter is unified with the owner type before discharge, so the constraint resolves to the owner's instance, not to the first instance that unifies.
- **Return-type-directed resolution** (`bind_call_result_to_expected`): before a call's bounds are discharged, a still-open result type is trial-unified with the node's expected type (`expected_here`: `let x: T = f()`, `return f()`, an argument with a known parameter type). `f()?` sets the inner call's expectation to `Result<E, err>` (the fn's error type in result mode). Only a successful unification is committed; the binding site still reports its own mismatch. This applies to generic fn calls, trait method calls (both spellings, through a bound or at a ground instance) and static calls.
- With one matching instance and no expectation the instance is still picked (the choice is forced). With several and no expectation the existing "Ambiguous instance" error stands (`tests/traits/compile_fail/trait_static_uninferable.coil`).

## Codegen

- **Concrete `Owner::m(args)`:** the discharged instance is planned by `plan_call_site_dicts`; the call lowers to a direct MIR `Call` of the instance FQN (arguments boxed or converted at the trait boundary as for any trait method), plus the instance dictionary only when the target is the trait's default body.
- **`T::m(args)` in a shared body:** the method is read out of the dictionary and called with `CallIndirect`, the dictionary trailing (no receiver).
- **`T::m(args)` in a mono clone:** `mono_type_param_tys` maps `T` to its concrete type, so the clone calls that instance directly. The argument-typed lookup cannot see a return-only `T`.
- **Generic function returning `T`:** `plan_call_site_dicts` binds scheme variables from the call's result type; `bind_scheme_vars` also matches `Result<T, E>` / `Option<T>` sums structurally.
- A `Construct`-shaped static call is a call, not a variant: the HIR build checks `tag_for` before treating it as a constructor.

## Library

- `Default::default` is a static trait method. `#[derive(Default)]` generates `static fn default()`; each class field takes its type's default (`0`, `0.0`, `false`, `""`, otherwise `Ty::default()`), and an enum takes its first variant with defaulted payloads.
- The placeholder `Serialize` / `Deserialize` derives were removed earlier; a prelude `Encode` / `Decode` pair is a separate plan that builds on this one.

## Not done

- Generic owners (`Box<int>::from_val(v)`) wait on generic trait instances (#550 / #551).
- Specialization keys are still argument types, so a generic call whose `T` is return-only uses the shared body plus dictionary (correct, not specialized).

## Trait-qualified calls

A trait method has two call spellings: the method form `x.m(rest)` and the trait-qualified `Trait::m(x, rest)`, the receiver (if the method takes one) passed first. `Trait::m(args)` names the trait, so two traits (or a trait and a free fn) sharing a method name are told apart, and it is the spelling for an instance method without a receiver (`Zero::zero()`) as well as for static methods.

A bare `m(args)` is not a trait call. When no free fn, local, builtin or virtual-module fn takes `m` and a trait declares it, it is E0153 (`BareTraitCall`) with both spellings in the help (`a.m(b)` when the method takes a receiver and the receiver's class has no inherent `m`, else only `Trait::m(a, b)`). That holds inside impl bodies (the method being inferred is bound under its bare name for recursion; that binding is not a free fn) and inside a trait's default bodies (its methods' signatures are bound under their bare names there).

- **Typechecking:** both spellings resolve through `resolve_trait_method_call`, the method form with `has_receiver` and no trait filter (`infer_method_trait_call`), the qualified one with `only = Some(Trait)` (`infer_qualified_trait_call`). The steps: a trait-object first argument (`ExistentialMethodCall`); an active bound's dictionary (`bound_method_candidates`, the receiver's variable or, with a concrete first argument, the candidates whose type the arguments fit, #1483); a type parameter without the bound is E0119 naming the missing bound; else the ground instance (`ground_trait_method_for_receiver` for the method form, the trait's scheme for the qualified one) the argument and expected types discharge. Where several traits declare the method's name (two user traits, or one and a builtin: `io`'s `Read::read` / `Write::write`, the prelude's `Length::len`), the method form takes the one with an instance for the receiver's type (`ground_trait_methods_for_receiver`, #2481); several with one are E0119 ("Ambiguous trait method", the help naming each `Trait::m(..)`), none is E0101. Bound and ground calls bind an open result to the expected type with `bind_call_result_to_expected` (commit only on success, the binding site reports its own mismatch), so `let v: Show = s.same()` packs as `Same::same(s)` does. A `readonly` receiver makes the result `readonly` in both spellings unless the method declares `readonly A self` (`method_result`). The method form checks inherent class methods first (an inherent `m` wins over a trait's). `qualified_call_trait` takes the owner as a trait only when it names no type parameter, class, enum or primitive (so `Type::assoc_fn`, `Enum::Variant` and `T::m` keep their meaning); `module::Trait` names a trait that module declares. Errors: an unknown owner is E0202, a method the trait does not declare is E0101, no instance (or more than one with nothing to choose) is E0119.
- **Lowering:** the checker records the callee name in `qualified_trait_call_at`: the bare method, or `Trait::m` when a free fn, local or builtin takes the bare name. `hir_bound_call` / `hir_ground_ufcs` / `hir_ground_bound_call` send the named call to the recorded dictionary or instance as they do `T::m`; the method form lowers through `Callee::Method`. Both reach the same instance entry.
- Tests: `tests/traits/trait_call_spellings.coil`, `tests/traits/qualified_trait_calls.coil`, `tests/traits/compile_fail/qualified_trait_*.coil`, `tests/traits/compile_fail/bare_trait_call*.coil`.

## `Owner::m` without parentheses (#2746)

A call is written with parentheses; a bare `Owner::m` is never a call. The parser keeps the two apart: `Owner::m` is `EnumConstructPayload::Unit`, `Owner::m()` is `EnumConstructPayload::Empty` (`coil fmt` prints each as written). A paren-less `Owner::x` resolves in this order:

1. a static field or `const` (`Counter::count`, `module::NAME`): read it. `Owner::x()` is a call, never a read (#2752): of the function a static field or `const` of function type holds (`K::h()`, `m::F()`, as `K::f(x)` is, #1705), else E0104 (`NotAFunction`).
2. a unit variant (`Shape::Dot`): construct it (`Shape::Dot()` as well).
3. an inherent static method (`Src::inc`, a generic `Src::sid<U>`, a generic class's `Bx::make`, an enum's `Shape::ring`): its function value, as a free function's name is. `infer_construct` records the span in `static_method_value_at` and types it as the method's instantiated scheme. HIR lowers it to `HirKind::Global { name: "Owner::m" }`, the node a free function's name is, and codegen takes the free-function road: `hir_global_fn` (a ground method: `MakeFn` of its entry or its `$value` adapter) or `hir_global_polyfn` (a method with its own type parameters, or any static method of a generic class, whose one shared body takes a `kinds` word: `PolyFn` / `PolyFnCapture`, `hir_mir_polyfn_value`). The escape walk (`fn_value_escape`) poisons a bare lowercase `Owner::m` as it does a `QualifiedAccess`, so the method's parameters keep the one-word ABI a value call passes.

A name that is both a static field and a static method reads the field bare and calls the method with parentheses: a method of the name wins over a field's function (`try_infer_qualified_value_call`).

### A call on a field of function type (#2752)

One rule for instance and static fields: a call on a field that holds a function calls that function, unless the owner has a method of the name, which wins. `o.f(x)` / `o.seven()` with no method `f` / `seven` on `o`'s class (a trait method whose trait has no instance for the receiver is none) calls the field's function, as `(o.f)(x)` does (#2472): `infer_method_call_expr` types it through `infer_field_value_call`, which records the field's type at the call's span (`field_value_call_at`), and HIR lowers it to `Call { callee: Value(Field { .. }) }`, the node `(o.f)(x)` is. `K::h()`, `K::f(x)` and `m::F()` call a static field's or another module's `const`'s function (`qualified_value_call_at`, `HirBuild::qualified_value_call`). A field, static or `const` of another type called is E0104 (`o.n(1)`, `K::n()`, `m::LIMIT()`); a bare `K::n` / `m::LIMIT` reads it.

Tests: `tests/functions/call_fn_field_direct.coil`, `tests/functions/call_fn_field.coil`, `tests/functions/qualified_fn_value_call.coil`, `tests/functions/compile_fail/call_non_function_*.coil`.

A trait's method has no function value of its own: a bare `T::m`, `Point::from_val` or `Trait::m` is E0119 ("names a trait method, which has no function value"), with the call or a lambda in the help. A zero-argument static method's value where its result was expected is the ordinary E0102, its help `add () to call it` (`Checker::uncalled_fn_of`).

Tests: `tests/functions/static_method_value.coil`, `tests/modules/imported_static_method_value.coil`, `tests/functions/compile_fail/static_method_value_not_called.coil`, `tests/traits/compile_fail/trait_method_bare_name.coil`.

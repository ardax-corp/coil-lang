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

Runnable example: `examples/static_trait_method.hy`. Tests: `tests/positive/static_trait_methods.hy`, `tests/positive/derive_default_static.hy`, `tests/compile_fail/trait_static_*.hy`, `tests/compile_fail/trait_instance_method_as_static.hy`.

## Surface (one spelling)

- **Declaration:** `static fn name(params) -> R {}` in a `trait` body, the same spelling as an inherent static. An instance implements it with `pub static fn`. Static-ness must match the declaration (error: "method `m` in instance of `Tr` must be static … but it is an instance method", and the converse).
- **Calls:** `Owner::name(args)`, where `Owner` is either
  - a concrete type with an instance (`Config::from_val(v)`, `int::from_val(v)`, `Dir::from_val(v)`), or
  - a type parameter in scope whose bound declares `name` (`T::from_val(v)`).
- **No bare `from_val(v)`** for static trait methods outside a trait's own default body (there the class constraint is active and the sibling call is the existing UFCS bound call). Nothing else selects an instance except the expected type, and a second spelling for the same call goes against the "one spelling per construct" rule.
- An instance method called as `Owner::m(..)` is an error that points at `obj.m(..)`; a static method called on a value is an ordinary unknown-method error.

## Typechecking

- `TypeClassMethodDef::is_static` records the declaration; `infer_typeclass_impl` checks static-ness per method.
- `Owner::m(args)` parses as `Construct` (or as `Call` with a `QualifiedAccess` name for primitive owners such as `int::m`). Both paths reach `try_infer_trait_static_call` after the enum-variant, static-field and inherent-static lookups miss:
  - **Type parameter in scope:** `bound_method_candidates(m, Some(var))` selects the bound, the scheme's class parameter is unified with the parameter's variable, and a `BoundMethodCall { has_receiver: false, .. }` is recorded (the existing UFCS-under-a-bound shape).
  - **Concrete owner:** every single-parameter trait that declares `m` and has an instance for the owner is a candidate; more than one is an ambiguity error. The scheme's class parameter is unified with the owner type before discharge, so the constraint resolves to the owner's instance, not to the first instance that unifies.
- **Return-type-directed resolution** (`bind_call_result_to_expected`): before a call's bounds are discharged, a still-open result type is trial-unified with the node's expected type (`expected_here`: `let x: T = f()`, `return f()`, an argument with a known parameter type). `f()?` sets the inner call's expectation to `Result<E, err>` (the fn's error type in result mode). Only a successful unification is committed; the binding site still reports its own mismatch. This applies to generic fn calls, UFCS bound calls and static calls.
- With one matching instance and no expectation the instance is still picked (the choice is forced). With several and no expectation the existing "Ambiguous instance" error stands (`tests/compile_fail/trait_static_uninferable.hy`).

## Codegen

- **Concrete `Owner::m(args)`:** the discharged instance is in `call_site_dicts`; `emit_trait_static_call` emits the arguments (boxed / layout-converted per `trait_method_boundary_sig`, staged when an operand may clobber the operand stack) and a direct `CALL` to the instance FQN, plus the instance dictionary only when the target is the trait's default body.
- **`T::m(args)` in a shared body:** dictionary slot, `Index`, `CallIndirect` with the trailing dictionary (no receiver).
- **`T::m(args)` in a mono clone:** `mono_type_param_tys` maps `T` to its concrete type, so the clone calls that instance directly. The argument-typed lookup cannot see a return-only `T`.
- **Generic function returning `T`:** `emit_call_site_dicts` binds scheme variables from the call's result type; `bind_scheme_vars` also matches `Result<T, E>` / `Option<T>` sums structurally.
- A `Construct`-shaped static call is a call, not a variant: `ctor_unbox_ty`, `expr_is_construct_of`, `expr_may_clobber_operand_stack` and `local_escape::is_in_frame_ctor` check `tag_for` first.

## Library

- `Default::default` is a static trait method. `#[derive(Default)]` generates `static fn default()`; each class field takes its type's default (`0`, `0.0`, `false`, `""`, otherwise `Ty::default()`), and an enum takes its first variant with defaulted payloads.
- The placeholder `Serialize` / `Deserialize` derives were removed earlier; a prelude `Encode` / `Decode` pair is a separate plan that builds on this one.

## Not done

- Generic owners (`Box<int>::from_val(v)`) wait on generic trait instances (#550 / #551).
- Specialization keys are still argument types, so a generic call whose `T` is return-only uses the shared body plus dictionary (correct, not specialized).

# Static and return-type-dispatched trait methods (plan)

GitHub: [#524](https://github.com/ardax-corp/coil-lang/issues/524). Status: **plan**, not implemented.

This is the last piece typed decode needs in coil-json, coil-toml and coil-msgpack. A trait method whose `Self` type appears only in its **return type** has to be callable. Examples are constructors such as `FromVal::from_val(Val) -> T`, `Default::default() -> T` and `Deserialize::deserialize(Vec<byte>) -> T`. The target is:

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

## Where things stand

These were found with the #520–#524 repros on `main`. Line references are approximate.

| # | Piece | Today |
|---|-------|-------|
| 1 | Trait declaration | The parser accepts `static fn` in a `trait` body. `infer_fn.rs` rejects it with E0119 ("`static fn` is only allowed inside an `impl` block"). `TypeClassMethodDef` has no `is_static`. |
| 2 | Method lookup | `typeclass_method_schemes[(class, method)]`. The ground path (`ground_trait_method_for_receiver`) picks the instance by **unifying the first parameter** with the receiver. The bound path (`bound_method_candidates`) keys on the receiver's type var. A method with no `T`-typed first parameter can't be selected by either. |
| 3 | `Owner::m(..)` | Parses as `Construct` / `QualifiedAccess`. `try_infer_static_method_call` only knows **inherent** statics (`is_static_method`). Otherwise the owner is looked up as an enum: "Cannot find enum `Point`" / "`T`". |
| 4 | Instance resolution | Eager, at the call (`resolve_instance`, around `checker.rs:8960`). An open type var is unified with every matching instance. With one instance it is **silently picked**, which pins `T` before any annotation is seen. With two or more you get "Ambiguous instance for `Default<t64>`". The `let x: T = …` annotation only unifies with the call's result *after* the call has been inferred. |
| 5 | Call-site dictionaries | `emit_call_site_dicts` binds scheme vars from argument types, then from the call's result type. That is enough for a return-only `T` once the checker knows it. A nullary call lost the result type (fixed in #535). |
| 6 | Dictionary ABI | One tuple of `CodePtr`s per bound, in `flattened_methods` order. A bound call does `LOAD dict; CONST slot; Index; CallIndirect`. `BoundMethodCall { has_receiver: false }` already exists for UFCS under a bound, so static methods fit the ABI unchanged. |
| 7 | Monomorphization | Specializations are keyed by **argument** ground types (`specialization_for_call`), so a return-only `T` never monomorphizes. The shared body plus dictionary is correct, just not specialized. |
| 8 | Derives | `#[derive(Default)]` fills **every** field with `0` (`synth_default_class`), so a `string` field fails to typecheck. The `Serialize` / `Deserialize` derives are byte-cast placeholders. |

## Design

### Surface (one spelling)

- **Declaration:** `static fn name(params) -> R {}` in a `trait` body, which is the same spelling as an inherent static. An instance implements it with `pub static fn`. Static-ness must match the declaration (a new error, "instance method `m` must be `static`").
- **Calls:** `Owner::name(args)`, where `Owner` is either:
  - a concrete type with an instance (`Config::from_val(v)`), or
  - a type parameter in scope whose bounds provide `name` (`T::from_val(v)`).
- **No bare `from_val(v)`** for static trait methods. Nothing selects an instance except the expected type, and a second spelling for the same call goes against the "one spelling per construct" rule. The expected type still matters for a *generic function* that returns `T` (`let cfg: Config = decode_as(v)`); see below.

### Typechecking

1. **Trait declarations** record `is_static` per method, and the scheme has no receiver. Instances check that static-ness matches.
2. **`Owner::m(args)`:** extend `try_infer_static_method_call`:
   - `Owner` is a class or type with an instance of a trait that has a static `m`: instantiate the method scheme with the class parameter set to `Owner`, then discharge. That records the instance in `call_site_dicts`, which codegen already uses.
   - `Owner` is a type parameter in scope: resolve through the bound and record a `BoundMethodCall { has_receiver: false, method_slot, dict_index }`.
   - Anything else keeps today's enum / inherent paths.
3. **Return-type-directed resolution** fixes row 4 and is the core change:
   - **Bidirectional step.** When a call's result type contains a bound's open var and the node has an expected type (`expected_here`, see #531), unify the result with the expected type *before* discharging. This covers `let x: T = f()`, `return f()`, `f()?` in a `Result` fn, and arguments with a known parameter type.
   - **Deferred bounds.** A bound whose args are still open after that step is not resolved eagerly. It goes on a per-function pending list and is retried after each statement. At the end of the function, anything left is an error ("cannot infer `T` for `Default<T>`; annotate the binding"). This removes the silent single-instance pick, which is a behavior change for any code that relies on it. Audit it with `coil test`, the vm-wire suites and the package repos before landing.

### Codegen

- **Concrete `Owner::m(args)`:** a direct `CALL` to the instance FQN (`FromVal__Config__from_val`), with no receiver. Pass a dictionary only when the target is a default body (as the bound-call path does today).
- **`T::m(args)` under a bound:** the existing `BoundMethodCall` lowering (dictionary slot, `CallIndirect`, `has_receiver: false`).
- **Generic function returning `T`:** already handled by `emit_call_site_dicts` once the checker binds `T` (#535). Optional later work: add the result type to the mono key for return-only params.
- **Layouts:** static methods returning `Result<T, E>` / `Option<T>` cross the generic boundary in boxed layout. #528 adds the adapter thunks and conversions, and they apply unchanged.

### Library follow-ups

- **`#[derive(Default)]`:** per field, `T::default()` for the field's declared type. Primitives can use literals (`0`, `0.0`, `false`, `""`) so they don't need a dictionary. That fixes the `string` field case in row 8.
- **Built-ins:** `Default::default` and `Deserialize::deserialize` become static trait methods.
- **Replacing the placeholder `Serialize` / `Deserialize` derives** with a prelude `Encode` / `Decode` pair is a separate plan. It builds on this one.

## Delivery (one PR each)

1. `static fn` in traits plus concrete `Owner::m(..)`: typecheck and codegen. Tests: concrete call, default static body, missing / mismatched static.
2. `T::m(..)` under a bound (shared body and mono), including `Result<T, E>` returns (depends on #528).
3. Bidirectional plus deferred bound resolution, and the error for an uninferable `T`. Tests: two `Default` instances chosen by annotation, `return f()`, `f()?`, and the error case. Audit existing single-instance inference.
4. `#[derive(Default)]` per field; `Default` / `Deserialize` as static trait methods. Docs: coil-website traits page, `limitations.md`.

## Open questions

- Should a bare call `from_val(v)` be accepted when exactly one instance exists? This plan says no, for one spelling and no silent picks.
- Should there be defaulting rules (for example, an integer literal defaulting to `int`) for otherwise-uninferable `T`? This plan says no; it's an error with a help note.

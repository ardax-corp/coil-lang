# Function keys and the id counter

The virtual `symbol` module has two functions meant as keys for run-time
tables such as memo caches (the caches themselves are userland's):

| Call | Value | Where it is decided |
|------|-------|---------------------|
| `symbol::fn_key()` | the `int` key of the body the call is written in | compile time (HIR build) |
| `symbol::next_id()` | the next value of one process-wide counter: `1`, `2`, … | run time (HostInvoke **179**) |

## `fn_key`

The checker types `fn_key()` as `() -> int` (`PreludeFn::FnKey`, exported
from `symbol`, not the prelude). It rejects arguments (`E0204`) and a call
outside a function, method, lambda or test body, such as a `const` or
`static` initializer or top-level code (`E0144`, checked by
`current_return_ty` being unset). It is not a function value: `let f =
fn_key;` does not resolve (`E0100`), as for the other compiler builtins.

`hir::build` writes the key in place of the call: `Cx::call` turns
`fn_key()` into `HirKind::Lit(Lit::Int(key))` using the body's `key_name`.
Nothing at run time knows about it, so there is no HostInvoke id, and the
purity rows give it no effect (`pure fn` may call it; call folding and
comptime see a constant).

The key is FNV-1a over the `key_name`, shifted right one bit so it is never
negative (`hir::build::fn_key`). The `key_name` of a body is:

| Body | `key_name` |
|------|------------|
| function | `module::name/arity` (`f/1`, `m::f/2`) |
| inherent method | `module::Owner::name/arity` (declared parameters, without `self`) |
| trait instance method | `module::Trait for Owner::name/arity` |
| trait default method | `module::Trait::name/arity` (one key, shared by every instance that uses the default) |
| lambda | the enclosing body's `key_name` + `::<lambda@OFFSET>` (byte offset of the lambda in its file); at top level the lambda's body name |
| test | `module::test "label"` |
| top-level code, static initializer | none: `E0144` |

Overloads differ by arity (`E0121` rejects two that accept the same count,
and a rest overload's declared parameter count is above every fixed one), so
`name/arity` tells them apart. A collision of two 64-bit FNV values in one
program is possible in principle but has not been made into an error.

Because the literal is written while building HIR, before every
optimization:

- **Inlining** copies the callee's literal into the caller, so an inlined
  function keeps its own key.
- **Generics** get a key per instantiation (see below).
- **DCE / tree-shaking** removes bodies, not keys: a key does not depend on
  which functions survive or on their order.
- **Incremental and build caches**: the key is a function of the module
  path, item name and arity only, never of compile order or ids handed out
  during a compile, so a cached module's literals stay valid. The build
  cache (`build-cache.md`) keys whole programs anyway.
- **Register VM / bytecode tiers**: the key is an ordinary constant by the
  time MIR sees it; nothing in the dense or LIR tiers knows about it.

Stability: a function's key stays the same across builds while its module,
name and arity do. A lambda's key also changes when code above it in the
file moves it.

## Generic bodies

In a generic body (own type parameters, a generic class's method, or a
lambda in one) the builder cannot know the types, so it records the call
in `HirBody::fn_key_sites` with the body's key name and writes a panic block
(`{ panic "fn_key() in `f` reached without its type arguments ..."; 0 }`)
in its place. `Compiler::hir_instance`, which lowers every mono clone and
instance clone from the generic HIR at its types, replaces each site with
the literal `fn_key("name<T1, T2>")`, the types in the order of the body's
type variables. So `ident(1)` and `ident(2)` share a key, `ident("a")` has
another, and a sibling generic called from a clone (#1419) is keyed at the
clone's types (`wrap(1) == ident(1)`).

The shared body has no type arguments at run time (unbounded `T` carries
no type information), so the design (agreed 2026-10-29) is to keep it from
being reached:

- `monomorphize::mark_keyed` marks a generic function *keyed* when it calls
  `fn_key()` or another keyed generic (fixpoint over bare-identifier calls).
  Calls of a keyed function bypass the clone budget (`--clone-budget`),
  like `let g = wrap` aliases (#916).
- A keyed generic read as a value (`let f = ident;`, passed as an
  argument) is `E0145` (`MonoPlan::keyed_values`): wrap the call at concrete
  types instead.
- A body with `fn_key_sites` is never inlined (`hir::inline::inlinable`).
- What is left reaches the panic: a keyed generic of another module used as
  a value, a clone refused by a gate (`knows_tys`, `layout_safe`), a
  trait-bound dictionary call. A trait default method body is not treated as
  generic: it keeps one key for every type that uses it, since it is often
  called through the shared dictionary path.

## `next_id`

`symbol_next_id` (archive 6.8, `machine/src/symbol.rs`) is a
`fetch_add(1, Relaxed)` on one `static AtomicI64` starting at `1`. It is
process-wide: every thread, task and VM in the process (the test runner runs
files and cases side by side in one process) shares it, so no two calls
return the same id, and calls on one thread see increasing ids that are
consecutive unless another thread took ids in between. The host row's
`HOST` effect keeps a call from being folded at compile time, hoisted out
of a loop, removed, or run in parallel automatically, and a `pure fn`
cannot call it (`E0413`).

Tests: `tests/functions/fn_key.hy`, `tests/generics/fn_key_per_instance.hy`,
`tests/generics/compile_fail/fn_key_generic_value.hy`, `tests/modules/fn_key_across_modules.hy`
(with `examples/src/fn_key_mod.hy`), `tests/functions/compile_fail/fn_key_outside_function.hy`,
`tests/host/symbol_next_id.hy`.

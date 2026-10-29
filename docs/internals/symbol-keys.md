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
- **Generics** share one key per source function: the HIR body is built once
  per definition, and both the shared (dictionary-passing) body and every
  mono clone come from it. `ident(1)` and `ident("a")` see the same key. A
  memo table keyed by `fn_key()` alone therefore needs the argument types in
  its key too when a generic function's results differ by instantiation.
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

## `next_id`

`symbol_next_id` (archive 6.7, `machine/src/symbol.rs`) is a
`fetch_add(1, Relaxed)` on one `static AtomicI64` starting at `1`. It is
process-wide: every thread, task and VM in the process (the test runner runs
files and cases side by side in one process) shares it, so no two calls
return the same id, and calls on one thread see increasing ids that are
consecutive unless another thread took ids in between. The host row's
`HOST` effect keeps a call from being folded at compile time, hoisted out
of a loop, removed, or run in parallel automatically, and a `pure fn`
cannot call it (`E0413`).

Tests: `tests/functions/fn_key.hy`, `tests/modules/fn_key_across_modules.hy`
(with `examples/src/fn_key_mod.hy`), `tests/functions/compile_fail/fn_key_outside_function.hy`,
`tests/host/symbol_next_id.hy`.

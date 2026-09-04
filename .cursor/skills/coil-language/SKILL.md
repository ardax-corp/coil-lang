---
name: coil-language
description: >-
  Write and reason about coil programs (.hy): syntax, HM types, modules, virtual
  builtins, coroutines, FFI, and tests. Use when authoring or editing coil source,
  explaining coil syntax, debugging type errors, or when the user mentions .hy files,
  coil.toml, or coil language features.
---

# coil language

coil is a **statically typed** scripting language with Hindley–Milner inference. Sources are `.hy`; the CLI compiles to versioned `.hyc` archives (`compile` / `run`) or runs in memory (default).

**Do not invent stdlib APIs** — userland helpers live in
[coil-stdlib](https://github.com/ardax-corp/coil-stdlib)
([docs](https://github.com/ardax-corp/coil-stdlib/blob/main/docs/README.md)).
Compiler builtins live in virtual modules; see [not-builtins](https://github.com/ardax-corp/coil-website/blob/main/src/content/docs/references/not-builtins.md) (`/docs/references/not-builtins`).

**Prefer method-based APIs** — operations on a type should be `impl` methods (`m.insert(k, v)`), not free functions (`insert(m, k, v)`). Virtual-module host primitives (`io::read`) stay as free fns; stdlib collections and new language surface default to methods.

## Quick workflow

```bash
cargo build --workspace
cargo run -- examples/fib.hy          # in-memory compile + run (no out.hyc)
cargo run -- compile examples/fib.hy    # writes out.hyc (or -o path)
cargo run -- test                     # discover **/*.hy under ./tests
cargo build --bin coil && (ulimit -v 65536; ./target/debug/coil test)  # 64MB leak check
```

| Goal | Command |
|------|---------|
| Run one file | `cargo run -- path/to/file.hy` |
| Compile only | `cargo run -- compile file.hy [-o path]` (default `out.hyc`) |
| Run archive | `cargo run -- run out.hyc` |
| Project tests | `cargo run -- test [path] [--fail-fast]` |

`coil compile` always recompiles; `coil run` never recompiles. Default mode always recompiles in memory — no stale bytecode cache.

## File shape

Every runnable program needs `fn main()` **unless** the file only has `test("…")` cases (harness injects virtual `main` for `coil test`; do not mix `main` and `test()` in one file).

Implicit imports (no `use` needed): prelude (`prelude`, `prelude::ops`, `prelude::test`, `prelude::math`) — injected by the compiler, not written in source.

Explicit `use` for: `io`, `string`, `ffi`, `thread`, `env`, `gc`, `clock` (`wall_nanos` / `mono_nanos` / `sleep_ms`). Time calendar/format: [coil-time](https://github.com/ardax-corp/coil-time). Regex: [coil-regex](https://github.com/ardax-corp/coil-regex). TLS: [coil-tls](https://github.com/ardax-corp/coil-tls). Crypto: [coil-crypto](https://github.com/ardax-corp/coil-crypto).

```coil
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn main() {
    write_all(stdout(), to_bytes(format("%i", 42)));
}
```

There is **no `print` statement** — use `io` + `string::format` / `to_bytes`.

## Syntax essentials

| Topic | Rule |
|-------|------|
| Functions | `fn name(T arg) -> R { … }`; `async fn` for coroutines |
| Variables | `let x = e;` / `const x = e;`; destructuring `let (a,b) = …`, `let { x } = …` |
| Control | `if`/`else`, `while`, `for x in iter`, `break`/`continue` |
| Types | Primitives `int` `float` `string` `bool` `byte`; arrays `[T]` / `[T; N]`; tuples; dicts as anonymous records |
| Enums | `enum E { A, B(T) }`; constructors are per-enum (`Status.Ok` next to `Result.Ok`). Canonical spelling is `Enum.Case` (`Status.Ok`, `Option.Some(x)`, `Color.Red`). Bare `Some`/`None`/`Ok`/`Err` is sugar only when unambiguous. `match e { … }` with record/nested patterns. `default` is the only whole-arm catch-all; `_` is nested wildcard only (`Result.Err(_)`, `Some(_)`, tuple/record slots). Scalar-backed: `#[repr(int)]` / `float` / `string` / `bool` or inferred when every case is `Case = lit` of one simple type. `#[derive(Show, Eq, Ord, Hash)]` composes with `#[repr]`. Runtime is the unboxed literal; type is still `E`. Show of a scalar case is the backing (`Status.Ok` → `200`). In expression position the value implicitly coerces to the backing (`let n: int = Status.Ok`). No reverse coerce (`int` → `Status`) and no matching raw `200` on a `Status` scrutinee. Payload enums stay boxed. |
| Errors | Built-in `Option`/`Result`; `raise`, `?`, `??`, `?.` |
| Classes | `class C { … }`, `impl C { … }`, `new C(…)` — prefer methods for type-tied ops; inherent `fn drop()` is a GC-time finalizer |
| Modules | `use path::{a, b};`, `mod foo;` (load without binding) |
| FFI | `extern "c" { fn …; }` or `use ffi::{dload, declare, invoke}` + `ffi::types::{Int, …}` |
| Attributes | `#[derive(Show, Eq, Ord, Hash)]` (composes with `#[repr(int)]` on scalar enums), user `attr` decorators; tests are `test("desc") { … }` only |
| Type query | `typeof expr` → compile-time FQN string (not evaluated at runtime) |

Named call-site args: positional prefix then `f(name: v)`. Rest: trailing `T... xs`. Spread: `f(...pack)`.

Ranges `a..b` / `a..=b` are **lazy** `Range<T: Ord>` / `RangeInclusive<T: Ord>`.
`for` and `.to_vec()` step `int`/`byte`/`float` only; other `Ord` is a type error.

Full grammar: [syntax](https://github.com/ardax-corp/coil-website/blob/main/src/content/docs/references/syntax.md) (`/docs/references/syntax`).

## Virtual modules (explicit `use`)

`use path::*` is banned (`E0124`) — list names explicitly for virtual and
userland modules. Prelude is auto-injected.

| Module | Typical import | Notes |
|--------|----------------|-------|
| `io` | `use io::{stdout, open, read, write};` | `Stream`, `[byte]`, files, TCP/UDP; non-blocking L0 |
| `string` | `use string::{format, to_bytes, from_bytes};` | Formatting / UTF-8 bytes |
| `ffi` | `use ffi::{dload, declare, invoke};` + `use ffi::types::{Int, Ptr};` | Dynamic loading |
| `thread` | `use thread::{spawn, join, channel, send, recv};` | OS threads, channels, mutex |
| `env` | `use env::{args, var, exec};` | process environment |
| `time` | `use time::{…}` via [coil-time](https://github.com/ardax-corp/coil-time) | userland package (`dload`); not a virtual module |
| `gc` | `use gc::{root, weak, collect};` | `Root` / `Weak` pins; class `fn drop()` runs at collect / teardown |

`byte` is 0..=255; integer literals coerce under `byte` / `[byte]` expectations.

## Coroutines

`async fn`, `yield`, `resume h`, `resume h with v`, `let x = yield e`, `yield from`, `done(h)`. Types: `coroutine<Y, S>`. Resume after done → default value.

Tutorial: [08-coroutines](https://github.com/ardax-corp/coil-website/blob/main/src/content/docs/manual/tutorial/08-coroutines.md) (`/docs/manual/tutorial/08-coroutines`).

## Tests

```coil
test("addition") {
    assert(1 + 1 == 2)?;
}
```

Body is Result mode. `panic` aborts VM; `coil test` treats as failure. `#[test]` on `fn` is a type error.

## Multi-file projects

`coil.toml` at project root; entry file has empty namespace. See [project-config](https://github.com/ardax-corp/coil-website/blob/main/src/content/docs/references/project-config.md) (`/docs/references/project-config`).

## Learn by example

| Topic | File |
|-------|------|
| Fibonacci smoke | `examples/fib.hy` |
| Option/Result | `examples/option.hy`, `examples/result.hy` |
| Modules | `examples/modules.hy` |
| Coroutines | `examples/coro.hy` |
| FFI | `examples/strlen.hy` |
| GC `fn drop()` | `examples/finalizer.hy` |
| Full catalog | [examples](https://github.com/ardax-corp/coil-website/blob/main/src/content/docs/manual/examples.md) (`/docs/manual/examples`) |

Tutorial path: [getting-started](https://github.com/ardax-corp/coil-website/blob/main/src/content/docs/manual/getting-started.md) (`/docs/manual/getting-started`) → chapters 01–11.

## Common pitfalls

1. **`main` + `test()`** — do not combine in one file.
2. **Assuming stdlib** — no `sort` or HTTP in the VM; add [coil-stdlib](https://github.com/ardax-corp/coil-stdlib) for collections/IO adapters; HTTP is [coil-http](https://github.com/ardax-corp/coil-http) via spool. `sqrt` is prelude math.
3. **Missing `use`** — `io`/`string` are not auto-imported.
4. **FFI** — needs system libffi; `resolve_library` searches entry dir, `coil.toml` paths, system.
5. **Stale `out.hyc`** — only from `coil compile`; delete before `coil run` if sources changed.
6. **Type errors** — read diagnostic `E####`; index in [error-codes](https://github.com/ardax-corp/coil-website/blob/main/src/content/docs/references/error-codes.md) (`/docs/references/error-codes`).
7. **`Option`/`Result`** — always boxed enums (archive 4). Free `fn f<T>(T) -> Option<T>` is still `E0127`; put that return on an inherent method.

## Debugging programs

For hangs, wrong values, panics, breakpoints: use the **coil-debug** skill (`coil debug`, `coil dissect`).

## Additional resources

- Syntax cheat sheet + patterns: [reference.md](reference.md)
- API lookup: [references](https://github.com/ardax-corp/coil-website/tree/main/src/content/docs/references) (`/docs/references`)
- Userland stdlib: [coil-stdlib docs](https://github.com/ardax-corp/coil-stdlib/blob/main/docs/README.md)
- Internals (contributors): **coil-contributor** skill

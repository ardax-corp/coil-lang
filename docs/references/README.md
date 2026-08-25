# References

Lookup docs for language constructs and compiler-provided APIs. For a guided introduction, start with the [manual](../manual/getting-started.md).

Compiler builtins live in **virtual modules** (not `.hy` files). Every file gets prelude auto-injected (`inject_prelude_scope` — `prelude`, `prelude::ops`, `prelude::test`, `prelude::math`; no source `use` needed). FFI, `io`, `string`, `thread`, `time`, `env`, `crypto`, and `gc` require an explicit `use` with concrete or brace imports (`use io::{stdout, open};`). `use path::*` is always rejected (`E0124`).

## Language

| Document | Contents |
|----------|----------|
| [Syntax](syntax.md) | Grammar overview, declarations, expressions |
| [Types](types.md) | Type system, aliases, aggregates, generics |
| [Operators](operators.md) | Arithmetic, comparison, logical, field access |
| [Keywords](keywords.md) | Reserved words and constructs |
| [Modules](modules.md) | Namespace rules, `use` resolution |
| [Project config](project-config.md) | `coil.toml` manifest format |
| [Error codes](error-codes.md) | Stable `E####` diagnostic codes |

## Built-ins and virtual modules

| Document | Kind | Purpose |
|----------|------|---------|
| [Option / Result](option-result.md) | Prelude enums | Built-in sum types |
| [print](print.md) | Migration note | Removed statement; use `io` + `string` |
| [format](format.md) | Intrinsic | `string::format(...)` builds a formatted string |
| [string](string.md) | Virtual module | `format` / UTF-8 byte conversions |
| [arrays](arrays.md) | Types / expression | Fixed `[T; N]`, growable `Vec<T>`, `len` |
| [math](math.md) | Prelude | IEEE float math plus `dot` / `matmul` / `cross` / `Matrix` |
| [FFI](ffi.md) | Virtual module | `dload` / `declare` / `invoke` / `extern` |
| [done](done.md) | Expression | Coroutine finished? |
| [io](io.md) | Virtual module | Non-blocking streams, TCP, UDP |
| [io::fs](io-fs.md) | Virtual module | Path / metadata helpers |
| [Iterator](iterator.md) | Prelude traits | `for x in` protocol |
| [assert](assert.md) | Prelude test | `assert(cond[, msg]) → Result` |
| [test harness](test-harness.md) | CLI | `test("…")` / `#[test]` |
| [panic](panic.md) | Keyword | Abort with a message |
| [casts](casts.md) | Expression | `expr as T` |
| [time](time.md) | Virtual module | Timestamps, sleep |
| [env](env.md) | Virtual module | Args, env vars, `exec` |
| [crypto](crypto.md) | Virtual module | Hashes, AEAD, keys |
| [regex](regex.md) | Userland package | [coil-regex](https://github.com/ardax-corp/coil-regex) — PCRE2 via FFI |
| [tls](tls.md) | Userland package | [coil-tls](https://github.com/ardax-corp/coil-tls) — rustls via `dload("tls")` |
| [gc](gc.md) | Virtual module | `Root` / `Weak` pins |
| [ord / char](ord-char.md) | Prelude | Single-byte string ↔ `byte` |
| [host natives](host-natives.md) | Embedder API | Rust closures via `HostInvoke` |
| [What is NOT a builtin](not-builtins.md) | Scope | Gaps vs builtins |
| [coil-stdlib](https://github.com/ardax-corp/coil-stdlib/blob/main/docs/README.md) | Userland | `bytes`, `text`, `collections`, `io::sync`, … |
| [coil-regex](https://github.com/ardax-corp/coil-regex/blob/main/docs/README.md) | Userland | PCRE2 regex — see [regex](regex.md) |
| [coil-tls](https://github.com/ardax-corp/coil-tls) | Userland | TLS (`libtls`) — see [tls](tls.md) |
| [coil-http](https://github.com/ardax-corp/coil-http/blob/main/docs/README.md) | Userland | HTTP/1.1 client + server — [install via spool](../manual/http-client.md) |

Do not document coil-stdlib APIs here; they live in that repo. Workspace
`[module].roots` look for `./.deps/coil-stdlib/src` or `../coil-stdlib/src`.

## Related

- [Manual](../manual/getting-started.md) — tutorials and examples
- [Internals](../internals/README.md) — pipeline, VM, debug info

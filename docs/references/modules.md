# Modules reference

This document specifies the syntax and semantics of coil's module system: `use` imports, `mod` forward declarations, namespace rules, and path resolution.

---

## Syntax

### `use` statement

```
use_stmt   ::= 'use' use_path ';'
use_path   ::= IDENT ('::' IDENT)* '::' '{' use_item (',' use_item)* ','? '}'
             | IDENT ('::' IDENT)* ('as' IDENT)?
use_item   ::= IDENT ('as' IDENT)?
```

(`use path::*` is still parsed but always rejected at typecheck with `E0124`.)

Forms:

| Form | Example |
|------|---------|
| Concrete import | `use foo::sadge;` |
| Aliased import | `use foo::sadge as f;` |
| Brace group | `use math::{add, mul as product};` |
| Multi-segment | `use lib::io::read;` |

Rules:

- Every `use` statement ends with `;`.
- Path segments are identifiers separated by `::`.
- The last segment is either an identifier (concrete import) or a `{ … }` brace group.
- Concrete imports and brace-group items may use `as`.
- A brace group must be the **last** segment.
- Brace groups desugar to one concrete import per item (same module path).
- **`use path::*` is banned** for every module (virtual and userland). List names explicitly (`use foo::{a, b}` or `use io::open;`). Prelude is auto-injected via `inject_prelude_scope` — no source `use prelude::*` needed.

### `mod` statement

```
mod_stmt ::= 'mod' IDENT ';'
```

Example: `mod foo;`

A `mod` declaration loads a module file but does not bind any names in the current scope.

---

## Virtual modules (compiler builtins)

Some `use` paths resolve to **compiler-owned virtual modules**, not `.hy` files on disk. The pipeline skips disk discovery for these paths.

| Module | Exports | Auto-imported? |
|--------|---------|----------------|
| `prelude` | `Option`, `Result`, `Iterator`, `IntoIterator`, `ArrayIter` | Yes (every file) |
| `prelude::ops` | `Add`, `Sub`, `Mul`, `Div`, `Num`, `Eq`, `Ord`, `Lt`, `Le`, `Gt`, `Ge`, `Show`, `Length`, `Into` | Yes (every file) |
| `prelude::test` | `assert` | Yes (every file) |
| `ffi` | `dload`, `declare`, `invoke` | No — `use ffi::{dload, declare, invoke};` |
| `ffi::types` | `Int`, `Float`, `String`, `Void`, `Ptr`, `Callback`, … | No — `use ffi::types::{Int, Ptr, …};` |
| `io` | `Stream`, `IoError`, `Read` / `Write`, `stdin` / `stdout` / `stderr` / `open` / `read` / `write` / `write_from` / `close`, `from_bytes` / `to_bytes`, stream timeouts | No — `use io::{stdout, open, …};` (blocking helpers: `io::sync`) |
| `io::net::tcp` | `connect` / `connect_timeout` / `listen` / `accept` / `accept_wait` / `accept_wait_timeout`, address helpers, `set_nodelay`, `shutdown` | No — `use io::net::tcp::{connect, listen, …};` |
| `io::net::udp` | `bind` / `connect` / `send_to` / `recv_from` / `recv_from_wait` / `local_port` | No — `use io::net::udp::{bind, send_to, …};` |
| `io::fs` | Path/metadata helpers (`exists`, `realpath`, `list_dir`, …) | No — `use io::fs::{exists, list_dir, …};` |
| `time` | `timestamp`, `Period`, `format` / `parse`, monotonic `Instant` | No — `use time::{timestamp, sleep_ms, …};` |
| `env` | `args`, `var`, `cwd`, `exit`, `exec` (argv-only) | No — `use env::{args, var, …};` |
| `crypto` | Hashes, HMAC, AEAD, Ed25519, Argon2, `random_bytes`, … | No — `use crypto::{sha256, random_bytes, …};` |
| `thread` | `spawn`, channels, mutexes | No — `use thread::{spawn, join, channel, …};` |
| `gc` | `Root` / `Weak`, `root` / `unroot` / `get` / `weak` / `upgrade`, `heap_bytes` / `collect` | No — `use gc::{root, weak, collect, …};` |

TLS for applications is **not** a virtual module named `tls` or `io::net::tls`. Use the [coil-tls](https://github.com/ardax-corp/coil-tls) package (`use tls::{client, server}`); see [tls](tls.md). `use tls` / `use io::net::tls` without that package on `[module].roots` is a module-not-found error. coil-tls binds leftover HostInvoke via `io::__tls`.

### Prelude rebind / redefine

Short prelude names are bound in scope so `Option::Some` and `T: Eq` work without imports. To redefine a prelude name:

```coil
use prelude::ops::Eq as PreludeEq; // frees short `Eq`
trait Eq<T> { /* your trait */ }   // now allowed
// Builtin still reachable as `prelude::ops::Eq` or `PreludeEq`
```

Without the `as` rebind, `trait Eq` / `enum Option` is a conflict diagnostic.

The implicit prelude (`prelude`, `prelude::ops`, `prelude::test`, `prelude::math`) is compiler-owned and not configurable via `coil.toml`. Extra disk search paths belong in `[module].roots`.

---

## Path resolution algorithm

Given a concrete import `use a::b::c;`:

0. If the path matches a **virtual module** export (see above), bind that export and stop — no disk file is loaded.
1. Split the path into segments. All segments except the last form the **directory path**; the last segment is the **item name**.
   - Path: `["a", "b"]`
   - Item name: `"c"`
2. For each search root in `[module].roots` (from `coil.toml`, in declaration order):
   - **One-item-per-file:** `<project_root>/<root>/a/b/c.hy`
   - If the file exists, **stop** — this is the resolved module file.
3. If no one-item-per-file candidate exists, try the **module-file** fallback for each root:
   - `<project_root>/<root>/a/b.hy` (items exported from the module file)
   - This is what makes `use math::add;` work when `add` lives in `math.hy`.
4. If no root contains either form, emit a module-not-found diagnostic.

**Shadowing:** when both `<root>/a/b/c.hy` (one-item-per-file) and `<root>/a/b.hy` (module file) exist, the one-item-per-file path always wins. Avoid keeping the same item name in both layouts.

Given a brace-group import `use math::{add, mul};`:

1. Desugar to `use math::add;` + `use math::mul;` (same path, one item each).
2. Resolve each item with the algorithm above (typically both hit the same `math.hy`).

Given a wildcard import `use a::b::*;`:

1. Discovery may still enqueue `<root>/…/stem.hy` (same path split as before:
   `use foo::*` → stem `"foo"`; `use a::b::*` → stem `"b"` under prefix `a`).
2. Typechecking always rejects the import with `E0124` (`WildcardImport`) before
   names are bound — virtual and userland alike. List exports explicitly.

Given a `mod foo;` declaration:

1. For each search root, check `<project_root>/<root>/foo.hy`.
2. First existing file wins.

### Resolution examples

| Statement | Resolved file (root = `src/`) |
|-----------|-------------------------------|
| `use foo::sadge;` | `src/foo/sadge.hy`, else `src/foo.hy` |
| `use math::{add, mul};` | `src/math.hy` (module-file fallback) |
| `use lib::io::read;` | `src/lib/io/read.hy`, else `src/lib/io.hy` |
| `use foo::*;` | rejected (`E0124`); use brace/concrete imports |
| `use io::{open, stdout};` | virtual `io` exports (no `.hy`) |
| `mod foo;` | `src/foo.hy` |

With multiple roots `["./src", "./vendor"]`, the compiler checks `./src/...` first, then `./vendor/...`. The first match wins.

### Userland stdlib

[coil-stdlib](https://github.com/ardax-corp/coil-stdlib) (same org as the language
and [spool](https://github.com/ardax-corp/spool)) is a separate package. Workspace
roots look for `./.deps/coil-stdlib/src` or `../coil-stdlib/src`.

How to consume it in other projects, module catalog, HTTP, and IO adapters:
[coil-stdlib docs](https://github.com/ardax-corp/coil-stdlib/blob/main/docs/README.md).

### Library deps via `spool`

Git/path library dependencies are declared in `coil.toml` `[dependencies]` and
fetched by **`spool`** (not by the compiler). After install, spool maintains a
managed root such as `.spool/deps/<package-name>` (symlinks into a shared cache)
and that directory should be listed in `[module].roots`:

```toml
[module]
roots = ["./src", "./.spool/deps"]
```

`use greet::hello;` then resolves under `.spool/deps/greet/hello.hy` with the same
algorithm as any other root — first match wins; local `./src` still shadows
deps. The compiler does **not** read `coil.lock` or inject roots automatically.
See [Project configuration](project-config.md) for `[package]` / `[dependencies]`
schema and the **`spool` vs `coil package`** naming distinction (`coil package`
builds an embedded executable; it is not the library dependency manager).

---

## Namespace rules

### Computing a file's namespace

For a resolved file path, the namespace is:

1. Find the **first** search root that contains the file.
2. Take the path relative to that root.
3. Strip the `.hy` extension.
4. Replace path separators with `::`.

Examples (root = `src/`):

| Absolute path | Relative path | Namespace |
|---------------|---------------|-----------|
| `src/foo.hy` | `foo.hy` | `foo` |
| `src/foo/sadge.hy` | `foo/sadge.hy` | `foo::sadge` |
| `src/lib/io/read.hy` | `lib/io/read.hy` | `lib::io::read` |

If a file is outside all search roots, the namespace falls back to the file's bare stem.

### Entry file

The file passed to the compiler (or declared in `[entry].file`) uses the **empty namespace** (`""`). Top-level items in the entry file have unprefixed FQNs.

### Fully qualified names (FQN)

Top-level functions register under:

```
<namespace>::<function_name>
```

If the namespace is empty, the FQN is just `<function_name>`.

User **classes** register the same way: `Ty::Con("point::Point")` for `class Point` in `src/point.hy`. `use point::{Point}` (and `as` aliases) bind that constructor so `new Point(...)`, `Point::static(...)`, fields, methods, and `Point<T>` annotations work in the importer. Two modules may both export `class Client`.

The FQN shape depends on **which file** path resolution loaded (see [Path resolution algorithm](#path-resolution-algorithm)):

**Convention A — one-item-per-file** (`use a::b::c;` → `<root>/a/b/c.hy`):

- File namespace: `a::b::c`
- Function name inside the file: `c`
- FQN: `a::b::c::c` (last path segment names both the file and the function)

**Convention B — module-file fallback** (`use math::add;` → `<root>/math.hy`):

- File namespace: `math`
- Function name inside the file: `add`
- FQN: `math::add` (namespace is the module file stem; item is the bare function name)

---

## Wildcard imports (`::*`)

`use path::*;` is **always** a compile error (`E0124`), for virtual modules
(`io`, `ffi`, `string`, …) and userland disk modules alike. Import concrete
names or use a brace group.

Prelude (`prelude`, `prelude::ops`, `prelude::test`, `prelude::math`) is
auto-injected every file via `inject_prelude_scope` — do not write
`use prelude::*` in source.

Discovery may still resolve a path ending in `*` to `<root>/…/stem.hy` when
scanning dependencies, but no names are imported from a wildcard `use`.

Brace-group example — `src/foo.hy`:

```coil
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
fn sadge() { write_all(stdout(), to_bytes(format("%i", 100))); }
fn greet() { write_all(stdout(), to_bytes(format("%i", 200))); }
```

After `use foo::{sadge, greet};` in another file, both are callable by those
local names.

---

## Aliasing rules

`use path::name as alias;`:

| Property | Behavior |
|----------|----------|
| Local name | `alias` |
| FQN target | Depends on resolved file (Convention A vs B; see above) |
| Function expected in file | `fn name()` |
| Typechecker | Inserts `alias` into the environment with a fresh type variable |

Without `as`, the local name defaults to the last path segment (`name`).

Examples:

```coil
// Convention A — foo/sadge.hy
use foo::sadge;           // local: sadge  → FQN foo::sadge::sadge
use foo::sadge as f;      // local: f      → FQN foo::sadge::sadge
use lib::io::read as rd;  // local: rd     → FQN lib::io::read::read

// Convention B — math.hy (no math/add.hy)
use math::add;            // local: add    → FQN math::add
use math::add as plus;    // local: plus   → FQN math::add
```

Aliases are **per-file**. They do not propagate to other modules.

---

## Discovery and compilation order

The pipeline runs in two passes:

### 1. Discovery pass

- Start with the entry file on the worklist.
- Parse each file; walk the AST for `use` and `mod` declarations.
- Enqueue referenced files (deduplicated).
- Repeat until the worklist stabilizes (all transitive dependencies found).
- Cache source text to avoid re-reading from disk.

### 2. Compilation pass

- Drain the worklist in **LIFO** order (dependencies compile before consumers).
- Each file compiles with its computed namespace.
- `use` statements in the consumer file resolve local names to FQNs via the alias map.
- Glob imports expand against the compiled function registry.
- Multi-file programs share **one constant pool** for the whole link (`?` / match / other pool-backed immediates). The pool is cleared only on a fresh compile (prologue-only bytecode), not between modules.

---

## Interaction with `coil.toml`

Module resolution depends on `[module].roots` from the project manifest. See [Project configuration](project-config.md) for manifest format and default behavior.

Without a manifest, the compiler uses a single default root: `src/`.

`[package]` and `[dependencies]` are accepted by the parser for **`spool`**, but they do not change discovery by themselves — only paths listed in `roots` are searched. Put `./.spool/deps` (or equivalent) in `roots` after `spool install` so dependency packages participate in `use` resolution.

---

## Diagnostics

Common errors:

| Situation | Message (approximate) |
|-----------|----------------------|
| File not found | Module not found for `use a::b::c` |
| Unknown identifier after import | Cannot find function `x` in this scope |
| Function name mismatch | Call fails at link time if FQN not in registry |

The typechecker validates that aliased names exist in the environment. The codegen maps local names to FQNs; a missing target surfaces when the call is emitted.

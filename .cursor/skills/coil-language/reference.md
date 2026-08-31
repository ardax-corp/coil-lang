# coil language reference (agent cheat sheet)

Read when you need syntax detail beyond SKILL.md. User docs live in [coil-website](https://github.com/ardax-corp/coil-website) (`src/content/docs/`; routes `/docs/…` until a domain is set).

## Type system highlights

- HM inference with generics, traits, associated types/GATs, existentials.
- Type aliases: `type Name = T;` — lexically scoped; inner may shadow outer.
- `never` from `return`/`raise`/`panic` and proven-infinite loops absorbs in joins.
- Path completeness: concrete non-unit returns need all paths to exit (`E0111`).
- Casts: `expr as T` — see [casts](https://github.com/ardax-corp/coil-website/blob/main/src/content/docs/references/casts.md) (`/docs/references/casts`).

## Expression forms

```
call        f(a, b, name: v)
index       arr[i]
field       rec.field (chained)
match       match e { pat => expr, … }
if          if cond { … } else { … }
block       { stmts; expr }
lambda      fn (T x) use (y) => expr   // first-class fn values
array lit   [1, 2, 3]
tuple       (a, b)
dict        { key: val, … }
enum ctor   Color.Red, Option.Some(x)
raise       raise "msg"
try?        expr?              // Result early return
default     expr ?? default
optional    expr?.field
range       0..10, 0..=10
for         for x in iter { … }
```

## Pattern forms (match / let)

- Literals, identifiers, `_`, tuple `(a,b)`, record `{ x, y }`, enum `E.A`, `E.B(x)`.
- Nested record patterns on enum variants use slot-based unpack (compiler detail).
- `let` destructuring: tuples and records only (no enum ctor patterns in `let`).

## IO patterns

```coil
use io::{stdout, stderr, open, read, write, write_all, close, from_bytes, to_bytes};
use string::{format, to_bytes};

// Write formatted text
write_all(stdout(), to_bytes(format("%s %i", "hi", 42)));

// Read file to bytes
let s = open("path", "r")?;
let buf = read_to_end(s)?;
close(s)?;
```

Buffers are `[byte]`, not `string`. Convert via `string::to_bytes` / `from_bytes`.

Sync adapters exist for blocking-style loops on non-blocking streams — see [io](https://github.com/ardax-corp/coil-website/blob/main/src/content/docs/references/io.md) (`/docs/references/io`).

## Error handling patterns

```coil
fn fallible() -> Result<int, string> {
    if bad { return Err("oops"); }
  return Ok(42);
}

fn caller() -> Result<int, string> {
    let x = fallible()?;   // propagate Err
    return Ok(x + 1);
}

match opt {
    None => 0,
    Some(v) => v,
}
```

`assert(cond)?` in tests — returns `Result<(), string>`.

## Module resolution

1. Virtual modules (`prelude`, `io`, `ffi`, …) — compiler-owned.
2. `mod name;` or `use` of user module → disk file relative to project/`coil.toml`.
3. Entry script namespace is empty; other files get prefix from path.

`use foo::bar as b;` — `as` on concrete imports and brace items.

## Attributes

| Attribute | Target |
|-----------|--------|
| `#[derive(Show, Eq, Ord)]` | `enum`, `class` |
| User `attr` | `fn`, methods, `class` — must forward `...args` to `target(...args)` |

Tests are `test("desc") { … }` statements, not `#[test]` on `fn`.

## FFI quick patterns

Compile-time (no `use ffi`):

```coil
extern "c" {
    fn strlen(string s) -> int;
}
```

Runtime:

```coil
use ffi::{dload, declare, invoke};
use ffi::types::{Int, Ptr};

let lib = dload("libc.so.6")?;
let f = declare(lib, "strlen", (String,), Int)?;
```

## Iterator / for-in

`for x in expr` uses `IntoIterator` / `Iterator` from prelude. Works on arrays, homogeneous tuples/dicts, numeric ranges (`int`/`byte`/`float`), coroutines, user `impl`s. Numeric ranges also have inherent `.to_vec() -> Vec<T>`.

## What does NOT exist

See [not-builtins](https://github.com/ardax-corp/coil-website/blob/main/src/content/docs/references/not-builtins.md) (`/docs/references/not-builtins`): no general `sort`, string slice/trim builtins, `sin`/`sqrt`, HTTP in VM, manual alloc/free.

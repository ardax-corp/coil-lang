# coil language reference (agent cheat sheet)

Read when you need syntax detail beyond SKILL.md. User docs live in [coil-website](https://github.com/ardax-corp/coil-website) (`src/content/docs/`; routes `/docs/…` until a domain is set).

## Type system highlights

- HM inference with generics, traits, associated types/GATs, existentials.
- Type aliases: `type Name = T;` — lexically scoped; inner may shadow outer.
- `never` from `return`/`raise`/`panic` and proven-infinite loops absorbs in joins.
- Path completeness: concrete non-unit returns need all paths to exit (`E0111`).
- Casts: `expr as T` — see [casts](https://github.com/ardax-corp/coil-website/blob/main/src/content/docs/language/casts.md) (`/docs/language/casts`).
- Cast matrix (`expr as T`; `T` is `int`, `float`, `byte`, `bool` or a byte array; no sized integer types, no `string` / `char` target). `int` to `float` converts; `float` to `int` truncates toward zero, saturates at the `int` range and gives `0` for `NaN`; `int` to `byte` keeps the low 8 bits (`300` is `44`, `-1` is `255`); `int` to `bool` is `x != 0`; `byte` to `int` and `bool` to `int` widen (`true` is `1`). These pairs are `E0102`: `byte` to `float`, `byte` to `bool`, `float` to `byte`, `float` to `bool`, `bool` to `byte`, `bool` to `float`; go through `int` (`b as int as float`). A literal `int` outside `0..=255` `as byte` is a compile error. `Into` has the same pairs (`let f: float = n.into()`).
- Scalar-backed enums stay a nominal type (`Status`). In expression position they implicitly coerce to the backing (`let n: int = Status::Ok`, `Status::Ok + 1`). Every operator the backing type takes works on the backing value and gives the backing type: arithmetic, bitwise, shift, string `+`, unary `-` / `~` / `!`, `&&` / `||`, a condition, and `x += Status::Ok` on an `int` `x`. `s += 1` on a `Status` is an error (the result is an `int`). Two values of one enum that has the operator's instance (`impl Mul for Bit`) use the instance. Show of a scalar case is the backing (`Status::Ok` shows as `200`). No reverse coerce (`int` → `Status`) and no matching a raw `200` against a `Status` scrutinee.
- Payload enums are not heap-only: heap `Option<T>` / heap-heap `Result<T,E>` are pointer niches; `Option<int>` / immediate-Ok `Result` / arity-≤1 user payload enums use two words `[payload, tag]` on a *direct* call and return. Arity-2 immediate products use `[a, b]` ([#302](https://github.com/ardax-corp/coil-lang/pull/302)). Nested, `CallIndirect`, and unsure consumers stay boxed `ObjEnum`.

## Function values and calls that look alike

- A bare `Owner::m` is the static method's function value (`let f = Src::inc; f(4)`); the call needs `()` (`Src::inc(4)`). A bare trait method (`T::make`, `Make::make`) has no value: `E0119`. `let c: Counter = Counter::fresh;` is `E0102` (add `()`).
- `o.f(x)` on a field `f` of function type calls the function, unless the type has a method `f` (the method wins). `o.n(4)` on a field that is not a function is `E0104`.
- A ground annotation on a `let` of a generic function fixes its type: `let f: int -> int = id;` (`fn id<T>(T x) -> T`); `f("a")` is then a type error. Without an annotation, or with a `forall` one, `f` keeps its generic type.
- `==` on two bare-trait existentials (`let x: Tr = …`) needs an `Eq` the trait gives its values (`trait Tr<S: Eq>`); without it `E0119`. Values of different types are never equal.
- `coil test` takes `.coil` files and directories, several at once (`coil test a.coil dir/`).
- Nesting: `E0125` past 4000 nested brackets, or 2000 levels in the checker (a parenthesised group counts about two; `f(f(…))` one per call). On wasm the limits are 400 and 1000, and 700 chained operators.

## Show output (`%v`, `x.show()`, `Show::show(x)`)

One rule: a `string` is never quoted or escaped, alone or inside anything, and `%s` and `%v` print a string the same. So `(1, "a")` shows as `(1, a)`, `["a", "b"]` as `[a, b]`, `Some("a")` as `Some(a)`, `{x: 1, s: "a"}` as `{ s: a, x: 1 }` (fields in name order), and a derived `Show` class `Person { name, age }` as `Person { name: Ann, age: 3 }` (fields in declaration order). A payload enum shows as `Tok::Word(hi)`, `Tok::Pair(hi, 2)`. Output cannot tell `"1"` from `1`; build the quotes yourself (`"\"" + s + "\""`) when it matters. A bare `show(x)` is `E0153`.

## Expression forms

```
call        f(a, b, name: v)
index       arr[i]
field       rec.field (chained)
match       match e { pat => expr, … }   // catch-all is `default` only (not `_`)
if          if cond { … } else { … }
block       { stmts; expr }
lambda      fn (T x) use (y) => expr   // first-class fn values; named fns CALL without use
array lit   [1, 2, 3]
tuple       (a, b)
dict        { key: val, … }
enum ctor   Status::Ok, Option::Some(x), Color::Red
raise       raise "msg"        // catchable Result::Err (not process abort)
panic       panic "msg"        // abort process; CLI/embed exit 1 (Q5)
try?        expr?              // Result early return
default     expr ?? default
optional    expr?.field
range       0..10, 0..=10
for         for x in iter { … }
```

## Pattern forms

`match` and `if let` / `while let` take the same patterns; `let` and `for` take another set.

| Pattern | `match`, `if let`, `while let` | `let`, `for` |
|---------|-------------------------------|--------------|
| Identifier (binds) | yes (`k => …` is a catch-all arm) | yes |
| `_` | only nested (`E::B(_)`); a whole arm `_ =>` is `E0216`, use `default` | yes, also nested (`let (a, _) = t`) |
| `default` | whole-arm catch-all, `match` only | no |
| Int literal, negative too (`3`, `-3`, `E::B(3)`) | yes | no |
| String, bool, range (`1..5`), or-pattern (`1 \| 3`), guard (`x if x > 2`) | no (a bool is `if` / `else`) | no |
| Enum case `E::A`, `E::B(p, q)`, nested (`Option::Some(Option::Some(x))`) | yes | no (`let E::B(x) = e` is a parse error) |
| Record variant `E::R { x, y }`, `E::R { x: 1, y: b }`, `{ f: pattern }` | yes | no |
| Tuple `(a, b)`, nested `(a, (b, c))` | **no**, top level or inside a case's payload | yes |
| Record `{ x, y }`, `{ x: px, y }` | **no** | yes, an anonymous record only (a `class` is `E0119`) |

Matching a tuple or an anonymous record is a `let`: `let (a, b) = t;` and then `match` on `a` and `b`, or `match` on an enum that holds them. A bare `Some(x)` / `None` pattern works where it is unambiguous (`Option::Some(x)` always).

## IO patterns

```coil
use io::{stdout, open, close};
use io::sync::{println, write_all, read_to_end};   // coil-stdlib
use string::{format, to_bytes};

// Write formatted text (`print` for no newline)
let _ = println(format("%s %i", "hi", 42));
// Raw bytes to a stream
let _ = write_all(stdout(), to_bytes("raw"));

// Read file to bytes
let s = open("path", "r")?;
let buf = read_to_end(s)?;
close(s)?;
```

Buffers are `Vec<byte>`, not `string`. Convert via `string::to_bytes` / `from_bytes`.

Sync adapters exist for blocking-style loops on non-blocking streams — see [io](https://github.com/ardax-corp/coil-website/blob/main/src/content/reference/io/index.md) (`/docs/reference/io`).

## Error handling patterns

```coil
fn fallible() -> Result<int, string> {
    if bad { return Result::Err("oops"); }
    return Result::Ok(42);
}

fn caller() -> Result<int, string> {
    let x = fallible()?;   // propagate Err
    return Result::Ok(x + 1);
}

match opt {
    Option::None => 0,
    Option::Some(v) => v,
}
```

Constructor names are per-enum. Qualified form is canonical (`Status::Ok`, `Result::Ok(x)`, `Option::Some(x)`). Bare `Some`/`None`/`Ok`/`Err` is prelude sugar only when a single in-scope enum owns that case; otherwise it is a compile error and you must write `Enum::Case`.

`assert!(cond)` / `assert!(cond, msg)` in tests (`use tests::assert;`, coil-stdlib) — raises `Err` when `cond` is false.

`raise` is catchable (`Err` + return). `panic` aborts; checksum / CLI boards
use `panic` so a `try`/`?` cannot swallow the failure (Q5).

## Module resolution

1. Virtual modules (`prelude`, `io`, `ffi`, …) — compiler-owned.
2. `mod name;` or `use` of user module → disk file under a `--root` search root.
3. Entry script namespace is empty; other files get prefix from path.

`use foo::bar as b;` — `as` on concrete imports and brace items.

## Attributes

| Attribute | Target |
|-----------|--------|
| `#[derive(Show, Eq, Ord, Hash)]` | `enum`, `class` — composes with `#[repr(int)]` (etc.) on scalar-backed enums. Eq/Hash/Ord/Show use the backing word (`Status::Ok` shows as `200`). Traits that cannot be derived on a payload enum also fail on a scalar enum. |
| `#[repr(int)]` / `float` / `string` / `bool` | `enum` — scalar-backed cases (`Status::Ok = 200`); omitted when every case is the same simple literal type. Runtime is the unboxed literal; the type is still the enum and coerces to the backing in expression position |
| Attribute macro `#[name(...)]` | `fn`, methods, `class`, `enum` — declared `attr name(FnDecl f, …) -> Code` / `attr name(TypeDecl t, …) -> Code` in another module; returns the code that replaces the item. Stacked ones apply outermost first |
| User derive `#[derive(X)]` | `class`, `enum` — `derive X(TypeDecl t) -> Code attrs(helper)` in another module; `#[helper(...)]` on fields / variants |
| Function-style macro `name!(…)` | expression, statement or top-level item — `macro name(Expr a, …) -> Code` in another module; each argument is an ordinary expression, a last `Vec<Expr>` takes the rest |

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

`for x in expr` uses `IntoIterator` / `Iterator` from prelude. Works on arrays, homogeneous tuples/dicts, numeric ranges (`int`/`byte`/`float`), coroutines, user `impl`s, and a value of a type parameter bounded by `Iterator` (`fn f<I: Iterator>(I it) { for x in it { … } }`, through the bound's dictionary; it advances `it`). A type parameter bounded only by `IntoIterator` is E0119: no bound says what `T::IntoIter`'s `next` is. Numeric ranges also have inherent `.to_vec() -> Vec<T>`.

## What does NOT exist

See [not-builtins](https://github.com/ardax-corp/coil-website/blob/main/src/content/docs/language/not-builtins.md) (`/docs/language/not-builtins`): no general `sort`, string slice/trim builtins, HTTP in the runtime, manual alloc/free.

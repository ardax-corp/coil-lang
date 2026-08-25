# Types reference

coil uses **Hindley–Milner (Algorithm W)** type inference with optional annotations. Types are checked once per program before codegen; the compiler caches `(NodeId → Ty)` for opcode selection (e.g. `ADD` vs `ADDF`).

---

## Primitive types

| Name | `Ty` representation | Notes |
|------|---------------------|-------|
| `int` | `Ty::Con("int")` | 64-bit signed integer at runtime |
| `float` | `Ty::Con("float")` | IEEE double |
| `byte` | `Ty::Con("byte")` | Integer in `0..=255`; same immediate `Value` shape as `int` |
| `string` | `Ty::Con("string")` | Heap-allocated UTF-8 string |
| `bool` | `Ty::Con("bool")` | `true` / `false` |
| `void` / `unit` | `Ty::Con("unit")` | Used for statements with no value; FFI `void` return |

Primitive names in annotations are matched **case-insensitively** (`Int` ≡ `int`).

Integer literals coerce to `byte` when the expected type is `byte` (returns, annotated `let`s, call args) or `Vec<byte>` / `[byte; N]` array elements, and the value is in `0..=255`. Under an expected `byte`, arithmetic of such literals (e.g. `return 1 + 1;` in a `-> byte` function) also types as `byte`. Unannotated `int` variables still do not coerce (`let x = 42; return x;` needs `let x: byte`).

**Single-byte string literals** also coerce to `byte` in those same expected-type positions (and in `b == "/"`-style comparisons): the literal’s UTF-8 encoding must be exactly one byte after escapes (`"/"`, `"\n"`, `"\""`). Multi-byte characters (e.g. `"é"`) and non-literal `string` values do not coerce to `byte`.

**String literals** also coerce to `Vec<byte>` / `[byte; N]` (UTF-8 bytes) under an expected byte-buffer type, via `"…" as Vec<byte>`, and as `Vec<byte>` call arguments (e.g. `write_all(stdout(), "hi")`). For `[byte; N]` the decoded length must be exactly `N`. Non-literal strings may use `s as [byte]` (lowers to `to_bytes`); use `to_bytes(s)` for `Vec<byte>` (`s as Vec<byte>` is a compile-time error). Fixed `[byte; N]` still needs a literal.

`byte` implements `Show` and `Eq`; it is not in `Num` / `Add` yet.

Strings support `+` / `+=` with other strings. `string::format(...)` returns `string` and validates literal format specifiers (`%i` accepts `byte`).

Opaque **`Stream`** (`Ty::Con("Stream")`) is the handle type for the virtual [`io`](io.md) module — not constructible in userland.

---

## Type constructors (`Ty::Con`)

Any identifier that is not a primitive becomes an opaque type constructor. User-defined **class** names register as `Ty::Con(name)` in the entry file and `Ty::Con("module::Name")` when declared in a named module, so `use lib::{Foo}` binds the class type (not a dummy var) and two packages can both export `class Client`.

Recursive enum references use isorecursive `Con(name)` inside variant payloads (not unfolded `Sum`), so recursive types like `Tree` are expressible without infinite-type errors during inference.

---

## Built-in `Option` and `Result`

The compiler pre-registers polymorphic sum types under the virtual `prelude` module (auto-imported). **Do not redeclare them** while the short name is still bound — a user `enum Option` / `enum Result` is a duplicate-enum error unless you first rebind with `use prelude::Option as …`.

| Type | Variants (tag order) | Annotation |
|------|----------------------|------------|
| `Option` | `None` (0), `Some(T)` (1) | `Option` / `Option<T>` |
| `Result` | `Ok(T)` (0), `Err(E)` (1) | `Result` / `Result<T, E>` |

Payload types are inferred at use sites (`Option::Some(1)` → `Option` of `int`). Error-handling operators (`raise`, `?`, `??`, `?.`) are documented in [Operators](operators.md) and [Tutorial 09](../manual/tutorial/09-error-handling.md).

**Result mode:** a function that uses `raise` or Result-`?` (or is annotated `-> Result<…>`) has return type `Result<T, E>`; success `return` values are implicitly wrapped as `Ok`. Explicit `return Result::Ok(v)` / `return Result::Err(e)` are also accepted. One `E` per function.

### Option / Result runtime ABI

User code always sees `Option<T>` / `Result<T, E>`. Codegen picks one of three representations; conversions happen at the boundary. **Decision ([COI-92](https://linear.app/ardax/issue/COI-92)):** keep this matrix — unifying would either box every ground `Option<string>` or invent a niche for `int` / nested / FFI payloads.

| Shape | Representation | When |
|-------|----------------|------|
| Ground `Option<T>` whose `T` is a non-null heap pointer (`string`, class, heap aggregate) | Pointer niche: `0` = `None`, payload pointer = `Some` | Locals and direct values at a known ground type |
| Statically known unary `Option` / `Result` **call return** | Two-slot `[payload, tag]` (`ReturnPair`) | Callee and caller both see a unary enum return; not a coroutine; not a pointer-niche `Option` |
| Everything else | Boxed heap enum | Generic / nested-nullable / stack-shaped / coroutine / FFI / `CallIndirect` / unknown host |

Cross a niche ↔ boxed boundary with `OptionNicheToHeap` / `HeapOptionToNiche`. `Vec::pop` / `Vec::remove` use allocation-free `HostInvokeNiche` when the item type is heap-only; other host results stay on `HostInvoke` and convert at the boundary.

Do not match on raw `0` vs pointer in user code — `match` / `?` / `??` are the API. See [limitations](../internals/limitations.md).

---

## Function types (`Ty::Fun`)

Curried internally: `int -> int -> int` means `(int, int) -> int`.

```coil
fn add(int a, int b) -> int { return a + b; }
// add : int -> int -> int
```

---

## Sum types / enums (`Ty::Sum`)

Declared with `enum Name { variants }`:

```coil
enum Tree {
    Leaf,
    Node(int, Tree, Tree),
}
```

Internal shape (illustrative):

```
Ty::Sum {
    name: "Tree",
    variants: [
        ("Leaf", Unit),
        ("Node", Tuple([int, Con("Tree"), Con("Tree")])),
    ],
}
```

### Runtime representation

User code always sees constructors and `match`. At runtime, enums are heap objects (`MakeEnum`). Codegen may skip that allocation only for a discarded constructor (`MakeEnum; POP`) or a unary variant immediately consumed by `Unpack` / `LoadField(0)`. Wider payloads, values that escape, and control-flow joins stay heap-backed — a DCE ceiling, not a second enum ABI ([COI-94](https://linear.app/ardax/issue/COI-94)). Named-local class unboxing is a different rule ([COI-84](https://linear.app/ardax/issue/COI-84)). Builtin `Option` / `Result` have their own niche / pair / boxed matrix ([Option / Result runtime ABI](#option--result-runtime-abi)).

### Generic enums (`Ty::App`)

User enums may take type parameters. Annotations and construct/match use the same `Ty::App` machinery as builtin `Option` / `Result`:

```coil
enum Box<T> {
    Empty,
    Full(T),
}

fn unwrap(Box<int> b) -> int {
    return match b {
        Box::Empty => 0,
        Box::Full(v) => v,
    };
}
```

`Box::Full(7)` has type `Box<int>`. Payload types are freshened per construct/match site from the enum's schema (type-param placeholders in the registry).

### Variant payload shapes (`EnumVariantPayloadTy`)

| Shape | Syntax example | Internal |
|-------|----------------|----------|
| Unit | `None` or `None()` | `Unit` |
| Tuple | `Some(int)` | `Tuple([int])` |
| Record | `Point { x: int, y: int }` | `Record([("x", int), ("y", int)])` |

Constructors in expressions and patterns use qualified form: `Option::Some(42)` (builtin), `Point::Point { x: 1, y: 2 }` (user enum).

### Constructor types (`Ty::Constructor`)

Applying a variant yields a constructor type carrying tag and arity. Unification
joins constructors with their parent sum (or applied `Ty::App` for polymorphic
enums). Distinct tags of the same enum also join at the owner, and binding a
type variable peels the refinement — so `min(Rank::Mid, Rank::Low)` and
`[Rank::Low, Rank::Mid]` type-check as the parent enum without annotations.

---

## Tuples (`Ty::Tuple`)

Heterogeneous fixed-length products:

```coil
let t = (1, "hi", true);   // (int, string, bool)
fn pair(int a, string b) -> (int, string) { return (a, b); }
```

Annotation: `(T1, T2, ...)`. Literal syntax requires a comma: `(1,)` is a 1-tuple; `(1)` is a parenthesized expression.

Tuples have structural `Show` support for `%v` when every element is showable. The printed form is `(a, b)` (and `(a,)` for a 1-tuple).

Homogeneous numeric tuples also support **element-wise arithmetic** (zip /
broadcast / unary `-`). See [Operators](operators.md).

---

## Arrays (`Ty::Array`) — `[T; N]` only

Homogeneous **fixed-length** arrays. Length `N` is part of the type and is
inferred from literals when possible; otherwise write it explicitly.
Annotation `[T]` (no length) is a type error (`E0119`) — use `[T; N]` or
growable [`Vec<T>`](arrays.md).

| Annotation | `ArrayLength` | Example |
|------------|---------------|---------|
| `[T; N]` | `Static(N)` | Literal `[1, 2, 3]` infers `[int; 3]` |
| `[T]` | — | **Rejected** — use `[T; N]` or `Vec<T>` |

```coil
let xs = [1, 2, 3];           // [int; 3]
let zs: [int; 3] = [0, 0, 0];
fn sum([int; 3] arr) -> int { /* ... */ }
```

Locals of type `[T; N]` occupy **N consecutive frame slots** (stack). Escaping
into a single-value context (call, return, store into a heap object) boxes into
a non-growable heap array.

Empty `[]` needs an annotation: `Vec<T>` or `[T; 0]`. There is **no**
`arr[] =` append — use `Vec::push` (rejected append is `E0107`).

`len(value)` returns length as `int` for arrays, `Vec`, strings, tuples, and
dicts (structural); for other types it requires a `Length` instance
(`impl Length for T { fn len(T x) -> int { … } }`). Fixed-size cases fold at
compile time.

### `Vec<T>` — growable vectors

```coil
let v: Vec<int> = Vec::new();
v.push(1);
v.push(2);
```

Statics: `Vec::new`, `Vec::with_capacity`, `Vec::from`. Methods: `push`, `pop`,
`insert`, `remove`, `clear`, `reserve`, `capacity`, `len`, plus `v[i]` /
`v[i] = x`. Rest parameters `T... xs` pack into `Vec<T>`. IO buffers use
`Vec<byte>`. Full table: [Arrays and Vec](arrays.md).

### Indexing

| Target | Compile-time index | Runtime index |
|--------|-------------------|---------------|
| Fixed array `[T; N]` | OOB literal → diagnostic | Allowed (no static check); OOB load yields `-1`, OOB store is a no-op |
| `Vec<T>` | N/A | Allowed (no OOB diagnostic); same `-1` / no-op VM contract |
| Tuple | OOB literal → diagnostic | — |
| Non-aggregate | Error | — |

Proven counted-loop `Index` / `StoreIndex` rewrite to unchecked opcodes ([#192](https://github.com/ardax-corp/coil-lang/pull/192); original [COI-85](https://linear.app/ardax/issue/COI-85) "Index stays checked" decision is superseded). Dynamic indices stay checked. Prefer `i < len(a)` in loops (`LEQ`/`GEQ` are not proofs). Details: [Arrays and Vec](arrays.md#out-of-range-index).

---

## Records / dicts (`Ty::Record`)

Anonymous structurally typed records:

```coil
let d = { x: 1, y: 2 };   // { x: int, y: int }
let n = d.x;              // field access
```

- Two record literals with the same field names and compatible field types unify structurally.
- Field access on records uses string-keyed `GetField`; enum record variants use index-based `LoadField`.
- Duplicate field names in one literal → parse error (`E0208`); typecheck repeats the check if parse is bypassed.
- Anonymous records have structural `Show` support for `%v` when every field is showable. Fields print in canonical name order as `{ a: 1, b: 2 }`.

Structural `Show` covers tuples and anonymous records automatically. Non-generic
enums and classes receive a **default** `Show`/`String` that returns the type name
as a string (same display as `typeof self` for non-generic types)
(the type's fully-qualified name). Prefer `#[derive(Show)]` for structural field
formatting (see [Trait derive](#trait-derive)), or write an explicit `impl Show for T`.

---

## `typeof`

`typeof expr` is a compile-time query: it typechecks `expr`, formats the ground
type as a fully-qualified name, and lowers to an ordinary string constant. The
operand is **not** evaluated at runtime.

```coil
typeof 42                 // "int"
typeof (1, 2)             // "(int, int)"
typeof Option::Some(1)    // "prelude::Option<int>"
```

Open / unsolved types (free type variables, `never`, bare `Option::None`
without an annotation) are a compile error. Nominal types include their defining
module when non-empty (`prelude::Option<int>`, `math::Point`); entry-file types
stay bare (`Point`).

---

## Static slots

Module-level and class-level singletons share one global slot table for the process.

```coil
static let hits = 0;
static const VERSION = "1.0";

class Counter {
    static count: int = 0,
    value: int,
}

impl Counter {
    static fn fresh() -> Counter {
        Counter::count = Counter::count + 1;
        return new Counter(0);
    }
}
```

| Form | Access | Mutation |
|------|--------|----------|
| `static let x = …` | bare `x` in defining module | Reassignable |
| `static const X = …` | bare `X` | Immutable after init |
| `Class::field` | `Counter::count` | `static let`-style unless `static const` |
| `static fn` | `Class::method()` | No `self`; not callable on instances |

Initializers run once in the program prologue (declaration order). Other modules import statics via `use` like ordinary items.

Class `static` fields require an initializer. `static` and `const` field modifiers are mutually exclusive on class fields.

---

## Readonly types

`readonly` seals a value against **external** mutation. Methods may still mutate via `self`.

```coil
let xs = readonly [1, 2, 3];
let p = new readonly Point(1, 2);
// sugar: readonly new Point(1, 2)
```

| Operation | `readonly T` handle | Inside `impl` via `self` |
|-----------|-------------------|---------------------------|
| Read fields / index | Allowed | Allowed |
| `p.field = …` / `xs[i] = …` | Error | Allowed for class fields |
| Rebind variable | Allowed (`let a = readonly [1]; a = readonly [2];`) | — |

Type pretty-print: `readonly T`. Arrays and dicts have no method exception — external `StoreIndex` / field writes are rejected.

---

Inherent `fn drop()` on a class is a GC-time finalizer (`unit` return, implicit `self` by value). See [`gc`](gc.md). Named locals are always heap-allocated (`InitTyped`); only a consumed `new Class(args).field` may skip the box, and never when the class has `fn drop()` ([COI-84](https://linear.app/ardax/issue/COI-84)).

## Class `const` fields

```coil
class Point {
    const x: int,
    const y: int,
}
```

Const fields may be set only in `new` (constructor). Any later `p.x = …` or `self.x = …` is a type error — stricter than `readonly`, which allows method bodies to mutate via `self`.

Local `const` bindings are shallow: the compiler warns when the initializer type is heap-mutable (`Vec<T>`, class instances, dicts) because interior mutation through fields or indexed writes still succeeds. Fixed `[T; N]` locals are multi-slot and warn when the array element type is itself heap-mutable.

---

## Type aliases (`type Name = T;`)

Substituted at typecheck time; zero runtime cost. Parametric aliases expand when applied:

```coil
type UserId = int;
type IntPair = (int, int);
type Pair<T> = (T, T);

fn id(UserId x) -> UserId { return x; }

fn main() {
    let p: Pair<int> = (3, 4); // same as `(int, int)`
}
```

| Property | Behavior |
|----------|----------|
| Scope | Lexical: program, function, and block scopes |
| Shadowing | Inner scopes may shadow outer aliases |
| Duplicate names | Duplicate alias in the same scope is a diagnostic |
| RHS | Any `type_annotation` form |
| Type parameters | `type Pair<T> = (T, T);` — `Pair<int>` expands to `(int, int)` |

---

## Type annotation syntax (all contexts)

| Context | Example |
|---------|---------|
| Function parameter | `fn f(int x, [string; 4] rows) -> bool` |
| Return type | `-> (int, int)` |
| `let` binding | `let x: int = 1;` |
| Enum variant payload | `Some(int)`, `Node { left: Tree, right: Tree }` |
| Type alias RHS | `type A = [int; 4];` / `type Buf = Vec<byte>;` |
| Class field | `name: string` |

Forms:

```
IDENT                         // int, MyEnum, Foo, Vec
IDENT '<' type (',' type)* '>' // Vec<T>, Option<T>, …
'[' type ';' INT ']'          // [T; N] — length required
'(' type (',' type)+ ')'      // tuples — at least two components in type position
```

Bare `[T]` (no `; N`) is rejected at typecheck (`E0119`).
---

## Inference highlights

| Feature | Behavior |
|---------|----------|
| Let-polymorphism | `let`-bound names generalize free type variables at binding site |
| Function recursion | Monomorphic recursion — `fn` body sees monomorphic self type |
| `match` exhaustiveness | Checked post-inference; non-exhaustive match → diagnostic |
| Format strings | `string::format("%i", x)` validates specifier vs argument type |
| `impl` methods | `self` is implicit first parameter of owner class type |
| API style | Prefer inherent/`impl` methods over free functions for type-tied ops (`m.insert` not `insert(m)`); virtual-module host primitives (`io::read`) stay free fns |

---

## Unification rules (summary)

Unification is structural (Robinson) with an occurs check.

| Left | Right | Result |
|------|-------|--------|
| Same `Ty::Var` | | Success |
| Same `Ty::Con` name | | Success |
| `Ty::Con(n)` | `Ty::Sum { name: n, .. }` | Isorecursive expand-and-unify |
| `Ty::Fun` | `Ty::Fun` | Unify args, then returns |
| `Ty::Tuple` | `Ty::Tuple` | Same length; unify each element |
| `Ty::Array` | `Ty::Array` | Unify elements; lengths compatible if either is `Dynamic` or both `Static` with same N |
| `Ty::Record` | `Ty::Record` | Same fields (sorted by name); unify each field type |
| `Ty::Sum` | `Ty::Sum` | Same enum name; same variant names, shapes, arities; unify payload types |
| `Ty::Constructor` | `Ty::Sum` / other constructor | Owner must unify; distinct tags of the same enum join at the parent (refinements peel when binding a `Ty::Var`) |
| `Ty::Var` | anything | Bind variable (if occurs check passes) |
| Otherwise | | `Type mismatch` error |

### Length compatibility (arrays)

```
Dynamic  ~  Static(N)   ✓
Dynamic  ~  Dynamic     ✓
Static(N) ~ Static(M)    ✓ (element types must unify; N and M need not match for unification of annotation vs literal in all cases — mismatched static lengths error when both static)
```

---

## Coroutine types (`coroutine<Y, S>`)

`async fn` bodies return a handle typed as `coroutine<Y, S>`:

| Parameter | Meaning |
|-----------|---------|
| `Y` | Type **yielded out** on each `yield expr` |
| `S` | Type **sent in** on `resume h with v` and received by `let x = yield e` |

When no binding-yield or send sites exist, `S` defaults to `unit` and diagnostics print `coroutine<Y>`.

```coil
async fn counter() -> coroutine<int> {
    yield 0;
    yield 1;
}

async fn ping() -> coroutine<string, string> {
    let msg = yield "ready";
    yield msg;
}
```

Resume expression type: if `h : coroutine<Y, S>`, then `resume h` has type `Y`, and `resume h with v` requires `v : S`.

`resume` has a single static result type (`Y`) covering BOTH the value
yielded by each `yield expr;` AND the value produced when the body
completes (`return expr;`, or falling off the end). A `return expr;`
inside an `async fn` therefore unifies `expr`'s type against the SAME
`Y` as every `yield` in that body — not `unit` — so the returned value
is not discarded:

```coil
async fn counter() {
    yield 1;
    yield 2;
    return 42; // completion value, type unifies with the `yield`s above
}

fn main() {
    let h = counter();
    resume h; // 1
    resume h; // 2
    resume h; // 42 (the `return` value)
    resume h; // 0  (Done — see below, NOT 42 again)
}
```

Resuming an already-`Done` coroutine always yields `Value::default()`
(`0`/equivalent), never the coroutine's last `return` value — there is
no error-handling protocol yet to signal "resumed after completion",
so a fixed sentinel keeps the behavior well-defined instead of leaking
a stale value.

---

## Generics and traits

Generic functions use an optional type-parameter list and trait bounds on parameters:

```coil
use io::{stdout, write_all};
use string::{format, to_bytes};
fn add<T: Num>(T a, T b) -> T { return a + b; }

fn main() {
    write_all(stdout(), to_bytes(format("%i", add(3, 4))));   // int
    write_all(stdout(), to_bytes(format("%f", add(1.5, 2.5)))); // float
}
```

Multi-parameter traits use a trailing `where` clause:

```coil
trait Convert<A, B> { fn cast(A x) -> B; }
impl Convert<int, int> { fn cast(int x) -> int { return x; } }
fn apply_cast<A, B>(A x) -> B where Convert<A, B> { return cast(x); }
```

Binder bounds (`T: Num`) remain the short form for unary classes; they desugar to
the same constraint shape as `where Num<T>`.

### Kinds

Kinds classify type-level parameters:

| Kind | Meaning | Example |
|------|---------|---------|
| `*` | A proper value type | `T: *` |
| `* -> *` | Unary type constructor | `F: * -> *` |
| `* -> * -> *` | Binary type constructor | `F: * -> * -> *` |
| `(* -> *) -> *` | Higher-order type constructor | `F: (* -> *) -> *` |
| `Constraint` | A fully applied trait predicate | internal result kind |
| `* -> Constraint` | Unary constraint constructor | `c: * -> Constraint` |

Constraint-kind parameters let a generic abstract over the class predicate
itself:

```coil
fn choose<c: * -> Constraint, T: c>(T a, T b) -> int {
    if lt_val(a, b) { return 0; } // selects c = Ordered
    if eq_val(a, b) { return 42; } // Equal method via Ordered superclass
    return 1;
}
```

The function body still uses concrete dictionaries. Method use pins the abstract
constraint parameter to a concrete class such as `Show` or `Ordered`; the stored
function scheme then carries that concrete constraint, and call sites pass the
ordinary instance dictionary. If no method/operator/`%v` use selects a concrete
class, `T: c` is rejected as an unsatisfied abstract constraint.

### Syntax

| Form | Meaning |
|------|---------|
| `fn id<T>(T x) -> T` | Unconstrained type parameter `T` |
| `fn add<T: Num>(T a, T b) -> T` | `T` must satisfy the `Num` bound |
| `fn both<T: Num + Eq>(T x) -> T` | Multiple bounds (`+`) |
| `fn f<A, B>(A x) -> B where Convert<A, B>` | Multi-param (or unary) `where` constraint |
| `fn print_any(Show x)` | Bare unary trait name as an existential value type |
| `trait Container<F: * -> *>` | Unary type-constructor parameter |
| `trait Bifunctor<F: * -> * -> *>` | Binary type-constructor parameter |
| `trait Higher<F: (* -> *) -> *>` | Higher-order constructor parameter |
| `fn f<F: * -> * -> *, Bifunctor, A, B>(F<A, B> x)` | Explicit kind plus a class bound on one parameter |
| `fn f<c: * -> Constraint, T: c>(T x)` | Constraint-kind parameter and abstract bound |

### Call-site dispatch

One calling convention, two ways to name the callee. **`CALL`** packs arity and a static bytecode offset. **`CallIndirect`** pops the target from the stack (`CodePtr`, dictionary `Index`, or a `PolyFn` local). Dictionaries are the ABI for open bounds; they are not a second dispatch model.

| Situation | Bytecode |
|-----------|----------|
| Direct call to a function or instance method with a known entry | `CALL` |
| Ground trait method / UFCS (`x.m()` / `m(x)`) with a resolved instance | `CALL` to that instance method. A trailing dictionary is still passed (default / sibling ABI). Primitive `Num`/`Eq`/`Ord` operators further lower to opcodes (`ADD`, `EQ`, …); structural `len` may become `ArrayLen`. |
| Ground call to a generic whose bounds are only `Num`/`Add`/…/`Ord`/`Lt`/…/`Eq` | **Monomorphize** into a specialized clone (unboxed `ADD`, etc.). No dictionary at the call site. |
| Same, with named args and/or rest packs (`T...`) | Same monomorphization — args are reordered/packed to match formals before keying |
| Ground or open call with **user** trait bounds, or builtin `Show` / `Length` | **Dictionary passing** — `CALL` the shared generic body with trailing dict tuples |
| Open type params inside a generic body (any bound) | `LOAD __dictN`; `Index`; `CallIndirect` |
| Existential (`Show x`) | Unpack the dict from the value; `CallIndirect` |
| Escaped generic fn value (`let f = id;`) | `MakePolyFn` / `MakePolyFnCapture` + `CallIndirect` |

**Decision ([COI-78](https://linear.app/ardax/issue/COI-78)):** keep this split. Ground user-trait methods already share the static-entry `CALL` path with ground builtin methods. Extending generic-function monomorphization to user traits would recompile bodies that still carry dictionary `bound_method_call` hints, can leave open `Ty::Var` at call sites (`Show` / `Length`), and would not remove the dictionary ABI that default and sibling methods need. Caps, escaped `PolyFn`, and nested open bounds would still use dictionaries. There is no opcode to fuse a user method into, unlike `Num` → `ADD`.

### Dictionary passing

Constrained calls that are not monomorphized append one dictionary per trait constraint after the value arguments. Each dictionary is a `MakeTuple` of method code offsets in declaration order. The callee reserves trailing locals `__dict0`, `__dict1`, …, loads the matching method slot with `Index`, and invokes it with `CallIndirect`. A generic calling another generic with the same open bound forwards its existing dictionary. Builtin classes use compiler-generated primitive method thunks through this same ABI; ground monomorphization remains an optimization.

**Flattened superclass layout.** When a unary class declares a param bound
(`trait Ordered<T: Equal>`), those bounds are stored as *superclasses*.
The runtime dictionary for the subclass is flattened: subclass methods first,
then each superclass’s methods in declaration order (transitively). An
`impl Ordered<int>` therefore requires an existing `Equal<int>` instance — its
methods fill the trailing dict slots.

```coil
trait Describable<T> { fn describe_val(T x) -> int; }
impl Describable<int> { fn describe_val(int x) -> int { return x + 1; } }
fn show<T: Describable>(T x) -> int { return x.describe_val(); }
// show(42) → CALL arity = 2 (value + Describable dict)
```

### Bare-class existential types

A unary trait name in a value type position denotes an existential value:

```coil
use io::{stdout, write_all};
use string::{format, to_bytes};
fn print_any(Show x) {
    write_all(stdout(), to_bytes(format("%s", show(x))));
}

fn main() {
    print_any(42); // packs the int value with the Show<int> dictionary
}
```

This applies in function parameters, `let` annotations, and return annotations:
`Show x`, `let x: Show = 42;`, and `-> Show`.

`T: Show` is still universal: one function body works for every caller-chosen
`T` that satisfies the bound. A bare `Show` value is existential: the concrete
type is hidden after the pack site, and calls such as `show(x)` dispatch through
the dictionary stored with the value.

Rules:

- Only unary traits whose parameter has kind `Type` may be used this way.
  Multi-parameter classes such as `Convert<A, B>` are rejected as bare value
  types.
- If a concrete type constructor and a trait have the same name, the type
  constructor wins.
- Packing a concrete value requires exactly one matching instance; otherwise
  the call site reports `No instance for Class<T>`.
- Runtime representation reuses existing tuple bytecode: `(boxed_value,
  dictionary_tuple)`. No separate opcode is needed.

Bound methods support both equivalent forms:

```coil
x.describe_val(); // method sugar
describe_val(x);  // bare / UFCS form
```

Default methods occupy normal dictionary slots. Every implementation method
receives the active dictionary as a hidden trailing argument, so a default can
call a sibling method. An omitted default slot points at the class default body.

### Builtin traits

The compiler pre-registers these traits and instances for `int`, `float`, and (where applicable) `string` / `bool` / `unit`:

| Trait | Purpose | Operators / methods |
|-------|---------|---------------------|
| `Add` | Addition | `+` → `add` |
| `Sub` | Subtraction | `-` → `sub` |
| `Mul` | Multiplication | `*` → `mul` |
| `Div` | Division | `/` → `div` |
| `Num` | Convenience bundle | Supertrait of `Add` + `Sub` + `Mul` + `Div` (no own methods) |
| `Lt` | Less-than | `<` → `lt` |
| `Le` | Less-or-equal | `<=` → `le` |
| `Gt` | Greater-than | `>` → `gt` |
| `Ge` | Greater-or-equal | `>=` → `ge` |
| `Ord` | Convenience bundle | Supertrait of `Lt` + `Le` + `Gt` + `Ge` (no own methods) |
| `Eq` | Equality | `==`, `!=` |
| `Show` | Display | `show(T) -> string`; used by format `%v` |
| `Length` | Size query | `len(T) -> int`; used by `len(x)` for custom types (arrays/tuples/dicts/strings are structural) |
| `Into` | Conversion | `into(Self) -> T` via `impl Into<T> for Self` (no builtin instances) |

`Into` is multi-parameter: `impl Into<T> for S` stores instance args
`[S, T]`. Prefer method form with an expected type —
`let y: T = x.into();` — so the target pins constraint discharge. Open
`where Into<A, B>` helpers also work. See `examples/into.hy`.

On open/generic operands, operators require the matching op trait (or the
`Num` / `Ord` convenience supertrait). Concrete `int`/`float` arithmetic and
comparisons still use hardwired opcodes. String concatenation
(`string + string`) is a separate path and is **not** covered by `Num`/`Add`.

### Trait derive

Non-generic `enum` and `class` declarations may include `#[derive(...)]`
attributes that synthesize structural instances of builtin traits:

```coil
#[derive(Show, Eq)]
enum Point {
    Origin,
    Point { x: int, y: int },
}

#[derive(Show, Eq)]
class Cell {
    value: int,
}
```

| Trait | Synthesized methods | Strategy |
|-------|---------------------|----------|
| `Show` | `show` | Enum: `match` + `format` / string lits; class: field walk via `.field` |
| `Eq` | `eq`, `ne` | Tag + payload `==`; `ne` is `!(a == b)` |
| `Ord` | `lt`, `le`, `gt`, `ge` | Lexicographic on declaration order |
| `Default` | `default` | First enum variant / zero field values for classes |
| `Hash` | `hash` | Tag + recursive `field.hash()` mix (`* 31 + hash`); builtins for `int`/`byte`/`bool`/`float`/`string`/`unit`; nested `Hash` types recurse |
| `String` | `to_string` | `format` with `%v` per field |
| `Serialize` | `serialize` | `Vec<byte>` wire: tag byte + payload field bytes in order (enum) or fields only (class). **MVP:** each payload field is cast through `byte` (`as_byte` / `as_int`); values outside `0..=255` and non-byte types silently corrupt — use only small integer / `byte` fields until a real encoding exists |
| `Deserialize` | `deserialize` | Inverse of `Serialize` from `Vec<byte>`; invalid tag → `panic` |
| `Send` | _(marker)_ | Empty instance (thread spawn still uses structural sendability) |
| `Sensitive` | _(marker)_ | Empty instance (redaction hooks deferred) |

**Default display (no derive):** every non-generic `enum` / `class` that lacks
`#[derive(Show)]` / `#[derive(String)]` and has no explicit `impl` gets a
compiler-generated instance whose body returns the type name string. Explicit
`impl` and structural `#[derive(Show)]` take precedence (overlap with a manual
`impl` is still an error if both exist).

Rules:

- Placement: immediately before the `enum` / `class` keyword (after any `///` doc comment).
- Whitelist only: `Show`, `Eq`, `Ord`, `Default`, `Hash`, `String`, `Serialize`, `Deserialize`, `Send`, `Sensitive`. Unknown / arithmetic traits (`Num`, …) error.
- Generics (`#[derive(Show)] enum Box<T> { … }`) are rejected for now — write an explicit `impl`.
- Combining `#[derive(Show)]` with a hand-written `impl Show for T` hits the usual overlap diagnostic.
- Empty `#[derive()]` with no traits is a parse error.

See `examples/derive_show_eq.hy`, `examples/derive_hash.hy`, and `examples/typeof_len.hy`.

### User-defined traits (sketch)

Declare a trait and provide instances for concrete types. Prefer the
`impl Trait for Type` form; the legacy `impl Trait<Type>` form is still accepted.
For multi-parameter traits, `impl Trait<A, B> for T` prepends `T` as the first
type argument (Self slot), so it is equivalent to `impl Trait<T, A, B>`.

```coil
trait Measurable<T> {
    fn size(T x) -> int;
}

impl Measurable for int {
    fn size(int x) -> int { return x; }
}

// Legacy form (still OK):
impl Measurable<int> {
    fn size(int x) -> int { return x; }
}
```

Instance methods compile to ordinary functions with mangled names
(`Class__Type__method`). Generic call sites discharge the bound at
typecheck time and pass the matching dictionary at runtime (above).

### Instance coherence

Typeclass instances follow module-path ownership rules so dictionary
resolution stays deterministic across projects:

- `impl Class<T…>` is allowed when the current module defines `Class`.
- Otherwise, every non-variable instance argument must have a nominal
  head (enum, class, or type alias) defined in the current module.
- Builtin types (`int`, `float`, `string`, tuples, arrays, and records)
  are not local nominal heads. For example, a module that did not define
  `Show` cannot add `impl Show<(int, int)>`, and `impl Into<Wrapper> for int`
  is rejected unless `Into` itself is defined in the current module.
- Exact duplicates and instances whose heads unify with an existing
  instance are rejected.
- If constraint discharge ever sees two matching instances, it reports
  an ambiguous-instance error rather than selecting the first one.

### Associated types and GATs

A trait may declare associated types; each impl must define them. Associated
types may be nullary (`type Elem;`) or generic (`type Ref<T>;`, also called a
generic associated type / GAT):

```coil
trait Collect<C> {
    type Elem;
    fn head(C xs) -> Elem;
}

impl Collect<Option<int>> {
    type Elem = int;
    fn head(Option<int> xs) -> int {
        return match xs {
            Option::Some(v) => v,
            Option::None => 0,
        };
    }
}
```

- **In method signatures** inside the class, bare `Elem` (and `Collect::Elem` /
  `C::Elem`) resolve to the associated type. Method schemes quantify class
  parameters first, then any associated-type projection variables.
- **Impls** must define every associated type (`type Elem = …;`) and may not
  introduce unknown ones. Missing or extra assoc types are type errors. A GAT
  definition repeats its own binders, for example `type Ref<T> = T;`.
- **Projections** `Owner::Assoc` and applied GAT projections
  `Owner::Assoc<T, U>` are allowed in type annotations. When the
  owner is a type parameter with an active class bound that declares the
  assoc type (`fn take_head<C: Collect>(C xs) -> C::Elem`), the projection
  is an open type variable that is pinned when a ground instance is
  discharged at the call site (`take_head(Option::Some(42))` → `int`).
- **GAT arguments are kind-checked.** `type Ref<F: * -> *>;` requires applied
  projections such as `P::Ref<Option>` to pass a constructor-kinded argument,
  while `P::Ref<int>` is rejected.

```coil
trait Pointer<P: * -> *> {
    type Ref<T>;
    fn deref<T>(P<T> ptr) -> Ref<T>;
}

impl Pointer<Option> {
    type Ref<T> = T;
    fn deref<T>(Option<T> ptr) -> T { /* ... */ }
}

fn get<P: * -> *, Pointer, A>(P<A> ptr) -> P::Ref<A> {
    return deref(ptr);
}
```

Associated types are erased at runtime (no dictionary slot); they exist only
in the typechecker. See `examples/assoc_type.hy` and
`examples/gat_pointer.hy`.

### Superclasses and implied bounds

Unary trait parameter bounds declare superclasses:

```coil
trait Equal<T> { fn eq_val(T a, T b) -> bool; }
trait Ordered<T: Equal> { fn lt_val(T a, T b) -> bool; }

impl Equal<int> { fn eq_val(int a, int b) -> bool { return a == b; } }
impl Ordered<int> { fn lt_val(int a, int b) -> bool { return a < b; } }

// Implied Equal: no need to write `T: Ordered + Equal`
fn cmp_eq<T: Ordered>(T a, T b) -> bool {
    return eq_val(a, b);
}
```

- **Impl check:** `impl Ordered<int>` errors unless `Equal<int>` already exists.
- **Implied bounds:** an active constraint `Ordered<T>` covers `Equal<T>` for
  discharge and method resolution, so superclass methods are available under
  the subclass bound alone.
- **Dict slots:** `Ordered` dict = `[lt_val, eq_val]` (subclass then superclass).

Builtin `Ord` / `Eq` are independent (no superclass link) so existing builtin
dict layouts stay unchanged. Prefer a custom `Ordered` / `Equal` pair when you
need superclass semantics. See `examples/superclass_ord.hy`.

### First-class generic functions

A generic function can escape into a local `PolyFn` value and be instantiated
more than once:

```coil
fn id<T>(T x) -> T { return x; }
let f = id;
let n = f(42);
let x = f(4.0);
```

Unconstrained escapes use `MakePolyFn`. Constrained generics always escape via
`MakePolyFnCapture`: each constraint slot is filled from an in-scope `__dictN`
or a concrete instance dictionary when the type arguments are ground; only
truly unavailable evidence (for example top-level `let f = show;`) leaves a
null slot for application-time synthesis. Applications use `CallIndirect`,
which merges captured evidence with any dictionaries synthesized at the call
site (preferring captures for already-filled slots). A generic identifier
passed to a compatible `forall T. T -> T` parameter uses the same path.

### Higher-rank `forall`

Type annotations may use prenex / higher-rank quantification:

```coil
fn app(forall T. T -> T f, int x) -> int {
    return f(x);
}
```

`forall T: Num. …` carries constraints on the binder. When checking an
argument against a `forall` expectation, the checker skolemizes the
binder (rigid variables) and rejects escaping skolems. A polymorphic
generic function identifier (e.g. `id`) is compatible with a matching
`forall` parameter type.

See `examples/generics.hy`, `examples/typeclass_dict.hy`,
`examples/existential_show.hy`, `examples/hkt_container.hy`,
`examples/hkt_bifunctor.hy`, `examples/multiparam.hy`,
`examples/constraint_kind.hy`, `examples/superclass_ord.hy`,
`examples/assoc_type.hy`, `examples/gat_pointer.hy`, and
`examples/polyfn.hy` for runnable demos.

### Boxing and unboxing at generic boundaries

When a concrete value crosses into a generic function body, the compiler wraps it in a heap-allocated `ObjBoxed` cell (`BoxValue`). When the generic call returns a value whose type is concrete at the call site, the compiler immediately unpacks it back to a raw value (`UnboxValue`).

This means **most generic calls to primitive-returning functions are transparent** — the caller receives a plain `int`, `float`, `bool`, or `string`, not a boxed wrapper:

```coil
use io::{stdout, write_all};
use string::{format, to_bytes};
fn id<T>(T x) -> T { return x; }

fn main() {
    let n = id(42);   // n is a raw int — unboxed automatically
    write_all(stdout(), to_bytes(format("%i", n)));    // prints: 42
}
```

**Displaying open / generic values — use `%v`:**

Concrete format specifiers (`%i`, `%f`, `%s`, `%z`, …) require a resolved concrete type. An open type parameter is a type error; use `%v`, which requires `T: Show` and lowers through the `show` method to a string before formatting:

```coil
use io::{stdout, write_all};
use string::{format, to_bytes};
fn show_it<T: Show>(T x) {
    write_all(stdout(), to_bytes(format("%v", x)));   // ok — dictionary Show
}

fn main() {
    show_it(42);
    show_it("hi");
    let s = format("%v", 99);  // same lowering; leaves a string
    write_all(stdout(), to_bytes(format("%s", s)));
}
```

Builtin `Show` instances cover `int`, `float`, `string`, `bool`, and `unit`. User types can `impl Show<MyType>`. See `examples/generic_print.hy`.

---

## Known limitations

| Area | Limitation |
|------|------------|
| Type aliases | Lexically scoped (stack of frames); duplicate names in the same frame are rejected; inner scopes may shadow outer; parametric aliases (`type Pair<T> = …`) expand on application |
| Classes | Nominal `Ty::Con`; ctor args / fields / methods supported — no inheritance or virtual dispatch |
| FFI | Broad scalar/Ptr/struct/callback tags via `ffi::types` / `extern struct` — see [FFI tutorial](../manual/tutorial/07-ffi.md) |
| Generics | Generic functions/enums/aliases/classes, `T: Class` bounds, multi-param `where` constraints, `forall` annotations, user `trait`/`impl`, superclasses, orphan/coherence checks, associated types, and GATs are supported |
| Trait runtime | **Decided ([COI-78](https://linear.app/ardax/issue/COI-78)):** ground instance methods use `CALL`; generic user-trait / `Show` / `Length` bounds keep dictionaries. Only ground `Num`/`Ord`/`Eq` (and operator supertraits) monomorphize to opcodes. See [Call-site dispatch](#call-site-dispatch). |
| Option / Result ABI | **Decided ([COI-92](https://linear.app/ardax/issue/COI-92)):** pointer niche, two-slot call return, or boxed enum — see [Option / Result runtime ABI](#option--result-runtime-abi). |
| Enum runtime | **Decided ([COI-94](https://linear.app/ardax/issue/COI-94)):** heap objects; DCE may skip discarded or unary-unpack constructors only — see [Runtime representation](#runtime-representation). |
| Existentials | Bare class names are existential value types only for unary `* -> Constraint` classes; multi-param bare existentials and constructor-kinded bare existentials are rejected |
| Higher-kinded types | Constructor kinds such as `F: * -> *`, `F: * -> * -> *`, and `F: (* -> *) -> *` are supported; kind variables / kind polymorphism are not supported |
| Associated types | Nullary associated types and generic associated type projections are supported; associated-type equality constraints in `where` clauses are not syntax |
| Typeclass deriving | `#[derive(Show, Eq, Ord)]` on non-generic `enum` / `class` (see [Trait derive](#trait-derive)); user traits and generics need an explicit `impl` |
| Effect system | No linear/ownership types |
| Callback returns | Opaque `Ptr` address; re-invoke requires host/`declare` of the pointed-to symbol (no automatic trampoline) |
| Inner match patterns | Same outer tag with different inner tags — supported (Phase 18A); complex nested cases may still need careful arm ordering |
| `async fn` `-> T` annotation | When present, `T` is unified with the coroutine yield/return type `Y` (same slot as `yield` / `return` / `resume`). A mismatch is a type error. |

---

## Pretty-printed forms

Diagnostic messages render types roughly as:

| Internal | Display |
|----------|---------|
| `int` | `` `int` `` |
| `(int, string)` | `` `(int, string)` `` |
| `[int]` | `` `[int]` `` |
| `[int; 5]` | `` `[int; 5]` `` |
| `{ x: int, y: int }` | `` `{ x: int, y: int } `` |
| `Option` sum | `` `Option` `` with variant detail in specialized errors |

---

## Related documents

| Document | Contents |
|----------|----------|
| [Syntax](syntax.md) | Where annotations appear in grammar |
| [Operators](operators.md) | Arithmetic and comparison typing |
| [Built-ins](README.md) | FFI type tags |
| [Tutorial: Types](../manual/tutorial/02-types-and-variables.md) | Guided introduction |

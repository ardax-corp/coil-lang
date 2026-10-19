Hover docs for Coil keywords, contextual keywords and builtin statement forms.
Each entry starts with a `## word` line. Contextual words are only special in
the position their entry describes and stay ordinary identifiers elsewhere.

## fn

Declares a function, a method, or (written in an expression) an anonymous function.

A declaration lists typed parameters as `Type name`, an optional `-> ReturnType`
and a body. Generic parameters go in `<…>` after the name. In an expression,
`fn (int x) => x + 1` is a lambda; it captures outer locals only when they are
listed in `use (…)`.

```coil
fn add(int a, int b) -> int {
    return a + b;
}

fn main() {
    let k = 10;
    let add_k = fn (int x) use (k) => x + k;
    let n = add_k(add(1, 2)); // 13
}
```

## let

Declares a mutable local variable.

The type is inferred from the initializer, or written after a colon. A `let`
can destructure a tuple: `let (a, b) = pair;`. `if let` and `while let` test a
pattern instead.

```coil
fn main() {
    let count = 0;
    let name: string = "coil";
    let (x, y) = (1, 2);
    count = count + x + y;
}
```

## const

Declares a local binding that cannot be reassigned, or a class field that is set only by `new`.

A `const` local is shallow: the variable cannot be reassigned, but the
contents of a `Vec`, class instance or record it holds can still change (the
compiler warns about that). On a class field, `const` forbids any write after
construction, even from the class's own methods. `static const` declares a
module constant.

```coil
class Point {
    pub const x: int,
    pub const y: int,
}

fn main() {
    const limit = 100;
    let p = new Point(1, 2);
    let total = limit + p.x + p.y;
}
```

## static

Declares a module-level or class-level slot shared by the whole program, or a method without `self`.

`static let` is a reassignable module variable and `static const` a constant;
their initializers run once before `main`. In a class, `static count: int = 0`
is a class field read as `Class::count`. In an `impl` or trait, `static fn`
is a method called on the type (`Counter::fresh()`), never on a value.

```coil
static let hits = 0;
static const VERSION = "1.0";

class Counter {
    value: int,
}

impl Counter {
    pub static fn fresh() -> Counter {
        hits = hits + 1;
        return new Counter(0);
    }
}

fn main() {
    let c = Counter::fresh();
}
```

## readonly

Seals an array literal or a new class instance against mutation from outside.

`readonly [1, 2, 3]` and `readonly new Point(1, 2)` produce a `readonly T`
value: field writes and index writes through it are compile errors, while the
class's own methods can still change it through `self`. The variable itself
can still be rebound.

```coil
class Point {
    pub x: int,
    pub y: int,
}

fn main() {
    let xs = readonly [1, 2, 3];
    let p = readonly new Point(1, 2);
    let sum = p.x + p.y; // reading is fine; `p.x = 5;` would not compile
}
```

## if

Runs a block when a `bool` condition is true.

The condition needs no parentheses and the body always takes braces. Chain
alternatives with `else if` and `else`. `if let Pattern = value { … }` runs the
block when the value matches the pattern, binding its names.

```coil
fn sign(int n) -> int {
    if n < 0 {
        return -1;
    } else if n == 0 {
        return 0;
    } else {
        return 1;
    }
}

fn main() {
    let found = Option::Some(3);
    if let Option::Some(v) = found {
        let doubled = v * 2;
    }
}
```

## else

Introduces the branch of an `if` that runs when its condition is false (or its pattern does not match).

`else` is followed by a block or by another `if`, which forms an `else if`
chain.

```coil
fn describe(int n) -> string {
    if n % 2 == 0 {
        return "even";
    } else {
        return "odd";
    }
}
```

## while

Repeats a block while a `bool` condition is true.

`while let Pattern = value { … }` repeats while the value matches. A `while`
header can carry `invariant` and `decreases` clauses before its body. Use
`break` and `continue` to leave or restart the loop.

```coil
fn main() {
    let i = 0;
    let total = 0;
    while i < 5 {
        total = total + i;
        i = i + 1;
    }
}
```

## for

Iterates over the values of an array, `Vec`, range, coroutine or any `IntoIterator`.

`for x in expr { … }` pulls one value at a time; ranges such as `0..n` are
lazy and allocate nothing. C-style `for (init; cond; step)` loops do not
exist. In `impl Trait for Type`, `for` names the type the trait is
implemented for.

```coil
fn main() {
    let total = 0;
    for x in [1, 2, 3] {
        total = total + x;
    }
    for i in 0..10 {
        total = total + i;
    }
}
```

## in

Separates the loop variable from the iterated value in a `for` loop.

```coil
fn main() {
    let names = ["ada", "grace"];
    let letters = 0;
    for name in names {
        letters = letters + name.len();
    }
}
```

## break

Leaves the innermost `while` or `for` loop at once.

A `break` skips the loop's `invariant` check.

```coil
fn first_negative(Vec<int> xs) -> int {
    let found = 0;
    for x in xs {
        if x < 0 {
            found = x;
            break;
        }
    }
    return found;
}
```

## continue

Skips the rest of the current iteration and goes on with the next one.

In a `while` loop the condition is tested again; in a `for` loop the next
value is taken.

```coil
fn sum_odd(int n) -> int {
    let total = 0;
    for i in 0..n {
        if i % 2 == 0 {
            continue;
        }
        total = total + i;
    }
    return total;
}
```

## return

Leaves the current function, optionally with a value.

`return;` returns `unit`. In a function in result mode (one that returns a
`Result`, uses `raise` or uses `?`), a plain `return v` is wrapped as
`Result::Ok(v)`. Pending `defer` blocks run before the function returns.

```coil
fn clamp(int n, int lo, int hi) -> int {
    if n < lo {
        return lo;
    }
    if n > hi {
        return hi;
    }
    return n;
}
```

## raise

Returns early from the current function with `Result::Err(e)`.

Using `raise` puts the function in result mode: its successful returns are
wrapped in `Result::Ok`. Each function has one error type, so every `raise`
and `?` in it must agree on `E`. It works as a statement or an expression.

```coil
fn parse_age(int n) -> Result<int, string> {
    if n < 0 {
        raise "age cannot be negative";
    }
    return n;
}
```

## panic

Aborts with a message: `panic "message"`.

The message is printed as `panic: <message> at <file>:<line>:<col>` and the
program exits with status 1. The `defer` blocks of the functions it leaves
still run. A panic inside a child task fails that task and its scope instead
of the program. Prefer `raise` or `Result` for errors a caller can handle.

```coil
fn checked_div(int a, int b) -> int {
    if b == 0 {
        panic "division by zero";
    }
    return a / b;
}
```

## typeof

Gives the static type of an expression as a `string`, at compile time.

The operand is type-checked but never evaluated. Nominal types are written
with their defining module (`prelude::Option<int>`); types from the entry
file stay bare. A type that is not fully known is a compile error.

```coil
fn main() {
    let a = typeof 42; // "int"
    let b = typeof (1, "x"); // "(int, string)"
    let c = typeof Option::Some(1); // "prelude::Option<int>"
}
```

## enum

Declares a sum type: a value is exactly one of its variants.

Variants can carry no data, a tuple payload or named fields. Build them with
`Enum::Variant` and take them apart with `match`. With `#[repr(int)]` (or
`string`, `float`, `bool`), each variant is backed by a literal (`Ok = 200`).
An enum may be generic.

```coil
enum Shape {
    Circle(float),
    Rect { w: float, h: float },
    Empty,
}

fn area(Shape s) -> float {
    return match s {
        Shape::Circle(r) => 3.14 * r * r,
        Shape::Rect { w, h } => w * h,
        Shape::Empty => 0.0,
    };
}
```

## match

Compares a value against patterns and evaluates the first arm that matches.

Arms are `pattern => expression` separated by commas. Patterns can be enum
variants (with nested payload patterns), integer literals, bindings, and `_`
inside a payload. The arms must cover every case; `default =>` catches the
rest. `match` is an expression, so it yields a value.

```coil
fn label(Option<int> code) -> string {
    return match code {
        Option::Some(200) => "ok",
        Option::Some(_) => "other",
        Option::None => "none",
    };
}
```

## default

The catch-all arm of a `match`: `default => …`.

It matches anything the earlier arms did not. It must be a whole arm: a
whole-arm `_ =>` is an error (`_` only works inside a payload, such as
`Option::Some(_)`). A `match` can have one `default`.

```coil
fn status_text(int status) -> string {
    return match status {
        200 => "ok",
        404 => "missing",
        default => "other",
    };
}
```

## type

Declares a type alias, or an associated type in a trait or `impl`.

`type Name = T;` gives an existing type a new name; the alias is replaced by
its type at compile time and may take type parameters. Inside a trait,
`type Elem;` declares an associated type that each `impl` defines with
`type Elem = int;`.

```coil
type UserId = int;
type Pair<T> = (T, T);

fn swap(Pair<UserId> p) -> Pair<UserId> {
    return (p[1], p[0]);
}
```

## use

Imports items from a module, or lists the outer variables a lambda or `defer` captures.

`use path::item;`, `use path::{a, b};` and `use path::item as alias;` bring
names into scope; `use path::*` is rejected. The prelude is imported
automatically. After a lambda's parameters or after `defer`, `use (x, y)`
names the outer locals the body may use.

```coil
use string::{format};
use io::sync::{println as say};

fn main() {
    let name = "coil";
    let greet = fn () use (name) => format("hello %s", name);
    let _ = say(greet());
}
```

## as

Renames an import, or converts a value between primitive types.

In `use path::item as alias;`, the item is bound as `alias`. In an
expression, `x as T` converts between `int`, `float`, `byte` and `bool`;
`float as int` truncates toward zero.

```coil
use io::sync::{println as say};

fn main() {
    let n = 7.9 as int; // 7
    let f = 3 as float; // 3.0
    let _ = say("done");
}
```

## mod

Loads a module file without importing any of its names: `mod name;`.

The file is looked up as `name.hy` in each source root. Use `use` to bind
items from it.

```coil
mod helpers;

use helpers::double;

fn main() {
    let n = double(21);
}
```

## extern

Binds C functions from a native library at compile time: `extern "lib" { fn …; }`.

Each declaration is a signature ending in `;`, called like an ordinary
function. The compiler loads the library and declares the functions for you,
panicking with a clear message if that fails. A trailing bare `...` marks C
varargs. The library stem is gated like `ffi::dload`: the build needs
`--allow-dload STEM`, and the run a `--dload-pin` or `--dload-trusted` for
it; `extern "c"` (libc) is always denied. `extern struct Name { … };`
declares a C struct passed by value.

```coil
extern "sum" {
    fn sum(int a, int b) -> int;
    fn sum_n(int n, ...) -> int;
}

fn main() {
    let n = sum(40, 2);
}
```

## class

Declares a class: a named heap type with fields, constructed with `new`.

Fields are `name: Type`, private unless marked `pub`; `const` fields are
set only by the constructor and `static` fields belong to the class. Methods
go in an `impl` block. A class can be generic and can declare an
`invariant`.

```coil
class Account {
    pub owner: string,
    balance: int,
}

impl Account {
    pub fn deposit(int amount) {
        self.balance = self.balance + amount;
    }
}

fn main() {
    let a = new Account("ada", 0);
    a.deposit(10);
}
```

## impl

Adds methods to a type (`impl Type { … }`) or implements a trait for it (`impl Trait for Type { … }`).

Methods have an implicit `self`; `pub` makes them callable from outside the
type, and `static fn` declares one that is called on the type. An inherent
`fn drop()` is a finalizer the garbage collector runs once.

A generic type's inherent impl declares its parameters, `impl Cell<T> { … }`
(bounds as `impl Cell<T: Eq>`). `impl Show<Foo> { … }` is an error: a trait
instance is always written `impl Show for Foo`.

```coil
class Celsius {
    pub deg: float,
}

trait Describe<T> {
    fn describe(T x) -> string;
}

impl Celsius {
    pub fn to_fahrenheit() -> float {
        return self.deg * 9.0 / 5.0 + 32.0;
    }
}

impl Describe for Celsius {
    fn describe(Celsius c) -> string {
        return "temperature";
    }
}
```

## pub

Makes a class field or method accessible from outside the type.

Without `pub`, fields and methods are private to the type's own `impl`
blocks. Top-level items need no `pub`.

```coil
class User {
    pub name: string,
    password: string,
}

impl User {
    pub fn greeting() -> string {
        return "hi " + self.name;
    }

    fn secret_len() -> int {
        return self.password.len();
    }
}
```

## new

Constructs a class instance: `new Class(args)`.

Arguments fill the fields in declaration order.
A class invariant is checked after construction. `readonly new Class(…)`
builds a sealed instance.

```coil
class Point {
    pub x: int,
    pub y: int,
}

fn main() {
    let p = new Point(3, 4);
    let sum = p.x + p.y;
}
```

## defer

Schedules a block to run when the enclosing function exits.

Deferred blocks run on every return, on falling off the end, and while a
panic or task cancellation unwinds, in reverse order of the `defer`
statements that were reached. The block sees outer locals only when they
are listed in `defer use (…)`.

```coil
fn work(Vec<string> log) -> int {
    log.push("start");
    defer use (log) {
        log.push("cleanup");
    }
    return log.len(); // 1: the defer runs after the value is computed
}

fn main() {
    let log: Vec<string> = Vec::new();
    let n = work(log); // log is now ["start", "cleanup"]
}
```

## true

The `bool` value true.

```coil
fn main() {
    let ready = true;
    if ready {
        let x = 1;
    }
}
```

## false

The `bool` value false.

```coil
fn main() {
    let done_yet = false;
    while !done_yet {
        done_yet = true;
    }
}
```

## gen

Declares a generator function (a coroutine): `gen fn name(…) { … }`.

Calling it allocates a suspended coroutine handle and runs nothing. Each
`resume` runs it to the next `yield` (or its `return`), and `for` can iterate
it. `gen` is only a keyword directly before `fn` (or `static fn`).

```coil
gen fn countdown(int n) {
    let i = n;
    while i > 0 {
        yield i;
        i = i - 1;
    }
}

fn main() {
    let total = 0;
    for v in countdown(3) {
        total = total + v;
    }
}
```

## async

Removed. Generator functions are spelled `gen fn`; `async fn` is a parse
error that names `gen fn`. Coil has no `async` / `await`; concurrency uses
the `task` module and threads.

## yield

Suspends a generator and hands a value to the code that resumed it.

`yield v` produces `v` as the result of the `resume`. Used as an expression
(`let msg = yield v;`) it also receives the value sent with
`resume h with x`. `yield from other()` delegates to another coroutine.

```coil
gen fn echo() {
    let msg = yield "ready";
    yield msg;
}

fn main() {
    let h = echo();
    let first = resume h; // "ready"
    let second = resume h with "hello"; // "hello"
}
```

## resume

Runs a coroutine until its next `yield` or its end, and gives the yielded or returned value.

`resume h with v` also sends `v`, which becomes the value of the `yield`
expression the coroutine is suspended at. Resuming a finished coroutine
panics; check with `done(h)`.

```coil
gen fn steps() {
    yield 10;
    yield 20;
    return 30;
}

fn main() {
    let h = steps();
    let a = resume h; // 10
    let b = resume h; // 20
    let c = resume h; // 30
}
```

## with

Sends a value into a coroutine as part of `resume`: `resume h with value`.

The value becomes the result of the `yield` the coroutine is suspended at.

```coil
gen fn adder() {
    let total = 0;
    while true {
        let n = yield total;
        total = total + n;
    }
}

fn main() {
    let h = adder();
    resume h;
    let now = resume h with 5; // 5
}
```

## where

Adds trait constraints to a generic function: `where Trait<A, B>`.

It comes after the return type and is needed for constraints with several
type parameters. A single-parameter bound is written on the parameter:
`fn f<T: Num>`, or a separate bounded impl block, `impl Cell<T: Num> { … }`,
for methods that need a bound on the impl's parameter. A unary
`where Num<T>` is an error.

```coil
fn convert<A, B>(A x) -> B where Into<A, B> {
    return into(x);
}

class Cell<T> {
    pub v: T,
}

impl Cell<T: Num> {
    pub fn doubled() -> T {
        return self.v + self.v;
    }
}
```

## pure

Declares that a function has no effects: `pure fn name(…)`.

The compiler checks the promise: a call that reads or writes files, uses the
network, the environment, threads or shared mutable state is error `E0413`.
Panicking and building new objects are allowed. Contextual: only a keyword
directly before `fn`.

```coil
pure fn area(int w, int h) -> int {
    return w * h;
}
```

## uses

Declares the effects a function may have: `uses {read, write}`.

The effects are `read`, `write`, `net`, `env`, `exec`, `ffi`, `thread`,
`suspend` and `mutate`. The compiler checks the list against what the body
does (error `E0413`); `uses {}` means the same as `pure`. Contextual: only a
keyword after the signature (and its `where` clause).

```coil
static let hits = 0;

fn bump() uses {read, mutate} {
    hits = hits + 1;
}
```

## requires

States a precondition, checked when the function is entered: `requires cond, "message"`.

A failed `requires` panics and blames the caller (the message names the call
site). Clauses come after the signature and before the body. Contextual:
only a keyword in that position.

```coil
fn isqrt(int n) -> int
    requires n >= 0, "negative input"
{
    let r = 0;
    while (r + 1) * (r + 1) <= n {
        r = r + 1;
    }
    return r;
}
```

## ensures

States a postcondition, checked on every return: `ensures cond, "message"`.

Inside the clause, `result` is the value being returned and `old(e)` is the
value `e` had on entry. Contextual: only a keyword after the signature.

```coil
fn abs(int n) -> int
    ensures result >= 0
{
    if n < 0 {
        return -n;
    }
    return n;
}
```

## invariant

States a condition that must always hold for a class's instances, or each time a loop tests its condition.

On a class (`class Name invariant cond { … }`) it is checked after `new` and
whenever a `pub` method returns. On a `while` or `for` loop it is checked
before the first iteration and after each one. Contextual: only a keyword in
those headers.

```coil
class Account
    invariant self.balance >= 0, "no overdraft"
{
    balance: int,
}

fn main() {
    let i = 0;
    while i < 3
        invariant i <= 3
    {
        i = i + 1;
    }
}
```

## decreases

States an `int` that every iteration of a `while` loop makes smaller, without going negative.

It proves the loop ends; a violation panics with `(went negative)` or
`(did not decrease)`. Contextual: only a keyword in a `while` header.

```coil
fn sum_to(int n) -> int {
    let i = 0;
    let s = 0;
    while i < n
        decreases n - i
    {
        s = s + i;
        i = i + 1;
    }
    return s;
}
```

## trait

Declares a trait: a set of functions that types implement with `impl Trait for Type`.

Its first type parameter is the implementing type. Methods may have default
bodies, and a trait can declare associated types (`type Elem;`) and
`static fn` constructors. Use it as a bound (`T: Area`) or as an
existential parameter type.

A trait method is called as a method (`x.area()`), as a function
(`area(x)`), or qualified by its trait (`Area::area(x)`). The qualified
form resolves like the function form but only considers that trait's
method, so it picks between two traits (or a trait and a free function)
that share a method name; it compiles to the same call.

```coil
trait Area<T> {
    fn area(T x) -> int;
}

class Square {
    pub side: int,
}

impl Area for Square {
    fn area(Square s) -> int {
        return s.side * s.side;
    }
}

fn total<T: Area>(T a, T b) -> int {
    return area(a) + Area::area(b);
}
```

## test

Declares a test case run by `coil test`: `test("name") { … }`.

The body runs in result mode, so `assert!(…)`, `raise` and `?` fail the case.
Each case runs in its own VM. Cases are left out of ordinary builds; a file
with test cases has no `fn main`.

```coil
use tests::assert;

test("addition works") {
    assert!(1 + 1 == 2);
}
```

## forall

Writes a polymorphic (higher-rank) function type: `forall T. T -> T`.

A parameter of that type accepts a generic function that the callee can use
at several types. Constraints go on the binder: `forall T: Num. T -> T`.

```coil
fn id<T>(T x) -> T {
    return x;
}

fn both(forall T. T -> T f) -> (int, string) {
    return (f(1), f("one"));
}

fn main() {
    let pair = both(id);
}
```

## from

Delegates a generator to another coroutine: `yield from other()`.

Each value the inner coroutine yields is yielded by the outer one, and sent
values pass through to it.

```coil
gen fn inner() {
    yield 1;
    yield 2;
}

gen fn outer() {
    yield 0;
    yield from inner();
}
```

## struct

Declares a C struct layout for FFI: `extern struct Name { field: type, … };`.

The struct can be passed to and returned from C functions by value; a
returned struct is read with `.field`. Field types are C scalar names such as
`int32`, `int` or `float`. There are no Coil structs: use `class`.

```coil
extern struct Point {
    x: int32,
    y: int32,
};
```

## attr

Declares an attribute macro: `attr name(FnDecl f, …) -> Code { … }`.

It runs at compile time on the item marked `#[name(…)]` (a function, method,
class or enum) and its output replaces that item. The first parameter is a
`FnDecl` or `TypeDecl`; the others bind the attribute's arguments. A module
cannot use its own macros: import them from a provider module.

```coil
use macro::{FnDecl, Code, raw};

/// Keeps the function and adds a `name_twice` wrapper.
attr twice(FnDecl f) -> Code {
    return quote items {
        ${raw(f.source)}
        fn ${raw(f.name.str())}_twice() -> int {
            return ${raw(f.name.str())}() * 2;
        }
    };
}
```

## derive

Declares a derive macro: `derive Name(TypeDecl t) -> Code { … }`, used as `#[derive(Name)]`.

It runs at compile time on the class or enum it is attached to, and its
output (usually an `impl`) is added after the type. `attrs(h, …)` lists the
field and variant attributes it owns. The built-in derives (`Show`, `Eq`,
`Ord`, `Hash`, `String`, `Default`, `Send`, `Sensitive`) need no import.
Contextual: elsewhere `derive` is an ordinary name.

```coil
use macro::{TypeDecl, Code, lit};

/// `T::type_name()` returns the type's name.
derive TypeName(TypeDecl t) -> Code {
    return quote items {
        impl ${t.name} {
            pub static fn type_name() -> string {
                return ${lit(t.name.str())};
            }
        }
    };
}
```

## macro

Declares a function-style macro: `macro name(Expr a, …) -> Code { … }`, used as `name!(…)`.

It runs at compile time with the call's arguments as unevaluated `Expr`
values and returns code that replaces the call: an expression, statements,
or declarations depending on where the call is. A last `Vec<Expr>` parameter
takes the remaining arguments. Contextual: `use macro::{…}` names the macro
model module.

```coil
use macro::{Expr, Code};

/// `square!(e)` expands to `e * e`.
macro square(Expr e) -> Code {
    return quote expr { ${e} * ${e} };
}
```

## quote

Builds generated code inside a macro: `quote items|expr|stmts|type { template }`.

The template is Coil source with holes: `${e}` splices an expression, name,
type or `Code`, and `$(xs),*` splices a `Vec<Code>` with a separator. The
result is a `Code` value. Names bound inside the template are renamed so
they cannot clash with the caller's. Contextual: `quote(x)` stays a call.

```coil
use macro::{Expr, Code};

macro twice(Expr e) -> Code {
    return quote stmts {
        ${e};
        ${e};
    };
}
```

## attrs

Lists the field and variant attributes a derive macro owns: `derive Name(TypeDecl t) -> Code attrs(names) { … }`.

An attribute such as `#[names(rename = "…")]` on a field is accepted only
when a derive on the type owns it. Contextual: only a keyword in a `derive`
header.

```coil
use macro::{TypeDecl, Code, lit};

derive FieldNames(TypeDecl t) -> Code attrs(names) {
    let names: Vec<Code> = Vec::new();
    for f in t.fields() {
        names.push(lit(f.attr_str("names", "rename", f.name.str())));
    }
    return quote items {
        impl ${t.name} {
            pub static fn field_names() -> Vec<string> {
                let out: Vec<string> = Vec::from([$(names),*]);
                return out;
            }
        }
    };
}
```

## done

Tells whether a coroutine has finished: `done(handle)` returns a `bool`.

It is `false` before the first `resume` and while the coroutine is suspended
at a `yield`, and `true` once its body has returned.

```coil
gen fn two() {
    yield 1;
    yield 2;
}

fn main() {
    let h = two();
    while !done(h) {
        resume h;
    }
}
```

## self

The receiver of a method: the instance the method was called on.

Instance methods take `self` implicitly (it is not listed in the
parameters). Read and write fields as `self.field` and call other methods as
`self.method()`. `static fn` methods have no `self`. Class invariants and
`ensures` clauses can mention `self`.

```coil
class Counter {
    n: int,
}

impl Counter {
    pub fn bump() -> int {
        self.n = self.n + 1;
        return self.n;
    }
}
```

## result

The value being returned, inside an `ensures` clause.

For a function returning `Result<T, E>`, `result` is the whole `Result`. A
function with an `ensures` cannot have a parameter named `result`.
Contextual: elsewhere `result` is an ordinary name.

```coil
fn double(int n) -> int
    ensures result == n * 2
{
    return n + n;
}
```

## old

The value an expression had when the function was entered, inside an `ensures` clause: `old(e)`.

Each `old(e)` is evaluated once, right after the `requires` checks.
Contextual: only special inside `ensures`.

```coil
class Account {
    pub balance: int,
}

impl Account {
    pub fn deposit(int amount)
        requires amount > 0
        ensures self.balance == old(self.balance) + amount
    {
        self.balance = self.balance + amount;
    }
}
```

## Constraint

The kind of a constraint, in a kind annotation: `c: * -> Constraint`.

It lets a type parameter stand for a trait, so a function can be generic over
which trait bounds another parameter.

```coil
fn show_it<c: * -> Constraint, T: c>(T x) -> string {
    return show(x);
}
```

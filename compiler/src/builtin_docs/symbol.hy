// Doc stubs for the builtin `symbol` module (hover docs; never compiled).

/// Returns the key of the function this call is written in.
///
/// The key is an `int` the compiler computes from the function's module
/// path, name and arity, and writes in place of the call: every call in
/// the same function returns the same key, whatever its arguments or its
/// caller, and every other function, method, overload, lambda and test
/// has its own. Inlining does not change it, and a generic function (or a
/// generic class's method) has a key per instantiation: `ident(1)` and
/// `ident(2)` share one, `ident("a")` has another. The key is not negative, and it
/// stays the same across builds as long as the function keeps its module,
/// name and arity (a lambda's key also depends on where it sits in the
/// file).
///
/// Use it to key a per-function cache, such as a memo table, without
/// spelling the function's name. Call it directly inside a function,
/// method, lambda or test; anywhere else (a `const` or `static`
/// initializer, top-level code) is error `E0144`, and it cannot be taken as
/// a function value. A generic function that calls it cannot be used as a
/// function value either (`E0145`): wrap the call at concrete types.
///
/// # Examples
///
/// ```
/// use symbol::{fn_key};
/// # use tests::assert;
/// fn which(int x) -> int {
///     return fn_key();
/// }
///
/// fn other() -> int {
///     return fn_key();
/// }
///
/// fn main() {
///     assert!(which(1) == which(2));
///     assert!(which(1) != other());
/// }
/// ```
fn fn_key() -> int {}

/// Returns the next value of a process-wide counter.
///
/// The first call in the process returns `1`, and each call after that one
/// more than the last, whichever function, thread or task makes it: no two
/// calls ever return the same id. Calls made at the same time on different
/// threads are ordered by the counter itself, so one thread's ids are
/// increasing but need not be consecutive while other threads take ids too.
/// A function that calls `next_id` counts as reading host state, so it is
/// never computed at compile time, hoisted out of a loop or run in
/// parallel automatically.
///
/// # Examples
///
/// ```
/// use symbol::{next_id};
/// # use tests::assert;
/// let a = next_id();
/// let b = next_id();
/// assert!(a >= 1);
/// assert!(b > a);
/// ```
fn next_id() -> int {}

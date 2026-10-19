// Doc stubs for the builtin `prelude` module (hover docs; never compiled).

/// An optional value: either `Some(value)` or `None`.
///
/// `Option` is auto-imported into every file. Construct it with
/// `Option::Some(x)` and `Option::None`; bare `Some` / `None` also work while
/// no other enum in scope has a case with the same name.
///
/// Take it apart with `match`, the coalesce operator `??` (use a default when
/// the value is `None`), the try operator `?` (return `None` early from a
/// function that itself returns an `Option`) or optional access `a?.field`.
/// `Vec::pop` and `Vec::remove` return `Option`.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// fn find(Vec<int> xs, int wanted) -> Option<int> {
///     let i = 0;
///     while i < xs.len() {
///         if xs[i] == wanted {
///             return Option::Some(i);
///         }
///         i = i + 1;
///     }
///     return Option::None;
/// }
///
/// fn main() {
///     let xs: Vec<int> = [4, 8, 15];
///     let at = match find(xs, 8) {
///         Option::Some(i) => i,
///         Option::None => -1,
///     };
///     assert!(at == 1);
///     assert!((find(xs, 99) ?? -1) == -1);
/// }
/// ```
enum Option<T> {
    /// No value.
    None,
    /// Some value of type `T`.
    Some(T),
}

/// The outcome of an operation that can fail: `Ok(value)` or `Err(error)`.
///
/// `Result` is auto-imported into every file. Construct it with
/// `Result::Ok(x)` and `Result::Err(e)`; bare `Ok` / `Err` also work while no
/// other enum in scope has a case with the same name.
///
/// A function annotated `-> Result<T, E>`, or one that uses `raise` or `?` on
/// a `Result`, runs in result mode: a plain `return v;` is wrapped as `Ok(v)`
/// and `raise e;` returns `Err(e)`. `x?` unwraps `Ok` or returns the `Err`
/// early. `x ?? default` unwraps `Ok` or falls back to `default`, silently
/// discarding the error, so prefer `match` or `?` when the failure matters.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// fn half(int n) -> Result<int, string> {
///     if n % 2 != 0 {
///         raise "odd";
///     }
///     return n / 2;
/// }
///
/// fn main() {
///     let ok = match half(10) {
///         Result::Ok(v) => v,
///         Result::Err(_) => 0,
///     };
///     assert!(ok == 5);
///     let msg = match half(3) {
///         Result::Ok(_) => "",
///         Result::Err(e) => e,
///     };
///     assert!(msg == "odd");
/// }
/// ```
enum Result<T, E> {
    /// The operation succeeded with a value of type `T`.
    Ok(T),
    /// The operation failed with an error of type `E`.
    Err(E),
}

/// A growable, heap-allocated vector of `T` values.
///
/// `Vec` is auto-imported. Create one with `Vec::new()`,
/// `Vec::with_capacity(n)`, `Vec::from(array)`, or an array literal where a
/// `Vec<T>` is expected (`let v: Vec<int> = [1, 2];`). Read and write
/// elements with `v[i]` and `v[i] = x`; an index that is negative or not less
/// than `v.len()` panics with `index out of bounds`.
///
/// `for x in v` iterates the elements in order. Rest parameters (`T... xs`)
/// arrive as a `Vec<T>`, and IO buffers are `Vec<byte>`. A `Vec` supports
/// unary `-` and scalar broadcast (`k * v`), but two `Vec` values cannot be
/// zipped with an operator; use fixed `[T; N]` arrays for that.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// let v: Vec<int> = Vec::new();
/// v.push(1);
/// v.push(2);
/// v[0] = 7;
/// let total = 0;
/// for x in v {
///     total = total + x;
/// }
/// assert!(total == 9);
/// assert!(v.len() == 2);
/// ```
class Vec<T> {}

impl Vec<T> {
    /// Creates a new, empty vector.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tests::assert;
    /// let v: Vec<string> = Vec::new();
    /// assert!(v.len() == 0);
    /// ```
    pub static fn new() -> Vec<T> {}

    /// Creates an empty vector with room for at least `n` elements.
    ///
    /// Pushing up to `n` elements does not reallocate. The length is still `0`.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tests::assert;
    /// let v: Vec<int> = Vec::with_capacity(16);
    /// assert!(v.len() == 0);
    /// assert!(v.capacity() >= 16);
    /// ```
    pub static fn with_capacity(int n) -> Vec<T> {}

    /// Creates a vector holding a copy of the elements of the fixed array `arr`.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tests::assert;
    /// let v = Vec::from([1, 2, 3]);
    /// v.push(4);
    /// assert!(v.len() == 4);
    /// assert!(v[3] == 4);
    /// ```
    pub static fn from([T; N] arr) -> Vec<T> {}

    /// Appends `x` to the end of the vector.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tests::assert;
    /// let v: Vec<int> = Vec::new();
    /// v.push(5);
    /// assert!(v[0] == 5);
    /// ```
    pub fn push(T x) {}

    /// Removes the last element and returns it, or `None` when the vector is empty.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tests::assert;
    /// let v: Vec<int> = [1, 2];
    /// assert!((v.pop() ?? 0) == 2);
    /// assert!((v.pop() ?? 0) == 1);
    /// assert!((v.pop() ?? 0) == 0);
    /// ```
    pub fn pop() -> Option<T> {}

    /// Inserts `x` at index `i`, shifting later elements one place to the right.
    ///
    /// `i` may equal `len()` (append).
    ///
    /// # Panics
    ///
    /// Panics when `i` is negative or greater than `len()`.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tests::assert;
    /// let v: Vec<int> = [1, 3];
    /// v.insert(1, 2);
    /// v.insert(3, 4);
    /// assert!(v[1] == 2);
    /// assert!(v[3] == 4);
    /// ```
    pub fn insert(int i, T x) {}

    /// Removes the element at index `i` and returns it, shifting later elements left.
    ///
    /// Returns `None` (and leaves the vector unchanged) when `i` is out of range.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tests::assert;
    /// let v: Vec<int> = [10, 20, 30];
    /// assert!((v.remove(0) ?? 0) == 10);
    /// assert!(v[0] == 20);
    /// assert!((v.remove(9) ?? -1) == -1);
    /// ```
    pub fn remove(int i) -> Option<T> {}

    /// Removes every element. The capacity is kept.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tests::assert;
    /// let v: Vec<int> = [1, 2, 3];
    /// v.clear();
    /// assert!(v.len() == 0);
    /// ```
    pub fn clear() {}

    /// Ensures room for at least `n` more elements beyond the current length.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tests::assert;
    /// let v: Vec<int> = [1, 2];
    /// v.reserve(10);
    /// assert!(v.capacity() >= 12);
    /// ```
    pub fn reserve(int n) {}

    /// Returns how many elements the vector can hold before it must reallocate.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tests::assert;
    /// let v: Vec<int> = Vec::with_capacity(8);
    /// assert!(v.capacity() >= 8);
    /// ```
    pub fn capacity() -> int {}

    /// Returns the number of elements in the vector.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tests::assert;
    /// let v: Vec<int> = [1, 2, 3];
    /// assert!(v.len() == 3);
    /// ```
    pub fn len() -> int {}
}

/// A lazy half-open range `start..end`: every value from `start` up to, but not including, `end`.
///
/// Written with the `..` operator; the bounds must have the same type with an
/// `Ord` instance. `for x in a..b` and `.to_vec()` step by `1` (or `1.0`) and
/// work for `int`, `byte` and `float` bounds only. A range whose end is not
/// greater than its start is empty.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// let sum = 0;
/// for i in 0..4 {
///     sum = sum + i;
/// }
/// assert!(sum == 6);
/// ```
class Range<T> {}

impl Range<T> {
    /// Collects the range into a new `Vec<T>`.
    ///
    /// Only numeric ranges (`int`, `byte`, `float`) can be collected; other
    /// element types are a compile-time error.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tests::assert;
    /// let v = (2..5).to_vec();
    /// assert!(v.len() == 3);
    /// assert!(v[0] == 2);
    /// assert!((5..2).to_vec().len() == 0);
    /// ```
    pub fn to_vec() -> Vec<T> {}
}

/// A lazy inclusive range `start..=end`: every value from `start` up to and including `end`.
///
/// Written with the `..=` operator. Iteration and `.to_vec()` follow the same
/// rules as `Range`: step `1` (or `1.0`), numeric bounds only, and empty when
/// `end` is less than `start`.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// let sum = 0;
/// for i in 1..=4 {
///     sum = sum + i;
/// }
/// assert!(sum == 10);
/// ```
class RangeInclusive<T> {}

impl RangeInclusive<T> {
    /// Collects the range into a new `Vec<T>`, including the end value.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tests::assert;
    /// let v = (1..=3).to_vec();
    /// assert!(v.len() == 3);
    /// assert!(v[2] == 3);
    /// ```
    pub fn to_vec() -> Vec<T> {}
}

/// A source of values that `for` loops pull from one at a time.
///
/// Implementations define the associated type `Item` (the element type) and
/// `next`, which returns `Some(item)` for each value and `None` when the sequence is
/// exhausted. `for x in e` first converts `e` with `IntoIterator`, then calls
/// `next` until it returns `None`. Arrays, `Vec`, homogeneous tuples and
/// records, coroutines and numeric ranges are iterable without an `impl`.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// class Countdown {
///     pub n: int,
/// }
///
/// impl Iterator for Countdown {
///     type Item = int;
///     pub fn next(Countdown c) -> Option<int> {
///         if c.n == 0 {
///             return Option::None;
///         }
///         c.n = c.n - 1;
///         return Option::Some(c.n + 1);
///     }
/// }
///
/// impl IntoIterator for Countdown {
///     type Item = int;
///     type IntoIter = Countdown;
///     pub fn into_iter(Countdown c) -> Countdown {
///         return c;
///     }
/// }
///
/// fn main() {
///     let seen = 0;
///     for x in new Countdown(3) {
///         seen = seen * 10 + x;
///     }
///     assert!(seen == 321);
/// }
/// ```
trait Iterator<I> {
    type Item;

    /// Returns the next value, or `None` when the iterator is exhausted.
    fn next(I it) -> Option<Item> {}
}

/// A type that can be turned into an `Iterator`, which makes it usable in `for x in value`.
///
/// `into_iter` returns the iterator (`IntoIter`), whose `Item` must match.
/// A class that is its own iterator returns itself. See `Iterator` for a full
/// example.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// class Ones {
///     pub left: int,
/// }
///
/// impl Iterator for Ones {
///     type Item = int;
///     pub fn next(Ones o) -> Option<int> {
///         if o.left == 0 {
///             return Option::None;
///         }
///         o.left = o.left - 1;
///         return Option::Some(1);
///     }
/// }
///
/// impl IntoIterator for Ones {
///     type Item = int;
///     type IntoIter = Ones;
///     pub fn into_iter(Ones o) -> Ones {
///         return o;
///     }
/// }
///
/// fn main() {
///     let n = 0;
///     for x in new Ones(4) {
///         n = n + x;
///     }
///     assert!(n == 4);
/// }
/// ```
trait IntoIterator<T> {
    type Item;
    type IntoIter;

    /// Converts `value` into its iterator.
    fn into_iter(T value) -> IntoIter {}
}

/// Returns the code of the single character in `s` as a `byte`.
///
/// `s` must hold exactly one character whose Unicode code point is at most
/// 255 (ASCII or Latin-1, so `ord("é")` is `233`). Otherwise the result is
/// `Err` with a message: `"empty string"`, `"expected single character"` or
/// `"character code out of byte range"`. The inverse is `char`.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// let code = match ord("A") {
///     Result::Ok(b) => b as int,
///     Result::Err(_) => -1,
/// };
/// assert!(code == 65);
/// let err = match ord("AB") {
///     Result::Ok(_) => "",
///     Result::Err(e) => e,
/// };
/// assert!(err == "expected single character");
/// ```
fn ord(string s) -> Result<byte, string> {}

/// Returns the one-character string whose code point is `b`.
///
/// Values `0..=127` give an ASCII character; `128..=255` give the Latin-1
/// character with that code point (two bytes of UTF-8, so `char(233)` is
/// `"é"`). The result is `Err("byte out of range")` only if the value is
/// outside `0..=255`, which a well-typed `byte` never is. The inverse is `ord`.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// let s = match char(66 as byte) {
///     Result::Ok(c) => c,
///     Result::Err(_) => "",
/// };
/// assert!(s == "B");
/// ```
fn char(byte b) -> Result<string, string> {}

/// Resumes the coroutine `handle` until it completes and returns its completion value.
///
/// Values produced by `yield` along the way are discarded; the result is the
/// value of the generator's `return`. The coroutine must not expect values
/// sent in with `resume ... with`.
///
/// Deprecated: every call warns `E0129`. Call the work directly, or run
/// concurrent work as a task (`task::scope` and `join`).
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// gen fn steps() -> int {
///     yield 1;
///     yield 2;
///     return 3;
/// }
///
/// fn main() {
///     assert!(block_on(steps()) == 3);
/// }
/// ```
fn block_on<Y>(coroutine<Y> handle) -> Y {}

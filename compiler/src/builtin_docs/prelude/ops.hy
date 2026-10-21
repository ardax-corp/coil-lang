// Doc stubs for the builtin `prelude::ops` module (hover docs; never compiled).

/// The addition operator `+`.
///
/// `a + b` on a type with an `Add` instance calls `add(a, b)`. `int` and
/// `float` have builtin instances (compiled to machine adds, not calls).
/// String concatenation with `+` is a separate rule and does not go through
/// `Add`. In a generic function, `T: Add` (or `T: Num`) allows `+` on `T`.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// class V2 {
///     pub x: int,
///     pub y: int,
/// }
///
/// impl Add for V2 {
///     pub fn add(V2 a, V2 b) -> V2 {
///         return new V2(a.x + b.x, a.y + b.y);
///     }
/// }
///
/// fn main() {
///     let p = new V2(1, 2) + new V2(10, 20);
///     assert!(p.x == 11);
///     assert!(p.y == 22);
/// }
/// ```
trait Add<T> {
    /// Returns `a + b`.
    fn add(T a, T b) -> T {}
}

/// The subtraction operator `-`.
///
/// `a - b` on a type with a `Sub` instance calls `sub(a, b)`. `int` and
/// `float` have builtin instances.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// class V2 {
///     pub x: int,
///     pub y: int,
/// }
///
/// impl Sub for V2 {
///     pub fn sub(V2 a, V2 b) -> V2 {
///         return new V2(a.x - b.x, a.y - b.y);
///     }
/// }
///
/// fn main() {
///     let d = new V2(5, 5) - new V2(1, 2);
///     assert!(d.x == 4);
///     assert!(d.y == 3);
/// }
/// ```
trait Sub<T> {
    /// Returns `a - b`.
    fn sub(T a, T b) -> T {}
}

/// The multiplication operator `*`.
///
/// `a * b` on a type with a `Mul` instance calls `mul(a, b)`. `int` and
/// `float` have builtin instances. On `Matrix` values `*` is matrix
/// multiplication.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// class V2 {
///     pub x: int,
///     pub y: int,
/// }
///
/// impl Mul for V2 {
///     pub fn mul(V2 a, V2 b) -> V2 {
///         return new V2(a.x * b.x, a.y * b.y);
///     }
/// }
///
/// fn main() {
///     let p = new V2(2, 3) * new V2(4, 5);
///     assert!(p.x == 8);
///     assert!(p.y == 15);
/// }
/// ```
trait Mul<T> {
    /// Returns `a * b`.
    fn mul(T a, T b) -> T {}
}

/// The division operator `/`.
///
/// `a / b` on a type with a `Div` instance calls `div(a, b)`. `int` and
/// `float` have builtin instances: `int` division truncates toward zero and
/// panics with `division by zero` when `b` is `0`; `float` division follows
/// IEEE-754 (`1.0 / 0.0` is infinity).
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// class V2 {
///     pub x: int,
///     pub y: int,
/// }
///
/// impl Div for V2 {
///     pub fn div(V2 a, V2 b) -> V2 {
///         return new V2(a.x / b.x, a.y / b.y);
///     }
/// }
///
/// fn main() {
///     let q = new V2(9, 8) / new V2(3, 2);
///     assert!(q.x == 3);
///     assert!(q.y == 4);
///     assert!(-7 / 2 == -3);
/// }
/// ```
trait Div<T> {
    /// Returns `a / b`.
    fn div(T a, T b) -> T {}
}

/// The unary negation operator `-x`.
///
/// `-x` on a type with a `Neg` instance calls `neg(x)`. `int` and `float`
/// have builtin instances; negating the smallest `int` panics with
/// `integer overflow`.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// class V2 {
///     pub x: int,
///     pub y: int,
/// }
///
/// impl Neg for V2 {
///     pub fn neg(V2 a) -> V2 {
///         return new V2(-a.x, -a.y);
///     }
/// }
///
/// fn main() {
///     let n = -new V2(1, -2);
///     assert!(n.x == -1);
///     assert!(n.y == 2);
/// }
/// ```
trait Neg<T> {
    /// Returns `-a`.
    fn neg(T a) -> T {}
}

/// The remainder operator `%`.
///
/// `a % b` on a type with a `Rem` instance calls `rem(a, b)`. `int` and
/// `float` have builtin instances. The `int` result has the sign of `a`
/// (`-7 % 2` is `-1`) and `% 0` panics with `division by zero`.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// class Hours {
///     pub h: int,
/// }
///
/// impl Rem for Hours {
///     pub fn rem(Hours a, Hours b) -> Hours {
///         return new Hours(a.h % b.h);
///     }
/// }
///
/// fn main() {
///     let t = new Hours(27) % new Hours(24);
///     assert!(t.h == 3);
///     assert!(-7 % 2 == -1);
/// }
/// ```
trait Rem<T> {
    /// Returns the remainder of `a / b`.
    fn rem(T a, T b) -> T {}
}

/// The exponentiation operator `**`.
///
/// `a ** b` on a type with a `Pow` instance calls `pow(a, b)`. `int` and
/// `float` have builtin instances. `**` is right-associative
/// (`2 ** 3 ** 2` is `512`). For `int` a negative exponent panics with
/// `negative exponent` and a result that does not fit panics with
/// `integer overflow`.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// class Gain {
///     pub g: int,
/// }
///
/// impl Pow for Gain {
///     pub fn pow(Gain a, Gain b) -> Gain {
///         return new Gain(a.g ** b.g);
///     }
/// }
///
/// fn main() {
///     let p = new Gain(3) ** new Gain(2);
///     assert!(p.g == 9);
///     assert!(2 ** 10 == 1024);
/// }
/// ```
trait Pow<T> {
    /// Returns `a` raised to the power `b`.
    fn pow(T a, T b) -> T {}
}

/// The numeric bundle: `Add`, `Sub`, `Mul`, `Div`, `Neg`, `Rem` and `Pow`.
///
/// `Num` has no methods of its own. A bound `T: Num` lets a generic function
/// use every arithmetic operator on `T`. `int` and `float` are `Num`
/// (`byte` is not). A hand-written `impl Num for X` needs an instance of each
/// of the seven traits first.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// fn square_plus<T: Num>(T a, T b) -> T {
///     return a * a + b;
/// }
///
/// fn main() {
///     assert!(square_plus(3, 1) == 10);
///     assert!(square_plus(1.5, 0.75) == 3.0);
/// }
/// ```
trait Num<T: Add + Sub + Mul + Div + Neg + Rem + Pow> {
}

/// The left shift operator `<<`.
///
/// `a << n` on a type with a `Shl` instance calls `shl(a, n)`. The shift
/// count is always an `int`, whatever the type of `a`, and the result has
/// the type of `a`. `int` and `byte` have builtin instances; shifting a
/// `byte` keeps the low 8 bits.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// class Flags {
///     pub bits: int,
/// }
///
/// impl Shl for Flags {
///     pub fn shl(Flags a, int n) -> Flags {
///         return new Flags(a.bits << n);
///     }
/// }
///
/// fn main() {
///     let f = new Flags(1) << 3;
///     assert!(f.bits == 8);
/// }
/// ```
trait Shl<T> {
    /// Returns `a` shifted left by `n` bits.
    fn shl(T a, int n) -> T {}
}

/// The right shift operator `>>`.
///
/// `a >> n` on a type with a `Shr` instance calls `shr(a, n)`. The shift
/// count is always an `int`. `int` and `byte` have builtin instances; the
/// `int` shift is arithmetic (it keeps the sign, so `-8 >> 1` is `-4`).
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// class Flags {
///     pub bits: int,
/// }
///
/// impl Shr for Flags {
///     pub fn shr(Flags a, int n) -> Flags {
///         return new Flags(a.bits >> n);
///     }
/// }
///
/// fn main() {
///     let f = new Flags(32) >> 2;
///     assert!(f.bits == 8);
///     assert!(-8 >> 1 == -4);
/// }
/// ```
trait Shr<T> {
    /// Returns `a` shifted right by `n` bits.
    fn shr(T a, int n) -> T {}
}

/// The bitwise AND operator `&`.
///
/// `a & b` on a type with a `BitAnd` instance calls `bitand(a, b)`. `int`
/// and `byte` have builtin instances. For booleans use `&&`.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// class Flags {
///     pub bits: int,
/// }
///
/// impl BitAnd for Flags {
///     pub fn bitand(Flags a, Flags b) -> Flags {
///         return new Flags(a.bits & b.bits);
///     }
/// }
///
/// fn main() {
///     let f = new Flags(12) & new Flags(10);
///     assert!(f.bits == 8);
/// }
/// ```
trait BitAnd<T> {
    /// Returns the bitwise AND of `a` and `b`.
    fn bitand(T a, T b) -> T {}
}

/// The bitwise OR operator `|`.
///
/// `a | b` on a type with a `BitOr` instance calls `bitor(a, b)`. `int` and
/// `byte` have builtin instances. For booleans use `||`.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// class Flags {
///     pub bits: int,
/// }
///
/// impl BitOr for Flags {
///     pub fn bitor(Flags a, Flags b) -> Flags {
///         return new Flags(a.bits | b.bits);
///     }
/// }
///
/// fn main() {
///     let f = new Flags(1) | new Flags(4);
///     assert!(f.bits == 5);
/// }
/// ```
trait BitOr<T> {
    /// Returns the bitwise OR of `a` and `b`.
    fn bitor(T a, T b) -> T {}
}

/// The bitwise XOR operator `^`.
///
/// `a ^ b` on a type with a `BitXor` instance calls `bitxor(a, b)`. `int`
/// and `byte` have builtin instances.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// class Flags {
///     pub bits: int,
/// }
///
/// impl BitXor for Flags {
///     pub fn bitxor(Flags a, Flags b) -> Flags {
///         return new Flags(a.bits ^ b.bits);
///     }
/// }
///
/// fn main() {
///     let f = new Flags(6) ^ new Flags(3);
///     assert!(f.bits == 5);
/// }
/// ```
trait BitXor<T> {
    /// Returns the bitwise XOR of `a` and `b`.
    fn bitxor(T a, T b) -> T {}
}

/// The bitwise NOT operator `~x`.
///
/// `~x` on a type with a `BitNot` instance calls `bitnot(x)`. Only `int` has
/// a builtin instance (`~x` is `-x - 1`); `byte` does not.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// class Flags {
///     pub bits: int,
/// }
///
/// impl BitNot for Flags {
///     pub fn bitnot(Flags a) -> Flags {
///         return new Flags(~a.bits);
///     }
/// }
///
/// fn main() {
///     let f = ~new Flags(0);
///     assert!(f.bits == -1);
/// }
/// ```
trait BitNot<T> {
    /// Returns `a` with every bit flipped.
    fn bitnot(T a) -> T {}
}

/// The integer bundle: `Num` plus `Shl`, `Shr`, `BitAnd`, `BitOr`, `BitXor` and `BitNot`.
///
/// `Integral` has no methods of its own. A bound `T: Integral` allows the
/// arithmetic, shift and bitwise operators on `T`. Only `int` is `Integral`
/// (`float` is `Num` but not `Integral`).
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// fn low_bits<T: Integral>(T x, T mask) -> T {
///     return x & mask;
/// }
///
/// fn main() {
///     assert!(low_bits(13, 3) == 1);
/// }
/// ```
trait Integral<T: Num + Shl + Shr + BitAnd + BitOr + BitXor + BitNot> {
}

/// The equality operators `==` and `!=`.
///
/// `a == b` calls `eq(a, b)` and `a != b` calls `ne(a, b)` on a type with an
/// `Eq` instance. The primitives have builtin instances (strings compare by
/// content). Arrays, `Vec`s, tuples, records and enums compare structurally,
/// and an element with an `Eq` instance (a class's too) compares with it.
/// Classes without an instance compare by identity. `#[derive(Eq)]`
/// generates a field-by-field instance.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// class Money {
///     pub cents: int,
/// }
///
/// impl Eq for Money {
///     pub fn eq(Money a, Money b) -> bool {
///         return a.cents == b.cents;
///     }
///     pub fn ne(Money a, Money b) -> bool {
///         return a.cents != b.cents;
///     }
/// }
///
/// fn main() {
///     assert!(new Money(100) == new Money(100));
///     assert!(new Money(1) != new Money(2));
/// }
/// ```
trait Eq<T> {
    /// Returns `true` when `a` and `b` are equal.
    fn eq(T a, T b) -> bool {}
    /// Returns `true` when `a` and `b` are not equal.
    fn ne(T a, T b) -> bool {}
}

/// The ordering bundle: `Lt`, `Le`, `Gt` and `Ge`.
///
/// `Ord` has no methods of its own. A bound `T: Ord` allows `<`, `<=`, `>`
/// and `>=` on `T`, and range bounds (`a..b`) must be `Ord`. `int`, `float`,
/// `byte` and `string` are ordered (a `string` by its UTF-8 bytes, so
/// `"B" < "a"` and a prefix sorts first); `bool` is not. A `string` is not
/// a range bound. `#[derive(Ord)]` orders an enum or class by its fields in
/// declaration order, and a scalar-backed enum (`#[repr(int)]`,
/// `#[repr(string)]`, ..) by its backing value.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// fn smaller<T: Ord>(T a, T b) -> T {
///     if a < b {
///         return a;
///     }
///     return b;
/// }
///
/// fn main() {
///     assert!(smaller(3, 2) == 2);
///     assert!(smaller(1.5, 2.5) == 1.5);
///     assert!(smaller("pear", "fig") == "fig");
/// }
/// ```
trait Ord<T: Lt + Le + Gt + Ge> {
}

/// The less-than operator `<`.
///
/// `a < b` on a type with an `Lt` instance calls `lt(a, b)`.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// class Version {
///     pub n: int,
/// }
///
/// impl Lt for Version {
///     pub fn lt(Version a, Version b) -> bool {
///         return a.n < b.n;
///     }
/// }
///
/// fn main() {
///     assert!(new Version(1) < new Version(2));
/// }
/// ```
trait Lt<T> {
    /// Returns `true` when `a` is less than `b`.
    fn lt(T a, T b) -> bool {}
}

/// The less-or-equal operator `<=`.
///
/// `a <= b` on a type with an `Le` instance calls `le(a, b)`.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// class Version {
///     pub n: int,
/// }
///
/// impl Le for Version {
///     pub fn le(Version a, Version b) -> bool {
///         return a.n <= b.n;
///     }
/// }
///
/// fn main() {
///     assert!(new Version(2) <= new Version(2));
/// }
/// ```
trait Le<T> {
    /// Returns `true` when `a` is less than or equal to `b`.
    fn le(T a, T b) -> bool {}
}

/// The greater-than operator `>`.
///
/// `a > b` on a type with a `Gt` instance calls `gt(a, b)`.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// class Version {
///     pub n: int,
/// }
///
/// impl Gt for Version {
///     pub fn gt(Version a, Version b) -> bool {
///         return a.n > b.n;
///     }
/// }
///
/// fn main() {
///     assert!(new Version(3) > new Version(2));
/// }
/// ```
trait Gt<T> {
    /// Returns `true` when `a` is greater than `b`.
    fn gt(T a, T b) -> bool {}
}

/// The greater-or-equal operator `>=`.
///
/// `a >= b` on a type with a `Ge` instance calls `ge(a, b)`.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// class Version {
///     pub n: int,
/// }
///
/// impl Ge for Version {
///     pub fn ge(Version a, Version b) -> bool {
///         return a.n >= b.n;
///     }
/// }
///
/// fn main() {
///     assert!(new Version(2) >= new Version(1));
/// }
/// ```
trait Ge<T> {
    /// Returns `true` when `a` is greater than or equal to `b`.
    fn ge(T a, T b) -> bool {}
}

/// Conversion to a display string, used by the `%v` format specifier.
///
/// `int`, `float`, `byte`, `string`, `bool` and `unit` have builtin
/// instances; tuples and records are shown structurally. A non-generic class
/// or enum without an instance shows its type name. `#[derive(Show)]`
/// generates a field-by-field instance.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// use string::{format};
///
/// class Point {
///     pub x: int,
///     pub y: int,
/// }
///
/// impl Show for Point {
///     pub fn show(Point p) -> string {
///         return format("(%i, %i)", p.x, p.y);
///     }
/// }
///
/// fn main() {
///     let p = new Point(1, 2);
///     assert!(show(p) == "(1, 2)");
///     assert!(format("at %v", p) == "at (1, 2)");
///     assert!(show(2.5) == "2.5");
/// }
/// ```
trait Show<T> {
    /// Returns the display form of `value`.
    fn show(T value) -> string {}
}

/// A size query: `x.len()`.
///
/// Arrays, `Vec`, strings (UTF-8 bytes), tuples and records (fields) have a
/// structural length and need no instance; fixed sizes fold at compile time.
/// They meet a `T: Length` bound too, `len` reading that same length.
/// Implement `Length` to give your own type a length.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// class Team {
///     pub names: Vec<string>,
/// }
///
/// impl Length for Team {
///     pub fn len(Team t) -> int {
///         return t.names.len();
///     }
/// }
///
/// fn main() {
///     let t = new Team(Vec::from(["ana", "bo"]));
///     assert!(t.len() == 2);
/// }
/// ```
trait Length<T> {
    /// Returns the length of `value`.
    fn len(T value) -> int {}
}

/// Conversion from `S` into `T`, written `impl Into<T> for S`.
///
/// Call it as a method where the target type is known:
/// `let y: T = x.into();`. There are no builtin instances; use `as` for
/// primitive conversions. Both `S` and `T` must be types defined in the
/// current module (an instance with a builtin type such as `int` on either
/// side is rejected as an orphan).
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// class Celsius {
///     pub c: int,
/// }
///
/// class Fahrenheit {
///     pub f: int,
/// }
///
/// impl Into<Fahrenheit> for Celsius {
///     pub fn into(Celsius x) -> Fahrenheit {
///         return new Fahrenheit(x.c * 9 / 5 + 32);
///     }
/// }
///
/// fn main() {
///     let f: Fahrenheit = new Celsius(100).into();
///     assert!(f.f == 212);
/// }
/// ```
trait Into<S, T> {
    /// Converts `value` into a `T`.
    fn into(S value) -> T {}
}

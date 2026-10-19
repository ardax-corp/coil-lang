// Doc stubs for the builtin primitive types (hover docs; never compiled).

/// A 64-bit signed integer, from `-9223372036854775808` to `9223372036854775807`.
///
/// Literals are decimal (`42`, `-1`); there are no hex, binary or
/// digit-separator forms. The smallest value cannot be written as a literal;
/// use `-9223372036854775807 - 1`.
///
/// Arithmetic is exact or it panics, in every build: `+`, `-`, `*`, `**`,
/// unary `-`, `++` / `--` and compound assignments panic with
/// `integer overflow` when the result does not fit; `/` and `%` by zero panic
/// with `division by zero`; `**` with a negative exponent panics with
/// `negative exponent`. `/` truncates toward zero and `%` takes the sign of
/// the left operand. `<<` and `>>` panic with `shift count out of range`
/// when the count is outside `0..=63` (a constant count is a compile error);
/// bitwise operators never panic. For wrapping,
/// checked or saturating arithmetic see the stdlib `arith` module.
///
/// Casts with `as`: `int as float` converts exactly when it can;
/// `int as byte` keeps the low 8 bits (an out-of-range literal is a
/// compile-time error); `int as bool` is `true` for any non-zero value.
/// Mixing `int` and `float` in one operator is a type error, so cast first.
///
/// `int` implements `Num`, `Integral`, `Ord`, `Eq` and `Show`.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// let n = 7;
/// assert!(n / 2 == 3);
/// assert!(-7 / 2 == -3);
/// assert!(-7 % 2 == -1);
/// assert!(2 ** 10 == 1024);
/// assert!(n as float == 7.0);
/// let big = 300;
/// assert!(big as byte == 44 as byte);
/// assert!(5 as bool);
/// ```
class int {}

/// A 64-bit IEEE-754 floating-point number.
///
/// Literals need digits on both sides of the point (`1.0`, `3.14`, not `1.`
/// or `.5`); there is no exponent form. Arithmetic follows IEEE-754 and
/// never panics: `1.0 / 0.0` is infinity and invalid operations give NaN.
/// `%` is the remainder with the sign of the left operand, like the `rem`
/// function.
///
/// Casts with `as`: `float as int` truncates toward zero (NaN becomes `0`,
/// infinities saturate). There is no direct cast between `float` and `byte`;
/// go through `int`. Mixing `int` and `float` in one operator is a type error.
///
/// `float` implements `Num`, `Ord`, `Eq` and `Show`, but not `Integral`.
/// Math functions such as `sqrt` and `sin` are in `prelude::math`.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// let x = 2.5;
/// assert!(x * 2.0 == 5.0);
/// assert!(7.5 % 2.0 == 1.5);
/// assert!(-2.7 as int == -2);
/// assert!(3 as float + x == 5.5);
/// assert!(1.0 / 0.0 > 1000000.0);
/// assert!(show(x) == "2.5");
/// ```
class float {}

/// A boolean: `true` or `false`.
///
/// Conditions in `if` and `while` take a `bool`. `&&` and `||` short-circuit;
/// `!` negates (it also accepts an `int`, where `0` is false). Booleans
/// support `==` and `!=` but are not ordered.
///
/// Casts with `as`: `bool as int` is `1` or `0`; `int as bool` is `true` for
/// any non-zero value.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// let ready = 3 > 2 && !false;
/// assert!(ready);
/// assert!(true as int == 1);
/// assert!(!(0 as bool));
/// assert!(show(ready) == "true");
/// ```
class bool {}

/// An unsigned 8-bit integer in `0..=255`.
///
/// There is no byte literal syntax. An integer literal in `0..=255` becomes a
/// `byte` where a `byte` is expected (an annotated `let`, an argument, a
/// return, a `Vec<byte>` element), and so does a string literal that encodes
/// to exactly one UTF-8 byte (`let c: byte = "A";`). A literal outside the
/// range is a compile-time error. Elsewhere, write `n as byte`.
///
/// `byte` arithmetic wraps modulo 256: `+`, `-`, `*`, `**`, `<<` and unary
/// `-` keep the low 8 bits (`200 + 100` is `44`, `0 - 1` is `255`), and each
/// such operator warns `E0130`. Convert to `int` first (`b as int`) for the
/// exact result. `/` and `%` by zero panic with `division by zero`.
///
/// Casts with `as`: `byte as int` is exact; `int as byte` keeps the low 8
/// bits. There is no direct cast to or from `float`.
///
/// `byte` implements `Eq`, `Show`, `Shl`, `Shr`, `BitAnd`, `BitOr` and
/// `BitXor`, and supports `<`, `<=`, `>`, `>=`. It is not `Num`, `BitNot`
/// or `Integral`. Byte buffers are `Vec<byte>`; see `string::to_bytes`.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// let a: byte = 65;
/// let c: byte = "A";
/// assert!(a == c);
/// assert!(a as int + 200 == 265);
/// assert!((a | 32 as byte) == 97 as byte);
/// let n = 257;
/// assert!(n as byte == 1 as byte);
/// ```
class byte {}

/// A UTF-8 text string.
///
/// Literals are written in double quotes, with escapes `\\`, `\"`, `\n`,
/// `\r`, `\t`, `\0`, `\e`, `\xHH` and `\u{HEX}`. `+` and `+=` concatenate two
/// strings (adding a non-string is a type error). `==` compares contents;
/// strings are not ordered. `s.len()` counts UTF-8 bytes, not characters.
///
/// A string is not cast with `as`: use `string::to_bytes` for its bytes and
/// `string::format` to build strings from other values. A one-byte string
/// literal coerces to `byte` where a `byte` is expected.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// let s = "héllo";
/// assert!(s.len() == 6);
/// let greeting = "hi" + ", " + "there";
/// greeting += "!";
/// assert!(greeting == "hi, there!");
/// assert!(show(s) == "héllo");
/// ```
class string {}

/// The type with a single value, `()`, used where there is no meaningful value.
///
/// A function with no `->` return type returns `unit`, and statements have
/// type `unit`. It is also written `void` (for example as an FFI return
/// type). `Result<unit, E>` is the usual result of an operation that can
/// fail but returns nothing.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// fn check(int n) -> Result<unit, string> {
///     if n < 0 {
///         raise "negative";
///     }
///     return ();
/// }
///
/// fn main() {
///     let ok = match check(1) {
///         Result::Ok(_) => true,
///         Result::Err(_) => false,
///     };
///     assert!(ok);
/// }
/// ```
class unit {}

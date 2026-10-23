// Doc stubs for the builtin `string` module (hover docs; never compiled).

/// Builds a string from the literal `fmt` and the arguments that follow it.
///
/// `format` takes any number of arguments after `fmt`; each `%` specifier
/// in `fmt` consumes the next one:
///
/// - `%i`: an `int` (or `byte`) in decimal.
/// - `%f`: a `float` (`1.5`, `2.0`).
/// - `%s`: a `string`.
/// - `%z`: a `bool` (`true` / `false`).
/// - `%v`: any value with a `Show` instance (enums, tuples, records, ...).
/// - `%x`, `%b`, `%u`: an `int` in hex, binary, or unsigned decimal.
/// - `%%`: a literal `%`.
///
/// `fmt` must be a string literal: the compiler checks every specifier
/// against its argument's type, and reports too many or too few arguments.
/// `format` is built into the compiler, not a library function.
///
/// # Examples
///
/// ```
/// use string::{format};
/// # use tests::assert;
/// assert!(format("%i-%s", 42, "x") == "42-x");
/// assert!(format("%f %z", 1.5, true) == "1.5 true");
/// assert!(format("%x %b %%", 255, 5) == "ff 101 %");
/// assert!(format("%v", (1, "a")) == "(1, a)");
/// ```
fn format(string fmt) -> string {}

/// Decodes `bytes` as UTF-8 text.
///
/// Returns `Err(IoError::InvalidInput)` when `bytes` is not valid UTF-8.
/// The inverse of `to_bytes`.
///
/// # Examples
///
/// ```
/// use io::{IoError};
/// use string::{from_bytes};
/// # use tests::assert;
/// let text = match from_bytes(Vec::from([111 as byte, 107 as byte])) {
///     Result::Ok(s) => s,
///     Result::Err(_) => "",
/// };
/// assert!(text == "ok");
/// let bad = match from_bytes(Vec::from([255 as byte])) {
///     Result::Err(IoError::InvalidInput) => true,
///     default => false,
/// };
/// assert!(bad);
/// ```
fn from_bytes(Vec<byte> bytes) -> Result<string, IoError> {}

/// Returns the part of `s` between byte offsets `start` (inclusive) and `end` (exclusive).
///
/// Offsets are clamped to `0..=s.len()`, and `end` to at least `start`, so
/// out-of-range offsets give a shorter (possibly empty) string rather than
/// an error. Returns `Err(IoError::InvalidInput)` when an offset falls
/// inside a multi-byte UTF-8 character.
///
/// # Examples
///
/// ```
/// use io::{IoError};
/// use string::{slice_bytes};
/// # use tests::assert;
/// let mid = match slice_bytes("hello", 1, 4) {
///     Result::Ok(s) => s,
///     Result::Err(_) => "",
/// };
/// assert!(mid == "ell");
/// let clamped = match slice_bytes("hello", 3, 100) {
///     Result::Ok(s) => s,
///     Result::Err(_) => "",
/// };
/// assert!(clamped == "lo");
/// let split_char = match slice_bytes("é", 0, 1) {
///     Result::Err(IoError::InvalidInput) => true,
///     default => false,
/// };
/// assert!(split_char);
/// ```
fn slice_bytes(string s, int start, int end) -> Result<string, IoError> {}

/// Encodes `s` as UTF-8 into a new `Vec<byte>`.
///
/// Always succeeds; the result has `s.len()` bytes. Use it to pass text to
/// `io::write` and the other byte-based IO functions.
///
/// # Examples
///
/// ```
/// use string::{to_bytes};
/// # use tests::assert;
/// let b = to_bytes("Hi");
/// assert!(b.len() == 2);
/// assert!(b[0] == 72 as byte && b[1] == 105 as byte);
/// ```
fn to_bytes(string s) -> Vec<byte> {}

// Doc stubs for the builtin `string` module (hover docs; never compiled).

/// Returns the byte at byte offset `i` of `s`, or `-1` when `i` is out of range.
///
/// Offsets count UTF-8 bytes, not characters, and `s.len()` is the byte
/// length. The result is `0..=255` for any offset in `0..s.len()`.
///
/// # Examples
///
/// ```
/// use string::{byte_at};
/// # use tests::assert;
/// assert!(byte_at("abc", 0) == 97);
/// assert!(byte_at("abc", 3) == -1);
/// assert!(byte_at("abc", -1) == -1);
/// ```
fn byte_at(string s, int i) -> int {}

/// Returns the byte offsets of every non-overlapping `needle` in `hay`, left
/// to right.
///
/// An empty `needle` matches at every character boundary, `0` through
/// `hay.len()`. `text::Searchable` methods use this for `string` patterns.
///
/// # Examples
///
/// ```
/// use string::{find_all};
/// # use tests::assert;
/// let at = find_all("a,b,,c", ",");
/// assert!(at.len() == 3 && at[0] == 1 && at[2] == 4);
/// assert!(find_all("aaaa", "aa").len() == 2);
/// ```
fn find_all(string hay, string needle) -> Vec<int> {}

/// Returns the byte offset of the first `needle` in `hay` at or after `start`, or `-1`.
///
/// `start` is clamped to `0..=hay.len()`. An empty `needle` matches at the
/// clamped `start`. Offsets are byte offsets.
///
/// # Examples
///
/// ```
/// use string::{find_from};
/// # use tests::assert;
/// assert!(find_from("a-b-c", "-", 0) == 1);
/// assert!(find_from("a-b-c", "-", 2) == 3);
/// assert!(find_from("a-b-c", "x", 0) == -1);
/// assert!(find_from("abc", "", 10) == 3);
/// ```
fn find_from(string hay, string needle, int start) -> int {}

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

/// Returns whether `needle` occurs in `s` starting exactly at byte offset `at`.
///
/// `false` when `at` is negative or the match would run past the end.
/// An empty `needle` matches at any offset in `0..=s.len()`.
///
/// # Examples
///
/// ```
/// use string::{match_at};
/// # use tests::assert;
/// assert!(match_at("key=value", "=", 3));
/// assert!(!match_at("key=value", "=", 2));
/// assert!(!match_at("ab", "abc", 0));
/// ```
fn match_at(string s, string needle, int at) -> bool {}

/// Returns `hay` with every non-overlapping `needle` replaced by `with`.
///
/// An empty `needle` inserts `with` at every character boundary.
///
/// # Examples
///
/// ```
/// use string::{replace_all};
/// # use tests::assert;
/// assert!(replace_all("a-b-c", "-", "+") == "a+b+c");
/// assert!(replace_all("ab", "", "-") == "-a-b-");
/// ```
fn replace_all(string hay, string needle, string with) -> string {}

/// Returns the byte offset of the last `needle` in `hay`, or `-1`.
///
/// An empty `needle` matches at `hay.len()`.
///
/// # Examples
///
/// ```
/// use string::{rfind};
/// # use tests::assert;
/// assert!(rfind("a/b/c", "/") == 3);
/// assert!(rfind("abc", "x") == -1);
/// assert!(rfind("abc", "") == 3);
/// ```
fn rfind(string hay, string needle) -> int {}

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

/// Returns the pieces of `hay` between the matches of `sep`.
///
/// A match at either end gives an empty first or last piece, and an empty
/// `sep` splits between every character (`["", "a", "b", ""]` for `"ab"`).
///
/// # Examples
///
/// ```
/// use string::{split};
/// # use tests::assert;
/// let parts = split("a,b,,c", ",");
/// assert!(parts.len() == 4 && parts[1] == "b" && parts[2] == "");
/// ```
fn split(string hay, string sep) -> Vec<string> {}

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

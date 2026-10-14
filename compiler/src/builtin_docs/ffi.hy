// Doc stubs for the builtin `ffi` module (hover docs; never compiled).

/// The category of an FFI failure, read from `Error.kind`.
///
/// Match on it to recover from a specific failure; use `Error.message` for
/// display. It implements `Show`, so `format("%v", k)` gives the variant name.
///
/// # Examples
///
/// ```
/// use ffi::{ErrorKind};
/// use string::{format};
/// # use tests::assert;
/// assert!(format("%v", ErrorKind::SymbolNotFound) == "SymbolNotFound");
/// ```
enum ErrorKind {
    /// The library passed the load gate but no file was found for it.
    LibraryNotFound,
    /// `declare` named a symbol the library does not export.
    SymbolNotFound,
    /// `invoke` got a different number of arguments than `declare` gave
    /// (fewer than the fixed prefix, for a variadic function).
    ArityMismatch,
    /// libffi rejected the call signature.
    Libffi,
    /// The signature is malformed: an empty or missing name, no return
    /// type, or `Void` used as an argument type.
    InvalidSignature,
    /// A library handle or function id is not one `dload` / `declare`
    /// returned.
    InvalidHandle,
    /// The call cannot be marshalled: an unsupported tag or value (such as
    /// `Bytes` as a return type), a string with a NUL byte, or a
    /// `read_ints` count out of range or null pointer.
    Unsupported,
    /// Anything else, including a library the load gate denied.
    Other,
}

/// The error of `dload`, `declare`, `invoke` and `read_ints`.
///
/// One record variant: read `e.kind` (an `ErrorKind`) to decide what to do
/// and `e.message` for a human-readable description.
///
/// # Examples
///
/// ```
/// use ffi::{Error, ErrorKind};
/// # use tests::assert;
/// fn describe(Error e) -> string {
///     return match e.kind {
///         ErrorKind::LibraryNotFound => "missing library: " + e.message,
///         default => e.message,
///     };
/// }
///
/// fn main() {
///     let e = Error::Error { kind: ErrorKind::LibraryNotFound, message: "demo" };
///     assert!(describe(e) == "missing library: demo");
/// }
/// ```
enum Error {
    /// The failure's category and its message.
    Error { kind: ErrorKind, message: string },
}

/// Loads a native shared library and returns a handle to it.
///
/// `path` is a stem (`"sum"` finds `libsum.so`, `libsum.dylib` or `sum.dll`
/// in the `--ffi-search-path` directories), a relative path or an absolute
/// path. Every library is gated by its file-name stem: the build needs
/// `--allow-dload STEM` (a literal stem without it is error `E0410`, and a
/// non-literal path is `E0411`; `-A` does not grant it), and at run time the
/// stem also needs `--dload-pin STEM=SHA256` or `--dload-trusted STEM`. A
/// denied load is `Err` with `ErrorKind::Other`. The libc aliases
/// (`dload("c")`) are always denied.
///
/// Returns `Err` with `ErrorKind::LibraryNotFound` when the stem is allowed
/// but no file is found.
///
/// # Examples
///
/// ```text
/// use ffi::{dload, ErrorKind};
///
/// // coil --allow-dload sum --dload-trusted sum --ffi-search-path native app.hy
/// let lib = match dload("sum") {
///     Result::Ok(h) => h,
///     Result::Err(e) => match e.kind {
///         ErrorKind::LibraryNotFound => panic "build libsum first",
///         default => panic e.message,
///     },
/// };
/// ```
fn dload(string path) -> Result<int, Error> {}

/// Looks up the C function `name` in a loaded library and records its signature.
///
/// `arg_types` is a tuple of FFI type tags from `ffi::types` (`(Int, Ptr)`,
/// or `()` for none), and `ret_type` is one tag (`Void` for no result).
/// Bare names (`int`, `float`, `string`, `void`) and `extern struct` types
/// are accepted too. Pass `true` as the optional fifth argument for a C
/// variadic function such as `printf`: `arg_types` is then the fixed prefix.
///
/// Returns the function id for `invoke`, or `Err` with
/// `ErrorKind::SymbolNotFound`, `Libffi` or `InvalidSignature`.
///
/// # Examples
///
/// ```text
/// use ffi::{dload, declare, invoke, Error};
/// use ffi::types::{Int};
///
/// fn add(int a, int b) -> Result<int, Error> {
///     let lib = dload("sum")?;
///     let sum_id = declare(lib, "sum", (Int, Int), Int)?;
///     return invoke(lib, sum_id, (a, b));
/// }
/// ```
fn declare<A, R>(int lib, string name, A arg_types, R ret_type, bool variadic) -> Result<int, Error> {}

/// Calls a function registered with `declare`, passing the tuple `args`.
///
/// The arguments are converted to C according to the declared tags. The
/// result type comes from the `declare` return tag (`unit` for `Void`) when
/// the compiler can trace `fn_id` back to its `declare`. Returns `Err` with
/// `ErrorKind::ArityMismatch` for a wrong argument count, or
/// `ErrorKind::InvalidHandle` for an unknown handle or id.
///
/// Native code runs with the full privileges of the process and is not
/// checked: a wrong signature or pointer can crash the program.
///
/// # Examples
///
/// ```text
/// use ffi::{dload, declare, invoke};
/// use ffi::types::{Int};
///
/// let lib = dload("sum")?;
/// let sum_id = declare(lib, "sum", (Int, Int), Int)?;
/// let n = invoke(lib, sum_id, (40, 2))?; // 42
/// ```
fn invoke<A, T>(int lib, int fn_id, A args) -> Result<T, Error> {}

/// Copies `count` 64-bit integers (`int64_t`) from native memory at `ptr`.
///
/// For C APIs that return a pointer to an array, such as PCRE2's
/// `pcre2_get_ovector_pointer`. `lib` must be a handle from `dload` (else
/// `ErrorKind::InvalidHandle`): reading native memory is part of the FFI
/// grant. A null `ptr` or a `count` outside `0..=1048576` is
/// `ErrorKind::Unsupported`. The pointer itself cannot be checked, so it
/// must point at `count` readable words.
///
/// # Examples
///
/// ```text
/// use ffi::{read_ints};
///
/// // Start and end offsets of every group in the last match: 2 words each.
/// let ovector = read_ints(lib, pcre2_get_ovector_pointer_8(match_data), groups * 2)?;
/// let first_start = ovector[0];
/// ```
fn read_ints(int lib, int ptr, int count) -> Result<Vec<int>, Error> {}

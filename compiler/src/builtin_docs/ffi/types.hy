// Doc stubs for the builtin `ffi::types` module (hover docs; never compiled).
//
// Each FFI type tag is its own export. A tag names a C type in the tuple of
// argument types and the return type given to `ffi::declare`.

/// FFI tag for a 64-bit signed integer (`int64_t`), passed from and returned as `int`.
///
/// The bare name `int` means the same in `declare` and `extern` blocks.
///
/// # Examples
///
/// ```text
/// use ffi::{dload, declare, invoke};
/// use ffi::types::{Int};
///
/// let lib = dload("sum")?;
/// let sum_id = declare(lib, "sum", (Int, Int), Int)?; // int64_t sum(int64_t, int64_t)
/// let n = invoke(lib, sum_id, (40, 2))?;
/// ```
class Int {}

/// FFI tag for a 64-bit float (`double`), passed from and returned as `float`.
///
/// The bare name `float` means the same.
class Float {}

/// FFI tag for a NUL-terminated C string (`const char *`), passed from and returned as `string`.
///
/// A Coil string is passed as a pointer to its UTF-8 bytes; C must not keep
/// it after the call. A returned `char *` is copied into a new Coil string
/// at once (a null pointer gives an empty string), and C keeps ownership of
/// its memory.
class String {}

/// FFI tag for no value (`void`), valid only as a return type.
///
/// `invoke` then returns `Ok(())`. As an argument type it is
/// `ErrorKind::InvalidSignature`. The bare name `void` means the same.
class Void {}

/// FFI tag for a C `bool` (one byte, `0` or `1`), passed from and returned as `bool`.
class Bool {}

/// FFI tag for an 8-bit signed integer (`int8_t`), passed from and returned as `int`.
///
/// Wider values are truncated when passed.
class Int8 {}

/// FFI tag for a 16-bit signed integer (`int16_t`), passed from and returned as `int`.
///
/// Wider values are truncated when passed.
class Int16 {}

/// FFI tag for a 32-bit signed integer (`int32_t`), passed from and returned as `int`.
///
/// Wider values are truncated when passed.
class Int32 {}

/// FFI tag for an 8-bit unsigned integer (`uint8_t`), passed from and returned as `int`.
class UInt8 {}

/// FFI tag for a 16-bit unsigned integer (`uint16_t`), passed from and returned as `int`.
class UInt16 {}

/// FFI tag for a 32-bit unsigned integer (`uint32_t`), passed from and returned as `int`.
class UInt32 {}

/// FFI tag for a 64-bit unsigned integer (`uint64_t`), passed from and returned as `int`.
///
/// Values above the largest `int` come back negative (the same 64 bits).
class UInt64 {}

/// FFI tag for a raw pointer (`void *`), returned as an `int` address.
///
/// A `Vec<T>`, `[T; N]` or tuple type written in a `declare` signature also
/// means `Ptr`.
///
/// As an argument, an `int` is passed as the address itself, and a non-empty
/// array or tuple of `int` is passed as a pointer to a temporary
/// `int64_t[]` copy whose changes are copied back after the call. Keep a
/// value C retains alive with `gc::root`.
///
/// # Examples
///
/// ```text
/// use ffi::{dload, declare, invoke};
/// use ffi::types::{Int, Ptr};
///
/// let lib = dload("sum")?;
/// // int64_t sum_array(const int64_t *xs, int64_t n)
/// let id = declare(lib, "sum_array", (Ptr, Int), Int)?;
/// let n = invoke(lib, id, ([1, 2, 3, 4, 5], 5))?; // 15
/// ```
class Ptr {}

/// FFI tag for a C function pointer, letting C call back into a Coil function.
///
/// As an argument, pass a named Coil function; C receives a pointer it can
/// call. The callback signature is currently fixed to `int64_t (*)(int64_t)`,
/// so the Coil function must take one `int` and return `int`. As a return
/// type, the function pointer comes back as an opaque `int` address, like
/// `Ptr`.
///
/// # Examples
///
/// ```text
/// use ffi::{dload, declare, invoke};
/// use ffi::types::{Callback, Int};
///
/// fn doubler(int x) -> int {
///     return x * 2;
/// }
///
/// // int64_t apply_cb(int64_t (*f)(int64_t), int64_t x) { return f(x); }
/// let id = declare(lib, "apply_cb", (Callback, Int), Int)?;
/// let n = invoke(lib, id, (doubler, 21))?; // 42
/// ```
class Callback {}

/// FFI tag for a C struct passed or returned by value.
///
/// Not written directly: name an `extern struct` type in `declare` instead,
/// and the compiler uses this tag with that struct's layout. A returned
/// struct becomes a record whose fields are read with `.name`.
///
/// # Examples
///
/// ```text
/// use ffi::{dload, declare, invoke};
/// use ffi::types::{Int32};
///
/// extern struct Point {
///     x: int32,
///     y: int32,
/// };
///
/// let make_id = declare(lib, "make_point", (Int32, Int32), Point)?;
/// let p = invoke(lib, make_id, (3, 4))?;
/// let sum = p.x + p.y; // 7
/// ```
class Struct {}

/// FFI tag for a writable byte buffer (`uint8_t *`) made from a `Vec<byte>`.
///
/// C gets a pointer to a copy of the bytes, and whatever it writes there is
/// copied back into the `Vec` after the call. No NUL terminator is added, so
/// pass the length separately. Argument only: as a return type it is
/// `ErrorKind::Unsupported` (return a `Ptr` instead).
class Bytes {}

/// FFI tag for a read-only byte view (`const uint8_t *`) of a `string` or `Vec<byte>`.
///
/// A string is lent in place without a copy; a `Vec<byte>` is copied in
/// once and never copied back. No NUL terminator is added, so pass the
/// length separately. Argument only.
class ConstBytes {}

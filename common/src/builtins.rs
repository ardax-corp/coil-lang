//! Compiler-provided built-in enums (`Option`, `Result`, `FFIType`).

pub use crate::ffi::{
    BUILTIN_FFI_TYPE_ENUM, BUILTIN_FFI_TYPE_VARIANTS, is_builtin_ffi_enum, is_builtin_ffi_variant,
};

/// Built-in `Option` enum name.
pub const BUILTIN_OPTION_ENUM: &str = "Option";

/// `Option` variants in tag order: `None` = 0, `Some` = 1.
pub const BUILTIN_OPTION_VARIANTS: &[&str] = &["None", "Some"];

/// Built-in `Result` enum name.
pub const BUILTIN_RESULT_ENUM: &str = "Result";

/// `Result` variants in tag order: `Ok` = 0, `Err` = 1.
pub const BUILTIN_RESULT_VARIANTS: &[&str] = &["Ok", "Err"];

/// Built-in `IoError` enum name (virtual `io` module).
pub const BUILTIN_IO_ERROR_ENUM: &str = "IoError";

/// `IoError` variants in tag order.
///
/// Append-only: existing discriminants must stay stable for match tags.
pub const BUILTIN_IO_ERROR_VARIANTS: &[&str] = &[
    "WouldBlock",
    "NotFound",
    "PermissionDenied",
    "AlreadyClosed",
    "InvalidInput",
    "Other",
    "NotADirectory",
    "AlreadyExists",
    "TimedOut",
    "Truncated",
    "Certificate",
    "Handshake",
];

/// Built-in `ThreadError` enum name (virtual `thread` module).
pub const BUILTIN_THREAD_ERROR_ENUM: &str = "ThreadError";

/// `ThreadError` variants in tag order.
pub const BUILTIN_THREAD_ERROR_VARIANTS: &[&str] = &[
    "WouldBlock",
    "Disconnected",
    "JoinFailed",
    "NotSendable",
    "Poisoned",
    "Other",
];

/// Largest argument count accepted by the `thread_spawn` host native.
///
/// Surface `spawn` takes at most one argument, but auto-par specialization
/// spawns arbitrary-arity recursive calls directly.
pub const MAX_THREAD_SPAWN_ARGS: usize = 16;

/// Built-in `EnvError` enum name (virtual `env` module).
pub const BUILTIN_ENV_ERROR_ENUM: &str = "EnvError";

/// Built-in `TimeError` enum name (virtual `time` module).
pub const BUILTIN_TIME_ERROR_ENUM: &str = "TimeError";

/// `TimeError` variants in tag order.
pub const BUILTIN_TIME_ERROR_VARIANTS: &[&str] =
    &["InvalidInput", "Overflow", "ParseError", "Other"];

/// `EnvError` variants in tag order.
pub const BUILTIN_ENV_ERROR_VARIANTS: &[&str] = &[
    "InvalidInput",
    "NotFound",
    "ExecDisabled",
    "ExecFailed",
    "Other",
];

/// Built-in `ErrorKind` enum name (virtual `ffi` module).
pub const BUILTIN_FFI_ERROR_KIND_ENUM: &str = "ErrorKind";

/// `ErrorKind` variants in tag order (userland FFI failures).
pub const BUILTIN_FFI_ERROR_KIND_VARIANTS: &[&str] = &[
    "LibraryNotFound",
    "SymbolNotFound",
    "ArityMismatch",
    "Libffi",
    "InvalidSignature",
    "InvalidHandle",
    "Unsupported",
    "Other",
];

/// Built-in `Error` enum name (virtual `ffi` module).
///
/// Single record variant `Error { kind: ErrorKind, message: string }` so
/// callers can check `e.kind` and read `e.message` without string matching.
pub const BUILTIN_FFI_ERROR_ENUM: &str = "Error";

/// Sole variant of [`BUILTIN_FFI_ERROR_ENUM`].
pub const BUILTIN_FFI_ERROR_VARIANT: &str = "Error";

/// True when `name` is a reserved built-in enum (`Option`, `Result`, `IoError`,
/// `Error` / `ErrorKind`, or `FFIType`).
pub fn is_builtin_enum(name: &str) -> bool {
    is_builtin_option_enum(name)
        || is_builtin_result_enum(name)
        || is_builtin_io_error_enum(name)
        || is_builtin_thread_error_enum(name)
        || is_builtin_env_error_enum(name)
        || is_builtin_time_error_enum(name)
        || is_builtin_ffi_error_enum(name)
        || is_builtin_ffi_error_kind_enum(name)
        || is_builtin_ffi_enum(name)
}

pub fn is_builtin_io_error_enum(name: &str) -> bool {
    name == BUILTIN_IO_ERROR_ENUM
}

pub fn is_builtin_thread_error_enum(name: &str) -> bool {
    name == BUILTIN_THREAD_ERROR_ENUM
}

pub fn is_builtin_env_error_enum(name: &str) -> bool {
    name == BUILTIN_ENV_ERROR_ENUM
}

pub fn is_builtin_time_error_enum(name: &str) -> bool {
    name == BUILTIN_TIME_ERROR_ENUM
}

pub fn is_builtin_ffi_error_enum(name: &str) -> bool {
    name == BUILTIN_FFI_ERROR_ENUM
}

pub fn is_builtin_ffi_error_kind_enum(name: &str) -> bool {
    name == BUILTIN_FFI_ERROR_KIND_ENUM
}

pub fn is_builtin_option_enum(name: &str) -> bool {
    name == BUILTIN_OPTION_ENUM
}

pub fn is_builtin_result_enum(name: &str) -> bool {
    name == BUILTIN_RESULT_ENUM
}

/// True when `name` is a polymorphic built-in sum (`Option` or `Result`).
pub fn is_poly_builtin_enum(name: &str) -> bool {
    is_builtin_option_enum(name) || is_builtin_result_enum(name)
}

/// Built-in nominal matrix wrapper (`Matrix<Data>`).
///
/// `Data` is a nested static array/tuple layout (`[[T; N]; M]`). Runtime
/// representation is the nested data itself (zero-cost wrap); `*` is
/// matmul via `Mul`, not element-wise zip.
pub const BUILTIN_MATRIX_TYPE: &str = "Matrix";

pub fn is_builtin_matrix_type(name: &str) -> bool {
    name == BUILTIN_MATRIX_TYPE
}

/// Explicit GC keep-alive handle (`Root<T>` from virtual `gc`).
pub const BUILTIN_ROOT_TYPE: &str = "Root";

/// Non-rooting GC handle (`Weak<T>` from virtual `gc`).
pub const BUILTIN_WEAK_TYPE: &str = "Weak";

/// Growable heap vector (`Vec<T>`) — replaces dynamic `[T]` arrays.
pub const BUILTIN_VEC_TYPE: &str = "Vec";

pub fn is_builtin_root_type(name: &str) -> bool {
    name == BUILTIN_ROOT_TYPE
}

pub fn is_builtin_weak_type(name: &str) -> bool {
    name == BUILTIN_WEAK_TYPE
}

pub fn is_builtin_vec_type(name: &str) -> bool {
    name == BUILTIN_VEC_TYPE
}

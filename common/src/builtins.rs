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
    "NotATerminal",
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

/// Built-in type constructors, besides `Option` / `Result`, that are keyed
/// by their bare name in every module: a user `class` or `enum` cannot take
/// one of these names (#1654).
pub const BUILTIN_RESERVED_TYPE_NAMES: &[&str] = &[
    "Range",
    "RangeInclusive",
    "ArrayIter",
    BUILTIN_MATRIX_TYPE,
    BUILTIN_ROOT_TYPE,
    BUILTIN_WEAK_TYPE,
    BUILTIN_VEC_TYPE,
];

/// True when `name` is one of [`BUILTIN_RESERVED_TYPE_NAMES`].
pub fn is_reserved_builtin_type(name: &str) -> bool {
    BUILTIN_RESERVED_TYPE_NAMES.contains(&name)
}

/// Primitive type names a type annotation matches in any letter case
/// (`String` is `string`), so a user `class` or `enum` cannot take one of
/// these names in any spelling (#1673).
pub const BUILTIN_PRIMITIVE_TYPE_NAMES: &[&str] = &["int", "float", "bool", "byte", "string", "void"];

/// The primitive `name` spells in any letter case, if any.
pub fn builtin_primitive_type_spelling(name: &str) -> Option<&'static str> {
    BUILTIN_PRIMITIVE_TYPE_NAMES
        .iter()
        .copied()
        .find(|p| p.eq_ignore_ascii_case(name))
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

/// Heap `Range` `InitTyped` id. High so user class ids (from 1) stay put.
pub const RANGE_HEAP_TYPE_ID: u32 = 0xFFFE;

/// Heap `RangeInclusive` `InitTyped` id. Inclusive lives in the type, not a slot.
pub const RANGE_INCLUSIVE_HEAP_TYPE_ID: u32 = 0xFFFD;

/// `io::fs::metadata`'s record, built by the host. Records are slotted
/// objects whose slots are their fields sorted by name.
pub const FS_METADATA_TYPE_ID: u32 = 0xFFFC;

/// An `extern struct` value an FFI call returns: a record, its slots the
/// struct's fields sorted by name.
pub const FFI_STRUCT_TYPE_ID: u32 = 0xFFFB;

/// First `InitTyped` id of a structural record type. Records take ids
/// from their own range, so the VM tells a record (`==` compares its
/// slots) from a class instance (`==` is identity) by its id alone.
pub const RECORD_TYPE_ID_BASE: u32 = 0x8000;

/// End (exclusive) of the record id range; the reserved host ids sit
/// above it.
pub const RECORD_TYPE_ID_END: u32 = 0xFF00;

/// Whether `type_id` names a record: a compiler-allocated record type or
/// a host-built one (`fs::metadata`, an FFI struct return).
pub fn is_record_type_id(type_id: u32) -> bool {
    (RECORD_TYPE_ID_BASE..RECORD_TYPE_ID_END).contains(&type_id)
        || type_id == FS_METADATA_TYPE_ID
        || type_id == FFI_STRUCT_TYPE_ID
}

/// Index of `type_id`'s row in the VM's dense field word kinds table:
/// class ids on even rows, record ids (from [`RECORD_TYPE_ID_BASE`]) on
/// odd rows, so neither range leaves a gap.
pub fn type_kinds_index(type_id: u32) -> usize {
    if (RECORD_TYPE_ID_BASE..RECORD_TYPE_ID_END).contains(&type_id) {
        (type_id - RECORD_TYPE_ID_BASE) as usize * 2 + 1
    } else {
        type_id as usize * 2
    }
}

/// The fields of `io::fs::metadata`'s record in slot (name) order.
pub const FS_METADATA_FIELDS: [&str; 5] = ["is_dir", "is_file", "is_symlink", "modified_unix", "size"];

/// `Some(slot)` when `type_id` is a slotted numeric Range and `name` is
/// `start`/`end`, or the host's `fs::metadata` record and `name` one of its
/// fields (a named read of a host-built slotted object).
pub fn range_heap_field_slot(type_id: u32, name: &str) -> Option<usize> {
    if type_id == FS_METADATA_TYPE_ID {
        return FS_METADATA_FIELDS.iter().position(|f| *f == name);
    }
    if type_id != RANGE_HEAP_TYPE_ID && type_id != RANGE_INCLUSIVE_HEAP_TYPE_ID {
        return None;
    }
    match name {
        "start" => Some(0),
        "end" => Some(1),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_type_ids_are_told_from_class_ids() {
        assert!(!is_record_type_id(1));
        assert!(is_record_type_id(RECORD_TYPE_ID_BASE));
        assert!(is_record_type_id(FS_METADATA_TYPE_ID));
        assert!(!is_record_type_id(RANGE_HEAP_TYPE_ID));
        assert_eq!(type_kinds_index(3), 6);
        assert_eq!(type_kinds_index(RECORD_TYPE_ID_BASE + 2), 5);
    }

    #[test]
    fn time_error_is_not_a_compiler_builtin() {
        assert!(!is_builtin_enum("TimeError"));
        let src = include_str!("builtins.rs");
        let prod = src.split("#[cfg(test)]").next().unwrap_or(src);
        assert!(
            !prod.contains("TimeError") && !prod.contains("BUILTIN_TIME_ERROR"),
            "compiler TimeError enum must stay deleted"
        );
    }

    #[test]
    fn range_heap_slots_are_start_end() {
        assert_eq!(range_heap_field_slot(RANGE_HEAP_TYPE_ID, "start"), Some(0));
        assert_eq!(
            range_heap_field_slot(RANGE_INCLUSIVE_HEAP_TYPE_ID, "end"),
            Some(1)
        );
        assert_eq!(range_heap_field_slot(1, "start"), None);
        assert_eq!(range_heap_field_slot(RANGE_HEAP_TYPE_ID, "inclusive"), None);
    }

    #[test]
    fn fs_metadata_slots_are_sorted_names() {
        let mut sorted = FS_METADATA_FIELDS;
        sorted.sort_unstable();
        assert_eq!(sorted, FS_METADATA_FIELDS);
        assert_eq!(range_heap_field_slot(FS_METADATA_TYPE_ID, "size"), Some(4));
    }
}

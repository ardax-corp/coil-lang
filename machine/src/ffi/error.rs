//! Runtime tags and allocation for virtual `ffi::Error` / `ffi::ErrorKind`.

use common::{BUILTIN_FFI_ERROR_KIND_VARIANTS, BUILTIN_FFI_ERROR_VARIANT, Value};

#[cfg(test)]
use crate::memory::Object;
use crate::memory::{Heap, Member};

use super::signature::FfiError;

/// Tag indices for [`ErrorKind`](common::BUILTIN_FFI_ERROR_KIND_ENUM).
#[repr(u32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FfiErrorKindTag {
    LibraryNotFound = 0,
    SymbolNotFound = 1,
    ArityMismatch = 2,
    Libffi = 3,
    InvalidSignature = 4,
    InvalidHandle = 5,
    Unsupported = 6,
    Other = 7,
}

impl FfiErrorKindTag {
    pub fn from_ffi_error(err: &FfiError) -> Self {
        match err {
            FfiError::LibraryNotFound { .. } => Self::LibraryNotFound,
            FfiError::LibraryDenied { .. } => Self::Other,
            FfiError::SymbolNotFound { .. } => Self::SymbolNotFound,
            FfiError::ArityMismatch { .. } => Self::ArityMismatch,
            FfiError::Libffi(_) => Self::Libffi,
            FfiError::MissingName
            | FfiError::MissingReturnType
            | FfiError::VoidArgument { .. }
            | FfiError::EmptyName => Self::InvalidSignature,
            FfiError::Unsupported(_) => Self::Unsupported,
            FfiError::InvalidHandle(_) => Self::InvalidHandle,
        }
    }
}

/// Allocate a unit-payload `ErrorKind` variant.
pub fn alloc_ffi_error_kind(heap: &mut Heap, tag: FfiErrorKindTag) -> Value {
    let _ = BUILTIN_FFI_ERROR_KIND_VARIANTS;
    alloc_enum(heap, tag as u32, vec![])
}

/// Allocate `Error::Error { kind, message }` on the heap.
pub fn alloc_ffi_error(heap: &mut Heap, kind: FfiErrorKindTag, message: String) -> Value {
    let _ = BUILTIN_FFI_ERROR_VARIANT;
    let kind_val = alloc_ffi_error_kind(heap, kind);
    let msg_gc = heap.intern(message);
    let msg_val = Value::from(msg_gc.as_ptr() as *mut u8 as u64);
    // Record payload in declaration order: kind (0), message (1).
    alloc_enum(
        heap,
        0,
        vec![
            member_from_value(heap, kind_val),
            member_from_value(heap, msg_val),
        ],
    )
}

/// Allocate `Result::Err(ffi::Error { kind, message })`.
pub fn alloc_result_ffi_err(heap: &mut Heap, kind: FfiErrorKindTag, message: String) -> Value {
    let err = alloc_ffi_error(heap, kind, message);
    crate::io::alloc_result_err(heap, err)
}

fn alloc_enum(heap: &mut Heap, tag: u32, payload: Vec<Member>) -> Value {
    heap.alloc_enum_value(tag, payload)
}

fn member_from_value(heap: &Heap, value: Value) -> Member {
    if !value.raw().is_null()
        && let Some(obj) = heap.find_object_by_addr(value.raw() as u64)
    {
        Member::Object(obj)
    } else {
        Member::Value(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::Heap;

    #[test]
    fn alloc_ffi_error_packs_kind_and_message() {
        let mut heap = Heap::default();
        let err = alloc_ffi_error(
            &mut heap,
            FfiErrorKindTag::LibraryNotFound,
            "missing lib".into(),
        );
        let obj = heap
            .find_object_by_addr(err.raw() as u64)
            .expect("Error enum on heap");
        let Object::Enum(gc) = obj else {
            panic!("expected Enum");
        };
        let e = gc.as_ref();
        assert_eq!(e.tag, 0);
        assert_eq!(e.payload.len(), 2);
        match &e.payload[0] {
            Member::Object(Object::Enum(kind_gc)) => {
                assert_eq!(
                    kind_gc.as_ref().tag,
                    FfiErrorKindTag::LibraryNotFound as u32
                );
            }
            _ => panic!("kind should be ErrorKind unit enum"),
        }
        match &e.payload[1] {
            Member::Object(Object::String(s)) => {
                assert_eq!(s.as_ref().data, "missing lib");
            }
            _ => panic!("message should be ObjString"),
        }
    }

    #[test]
    fn from_ffi_error_maps_variants() {
        assert_eq!(
            FfiErrorKindTag::from_ffi_error(&FfiError::SymbolNotFound { name: "foo".into() }),
            FfiErrorKindTag::SymbolNotFound
        );
        assert_eq!(
            FfiErrorKindTag::from_ffi_error(&FfiError::EmptyName),
            FfiErrorKindTag::InvalidSignature
        );
        assert_eq!(
            FfiErrorKindTag::from_ffi_error(&FfiError::Unsupported("x".into())),
            FfiErrorKindTag::Unsupported
        );
        assert_eq!(
            FfiErrorKindTag::from_ffi_error(&FfiError::LibraryDenied {
                name: "c".into(),
                stem: "c".into(),
            }),
            FfiErrorKindTag::Other
        );
        assert_eq!(
            FfiErrorKindTag::from_ffi_error(&FfiError::LibraryNotFound {
                name: "crypto".into(),
                tried: vec![],
                detail: String::new(),
            }),
            FfiErrorKindTag::LibraryNotFound
        );
    }
}

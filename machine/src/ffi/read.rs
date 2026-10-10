//! `ffi::read_ints`: copy C `int64_t`s out of memory a native handed back.

use common::Value;

use crate::memory::{Heap, ObjArray, Object};

use super::error::{FfiErrorKindTag, alloc_ffi_error};

/// Most words one call copies (8 MiB); a larger count is a bug, not data.
const MAX_WORDS: i64 = 1 << 20;

/// `read_ints(lib, ptr, count) -> Result<Vec<int>, ffi::Error>`.
///
/// `lib` must be a handle `dload` returned: reading native memory is part of
/// the FFI capability, not something plain code can do. `ptr` is trusted
/// the same way an `invoke` argument is; a bad pointer crashes like a bad
/// call would.
pub fn read_ints(heap: &mut Heap, args: &[Value]) -> Value {
    let r = match words_at(heap, args) {
        Ok(words) => {
            let elements = words.into_iter().map(Value::from).collect();
            let (obj, _) = heap.alloc(ObjArray::new(elements), Object::Array);
            Ok(Value::from(obj.addr()))
        }
        Err((kind, message)) => Err(alloc_ffi_error(heap, kind, message)),
    };
    crate::host_enum::pack_result_or_panic(heap, r)
}

fn words_at(heap: &Heap, args: &[Value]) -> Result<Vec<i64>, (FfiErrorKindTag, String)> {
    let [lib, ptr, count] = args else {
        return Err((FfiErrorKindTag::ArityMismatch, "read_ints takes 3 arguments".into()));
    };
    if !matches!(
        heap.find_object_by_addr(lib.raw() as u64),
        Some(Object::Library(_))
    ) {
        return Err((
            FfiErrorKindTag::InvalidHandle,
            "read_ints: first argument is not a dload library handle".into(),
        ));
    }
    let count = count.as_int();
    if !(0..=MAX_WORDS).contains(&count) {
        return Err((
            FfiErrorKindTag::Unsupported,
            format!("read_ints: count {count} is outside 0..={MAX_WORDS}"),
        ));
    }
    if count == 0 {
        return Ok(Vec::new());
    }
    let ptr = ptr.as_int() as usize as *const i64;
    if ptr.is_null() {
        return Err((FfiErrorKindTag::Unsupported, "read_ints: null pointer".into()));
    }
    Ok((0..count as usize)
        // SAFETY: the caller vouches for `count` words at `ptr`, as for any
        // pointer passed through `invoke`; unaligned reads are allowed.
        .map(|i| unsafe { ptr.add(i).read_unaligned() })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::Heap;

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn copies_words_through_a_library_handle() {
        let lib = crate::ffi::library_candidates("c", None, &[])
            .into_iter()
            .find_map(|c| unsafe { libloading::Library::new(&c) }.ok());
        let Some(lib) = lib else {
            if std::env::var_os("CI").is_some() {
                panic!("FFI soft-skip forbidden in CI: libc not reachable via dlopen");
            }
            return;
        };
        let mut heap = Heap::default();
        let (handle, _) = heap.alloc_library(std::sync::Arc::new(lib));
        let words = [7_i64, -8, i64::MAX];
        let got = words_at(
            &heap,
            &[
                Value::from(handle.addr()),
                Value::from(words.as_ptr() as i64),
                Value::from(3_i64),
            ],
        );
        assert_eq!(got.ok(), Some(words.to_vec()));
    }

    #[test]
    fn rejects_a_non_library_handle() {
        let heap = Heap::default();
        let words = [7_i64, 8];
        let got = words_at(
            &heap,
            &[Value::from(0_i64), Value::from(words.as_ptr() as i64), Value::from(2_i64)],
        );
        assert!(matches!(got, Err((FfiErrorKindTag::InvalidHandle, _))));
    }

    #[test]
    fn rejects_negative_and_huge_counts() {
        let heap = Heap::default();
        for count in [-1, MAX_WORDS + 1] {
            let err = words_at(&heap, &[Value::from(0_i64), Value::from(8_i64), Value::from(count)]);
            assert!(err.is_err());
        }
    }
}

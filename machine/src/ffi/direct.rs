//! Pointer arguments of direct extern calls from native code, and the
//! copies a call needs, without libffi (`coil_native_ffi_arg`).

use std::ffi::c_void;

use common::Value;

use crate::memory::{Heap, Object};

use super::signature::FfiError;

/// How a direct extern call passes a pointer argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectPtr {
    /// A `string` as a NUL-terminated copy for the call.
    CStr,
    /// A `string` or `Vec<byte>` as `const uint8_t *`.
    ConstBytes,
    /// A `Vec<byte>` as `uint8_t *`, written back.
    Bytes,
    /// A `ptr`: an `int` address, or an `int` array or tuple as a
    /// temporary `int64_t[]` written back.
    Ptr,
}

/// The copies the pointer arguments of one direct extern call made, until
/// [`Self::finish`] writes them back after the call.
#[derive(Default)]
pub struct DirectCopies {
    ints: Vec<Vec<i64>>,
    ints_back: Vec<(u64, usize)>,
    bytes: Vec<Vec<u8>>,
    bytes_back: Vec<(u64, usize)>,
}

impl DirectCopies {
    /// `value` as the C pointer a direct extern call passes for `kind`,
    /// with the same rules as the runtime's call: a buffer is lent in place
    /// when `lend` (no other argument is the same word) and it is packed,
    /// else copied (and written back by [`Self::finish`] for `Bytes` and an
    /// array `Ptr`).
    pub fn arg(&mut self, heap: &mut Heap, kind: DirectPtr, value: Value, lend: bool) -> Result<u64, FfiError> {
        Ok(match kind {
            DirectPtr::CStr => match heap.cstr_from_addr(value.bits()) {
                Ok(Some(p)) => p as u64,
                Ok(None) => heap.intern_ffi_bytes(b"").map_err(|_| FfiError::InteriorNul)? as u64,
                Err(crate::memory::InteriorNul) => return Err(FfiError::InteriorNul),
            },
            DirectPtr::ConstBytes | DirectPtr::Bytes => {
                let writable = kind == DirectPtr::Bytes;
                let (p, back) = byte_arg_from_value(heap, &value, writable, lend, &mut self.bytes)?;
                if let Some(addr) = back {
                    self.bytes_back.push((addr, self.bytes.len() - 1));
                }
                p as u64
            }
            DirectPtr::Ptr => {
                let (p, back) = array_buffer_from_value(heap, &value, &mut self.ints)?;
                if let Some(addr) = back {
                    self.ints_back.push((addr, self.ints.len() - 1));
                }
                p as u64
            }
        })
    }

    /// A call that does not run after all (an argument failed): drop the
    /// copies without writing them back, with the call's C strings.
    pub fn discard(&mut self, heap: &mut Heap) {
        self.ints.clear();
        self.ints_back.clear();
        self.bytes.clear();
        self.bytes_back.clear();
        heap.reset_ffi_strings();
    }

    /// After the call: write the copies back and drop them, with the
    /// call's C strings.
    pub fn finish(&mut self, heap: &mut Heap) {
        copy_array_buffers_back(heap, &self.ints_back, &self.ints);
        copy_byte_buffers_back(heap, &self.bytes_back, &self.bytes);
        self.discard(heap);
    }
}

pub(super) fn array_buffer_from_value(
    heap: &Heap,
    value: &Value,
    bufs: &mut Vec<Vec<i64>>,
) -> Result<(*mut c_void, Option<u64>), FfiError> {
    let addr = value.bits();
    if let Some(obj) = heap.find_object_by_addr(addr) {
        let mut buf: Vec<i64> = match &obj {
            Object::Array(gc) => gc.as_ref().iter().map(|v| v.as_int()).collect(),
            Object::Enum(gc) if gc.as_ref().is_tuple() => gc.as_ref().elements().iter().map(|v| v.as_int()).collect(),
            _ => Vec::new(),
        };
        if !buf.is_empty() {
            let ptr = buf.as_mut_ptr() as *mut c_void;
            bufs.push(buf);
            return Ok((ptr, Some(addr)));
        }
    }
    Ok((value.raw() as *mut c_void, None))
}

/// Backing for an empty byte view, so C never gets a dangling pointer.
static EMPTY_CONST_BYTES: [u8; 1] = [0];

/// A byte argument as C sees it: a `Vec<byte>` (or byte tuple) as `Bytes`
/// (`uint8_t *`, `writable`), or a `string` / `Vec<byte>` (or byte tuple) as
/// `ConstBytes` (read-only `const uint8_t *`). No NUL terminator; the callee
/// takes the length separately. `Some(addr)` when the bytes are a copy that
/// goes back into that array after the call.
///
/// A string is lent in place: its bytes never move or change while the
/// string lives (`memory::strbuf`), and the interpreter roots the call's arguments
/// until it returns. A packed array is lent in place too when `lend` (no
/// coil callback can run during the call, so nothing can grow, unpack or
/// free it) and no other argument names the same array (so C sees no
/// aliasing a copy would not have had): a `Bytes` callee then writes the
/// array itself, which is what copying back gave. Anything else (a
/// word-stored array, a tuple, an empty array) is copied in, and a
/// `Bytes` array copied back, as before.
pub(super) fn byte_arg_from_value(
    heap: &Heap,
    value: &Value,
    writable: bool,
    lend: bool,
    bufs: &mut Vec<Vec<u8>>,
) -> Result<(*mut c_void, Option<u64>), FfiError> {
    let addr = value.bits();
    if addr == 0 && !writable {
        return Ok((EMPTY_CONST_BYTES.as_ptr() as *mut c_void, None));
    }
    let (mut buf, copy_back): (Vec<u8>, bool) = match heap.find_object_by_addr(addr) {
        Some(Object::String(gc)) if !writable => {
            let bytes = gc.as_ref().data.as_bytes();
            if bytes.is_empty() {
                return Ok((EMPTY_CONST_BYTES.as_ptr() as *mut c_void, None));
            }
            return Ok((bytes.as_ptr() as *mut c_void, None));
        }
        Some(Object::Array(mut gc)) => {
            if lend
                && let Some(bytes) = gc.as_mut().bytes_mut()
                && !bytes.is_empty()
            {
                return Ok((bytes.as_mut_ptr() as *mut c_void, None));
            }
            (gc.as_ref().to_byte_vec(), writable)
        }
        Some(Object::Enum(gc)) if gc.as_ref().is_tuple() => {
            (gc.as_ref().elements().iter().map(|v| v.as_int() as u8).collect(), false)
        }
        _ => {
            return Err(FfiError::Unsupported(if writable {
                "Bytes argument must be a Vec<byte>".into()
            } else {
                "ConstBytes argument must be a string or Vec<byte>".into()
            }));
        }
    };
    // Never hand C a dangling pointer for an empty buffer.
    buf.reserve(1);
    let ptr = buf.as_mut_ptr() as *mut c_void;
    bufs.push(buf);
    Ok((ptr, copy_back.then_some(addr)))
}

pub(super) fn copy_byte_buffers_back(heap: &mut Heap, targets: &[(u64, usize)], bufs: &[Vec<u8>]) {
    for &(addr, buf_idx) in targets {
        let Some(buf) = bufs.get(buf_idx) else {
            continue;
        };
        heap.update_array_bytes(addr, buf);
    }
}

pub(super) fn copy_array_buffers_back(heap: &mut Heap, targets: &[(u64, usize)], bufs: &[Vec<i64>]) {
    for &(addr, buf_idx) in targets {
        let Some(buf) = bufs.get(buf_idx) else {
            continue;
        };
        heap.update_array_elements(addr, buf);
    }
}

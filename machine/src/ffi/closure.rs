//! libffi closures for C → coil callbacks.

use std::ffi::c_void;

use common::Value;
use libffi::low;
use libffi::middle::{Cif, ClosureOnce};

use crate::memory::FfiType;

use super::signature::FfiError;

/// Keeps a libffi closure alive for the lifetime of a loaded library.
pub struct OwnedClosure {
    inner: ClosureOnce,
}

impl OwnedClosure {
    pub fn code_ptr_usize(&self) -> usize {
        (*self.inner.code_ptr()) as *const () as usize
    }
}

/// Type-erased reentrant call into the running program (set per closure).
pub type ProgramCallFn = unsafe fn(*mut c_void, u32, *const Value, usize) -> Value;

/// runtime state stashed in callback userdata (single-threaded).
pub struct CallbackState {
    pub program: *mut c_void,
    pub fn_offset: u32,
    pub call_fn: ProgramCallFn,
}

/// Trampoline for `extern "C" fn(int64) -> int64` callbacks into coil.
unsafe extern "C" fn int_to_int_trampoline(
    _cif: &low::ffi_cif,
    result: &mut i64,
    args: *const *const c_void,
    userdata: &mut Option<CallbackState>,
) {
    // Edition 2024: bodies of `unsafe fn`/`unsafe extern` are safe by default.
    unsafe {
        let Some(state) = userdata.as_mut() else {
            return;
        };
        let arg_ptr = *args;
        let arg = *(arg_ptr as *const i64);
        let args = [Value::from(arg)];
        let out = (state.call_fn)(state.program, state.fn_offset, args.as_ptr(), args.len());
        *result = out.as_int();
    }
}

/// Build a libffi CIF for a callback signature (same as a normal function CIF).
pub fn callback_cif(
    args: &[FfiType],
    ret: FfiType,
    layouts: &[crate::memory::CStructLayout],
) -> Result<Cif, FfiError> {
    super::call::prepare_cif(
        &super::FfiSignature {
            name: String::new(),
            args: args.to_vec(),
            ret,
            variadic: false,
        },
        layouts,
    )
    .map(|p| p.cif)
}

/// Create an owned closure for a coil `fn(int) -> int` at `fn_offset`.
pub fn make_int_callback(
    program: *mut c_void,
    fn_offset: u32,
    call_fn: ProgramCallFn,
    cif: Cif,
) -> Result<OwnedClosure, FfiError> {
    let state = CallbackState {
        program,
        fn_offset,
        call_fn,
    };
    let inner = ClosureOnce::new(cif, int_to_int_trampoline, state);
    Ok(OwnedClosure { inner })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::FfiType;
    use common::Value;
    use std::ffi::c_void;

    #[test]
    fn make_int_callback_invokes_the_trampoline() {
        unsafe fn fake_call(
            _program: *mut c_void,
            _offset: u32,
            args_ptr: *const Value,
            len: usize,
        ) -> Value {
            unsafe {
                let args = std::slice::from_raw_parts(args_ptr, len);
                Value::from(args[0].as_int() * 2)
            }
        }
        let cif = callback_cif(&[FfiType::Int], FfiType::Int, &[]).unwrap();
        let closure = make_int_callback(std::ptr::null_mut(), 0, fake_call, cif).expect("closure");
        type Cb = unsafe extern "C" fn(i64) -> i64;
        let cb: Cb = unsafe { std::mem::transmute(closure.code_ptr_usize()) };
        assert_eq!(unsafe { cb(21) }, 42);
    }
}

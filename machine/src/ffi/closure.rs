//! libffi closures for C → coil callbacks (the runtime's calls; native
//! code's direct calls have a thunk of their own per callback).

use std::ffi::c_void;

use libffi::low;
use libffi::middle::ClosureOnce;

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

/// Type-erased reentrant call into the running program: the body at the
/// offset, with the callback signature (a `tag::CALLBACK` aux, see
/// [`common::decode_callback_sig`]), on the C arguments as raw words (an
/// `int` or `ptr` as its bits, a `double`'s bits, a `bool` as 0 or 1, a C
/// string's pointer). Its result word is the C result the same way.
pub type ProgramCallFn = unsafe fn(*mut c_void, u32, u32, *const u64, usize) -> u64;

/// What a callback's userdata holds.
pub struct CallbackState {
    pub program: *mut c_void,
    pub fn_offset: u32,
    pub aux: u32,
    pub call_fn: ProgramCallFn,
    params: Vec<FfiType>,
}

/// Every callback's trampoline: the C arguments as words, the program's
/// call, and its result word in libffi's result slot (a `double`'s bits, an
/// integer widened to a whole `ffi_arg`, as libffi reads them).
unsafe extern "C" fn trampoline(_cif: &low::ffi_cif, result: &mut u64, args: *const *const c_void, userdata: &mut Option<CallbackState>) {
    // SAFETY: libffi passes one pointer per parameter of the CIF.
    unsafe {
        let Some(state) = userdata.as_mut() else {
            return;
        };
        let mut words = [0u64; common::CALLBACK_MAX_PARAMS];
        for (i, ty) in state.params.iter().enumerate() {
            let p = *args.add(i);
            words[i] = match ty {
                FfiType::Float => (*(p as *const f64)).to_bits(),
                FfiType::Bool => u64::from(*(p as *const u8) != 0),
                FfiType::String | FfiType::Ptr => *(p as *const usize) as u64,
                _ => *(p as *const i64) as u64,
            };
        }
        *result = (state.call_fn)(state.program, state.fn_offset, state.aux, words.as_ptr(), state.params.len());
    }
}

/// An owned closure that calls the coil function at `fn_offset` with
/// callback signature `aux`.
pub fn make_callback(program: *mut c_void, fn_offset: u32, aux: u32, call_fn: ProgramCallFn) -> Result<OwnedClosure, FfiError> {
    let (params, ret) = common::decode_callback_sig(aux).ok_or_else(|| FfiError::Unsupported(format!("callback signature {aux}")))?;
    let tys = |tags: &[u32]| tags.iter().map(|&t| FfiType::from_tag(t, 0)).collect::<Vec<_>>();
    let params = tys(&params);
    let ret = FfiType::from_tag(ret, 0);
    let cif = super::call::prepare_cif(
        &super::FfiSignature {
            name: String::new(),
            args: params.clone(),
            ret,
            variadic: false,
        },
        &[],
    )?
    .cif;
    let state = CallbackState { program, fn_offset, aux, call_fn, params };
    Ok(OwnedClosure { inner: ClosureOnce::new(cif, trampoline, state) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::tag;
    use std::ffi::c_void;

    unsafe fn fake_call(_program: *mut c_void, _offset: u32, aux: u32, words: *const u64, len: usize) -> u64 {
        let words = unsafe { std::slice::from_raw_parts(words, len) };
        match common::decode_callback_sig(aux).unwrap() {
            (_, tag::FLOAT) => (f64::from_bits(words[0]) * 1.5 + words[1] as f64).to_bits(),
            (_, tag::BOOL) => u64::from(words[0] == 1),
            _ => words.iter().sum::<u64>() * 2,
        }
    }

    #[test]
    fn a_callback_takes_and_returns_its_signature_s_types() {
        let closure = make_callback(std::ptr::null_mut(), 0, 0, fake_call).expect("closure");
        let cb: unsafe extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(closure.code_ptr_usize()) };
        assert_eq!(unsafe { cb(21) }, 42);

        let aux = common::encode_callback_sig(&[tag::FLOAT, tag::INT], tag::FLOAT).unwrap();
        let closure = make_callback(std::ptr::null_mut(), 0, aux, fake_call).expect("closure");
        let cb: unsafe extern "C" fn(f64, i64) -> f64 = unsafe { std::mem::transmute(closure.code_ptr_usize()) };
        assert_eq!(unsafe { cb(2.0, 4) }, 7.0);

        let aux = common::encode_callback_sig(&[tag::BOOL], tag::BOOL).unwrap();
        let closure = make_callback(std::ptr::null_mut(), 0, aux, fake_call).expect("closure");
        let cb: unsafe extern "C" fn(bool) -> bool = unsafe { std::mem::transmute(closure.code_ptr_usize()) };
        assert!(unsafe { cb(true) });
        assert!(!unsafe { cb(false) });
    }
}

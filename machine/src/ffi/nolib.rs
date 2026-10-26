//! Stand-ins for libffi and libloading on targets without dynamic libraries
//! (wasm). Declaring an `extern` signature still works; loading a library or
//! calling through FFI fails with [`FfiError::Unsupported`].

use std::ffi::c_void;
use std::fmt;

use common::Value;

use crate::memory::{CStructLayout, FfiType, Heap};

use super::signature::{FfiError, FfiSignature};

const NO_FFI: &str = "FFI is not available on this platform (no dynamic libraries)";

fn unsupported() -> FfiError {
    FfiError::Unsupported(NO_FFI.into())
}

/// Error from [`Library::new`]: never opens anything.
#[derive(Debug)]
pub struct NoDynamicLibraries;

impl fmt::Display for NoDynamicLibraries {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(NO_FFI)
    }
}

/// A shared library handle that can never be created.
#[derive(Debug)]
pub struct Library(());

impl Library {
    /// # Safety
    /// Always fails; `unsafe` only to match `libloading::Library::new`.
    pub unsafe fn new<P>(_path: P) -> Result<Self, NoDynamicLibraries> {
        Err(NoDynamicLibraries)
    }
}

/// Placeholder for a libffi CIF.
pub struct Cif(());

pub struct PreparedCall {
    pub cif: Cif,
}

pub struct InvokeContext;

impl InvokeContext {
    pub fn new(_heap: *mut Heap, _struct_layouts: &[CStructLayout]) -> Self {
        Self
    }

    pub fn lending_arrays(self, _lend: bool) -> Self {
        self
    }
}

pub fn prepare_cif(_sig: &FfiSignature, _layouts: &[CStructLayout]) -> Result<PreparedCall, FfiError> {
    Err(unsupported())
}

pub fn prepare_variadic_cif(
    _arg_types: &[FfiType],
    _nfixed: usize,
    _ret: FfiType,
    _layouts: &[CStructLayout],
) -> Result<Cif, FfiError> {
    Err(unsupported())
}

/// Default C argument promotions for the `...` region.
pub fn promote_variadic_arg_type(ty: FfiType) -> FfiType {
    match ty {
        FfiType::Bool | FfiType::Int8 | FfiType::Int16 | FfiType::UInt8 | FfiType::UInt16 => {
            FfiType::Int
        }
        other => other,
    }
}

pub fn prepare_cif_for_symbol(
    _sig: &FfiSignature,
    _library: &Library,
    _symbol: &str,
    _layouts: &[CStructLayout],
) -> Result<PreparedCall, FfiError> {
    Err(unsupported())
}

pub fn resolve_symbol(_library: &Library, _symbol: &str) -> Result<usize, FfiError> {
    Err(unsupported())
}

pub fn invoke_via_libffi(
    _prepared: &PreparedCall,
    _sig: &FfiSignature,
    _args: &[Value],
    _variadic_arg_types: Option<&[FfiType]>,
    _ctx: &mut InvokeContext,
    _callback_closures: &mut Vec<*mut c_void>,
) -> Result<Option<Value>, FfiError> {
    Err(unsupported())
}

pub struct OwnedClosure(());

impl OwnedClosure {
    pub fn code_ptr_usize(&self) -> usize {
        0
    }
}

/// Type-erased reentrant call into a `Machine<S>` (set per closure).
pub type VmCallFn = unsafe fn(*mut c_void, u32, *const Value, usize) -> Value;

pub fn callback_cif(
    _args: &[FfiType],
    _ret: FfiType,
    _layouts: &[CStructLayout],
) -> Result<Cif, FfiError> {
    Err(unsupported())
}

pub fn make_int_callback(
    _vm: *mut c_void,
    _fn_offset: u32,
    _call_fn: VmCallFn,
    _cif: Cif,
) -> Result<OwnedClosure, FfiError> {
    Err(unsupported())
}

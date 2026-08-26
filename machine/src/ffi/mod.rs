//! FFI: explicit signatures and libffi dispatch (no runtime guessing).

mod call;
mod closure;
mod error;
mod registry;
mod resolve;
mod runtime;
mod signature;

pub use call::{
    InvokeContext, PreparedCall, invoke_via_libffi, prepare_cif, prepare_cif_for_symbol,
    prepare_variadic_cif, promote_variadic_arg_type, resolve_symbol,
};
pub use closure::{OwnedClosure, VmCallFn, callback_cif, make_int_callback};
pub use error::{FfiErrorKindTag, alloc_ffi_error, alloc_ffi_error_kind, alloc_result_ffi_err};
pub use libloading::Library;
pub use registry::{HostClosureFn, NativeFn, Natives};
pub use resolve::{library_candidates, platform_shared_lib_filename, resolve_library};
pub use runtime::{check_native_libraries, packaged_app_ffi_startup_check, probe_system_libffi};
pub use signature::{FfiError, FfiSignature, FfiSignatureBuilder};

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub fn register_on_library(
    obj_lib: &mut crate::memory::ObjLibrary,
    sig: FfiSignature,
    layouts: &[crate::memory::CStructLayout],
) -> Result<usize, FfiError> {
    let prepared = prepare_cif_for_symbol(&sig, &obj_lib.library, &sig.name, layouts)?;
    let id = obj_lib.signatures.len();
    let name = sig.name.clone();
    obj_lib.signatures.push(crate::memory::RegisteredFunction {
        sig: crate::memory::FunctionSig::from_ffi_signature(&sig),
        prepared,
    });
    obj_lib.by_name.insert(name, id);
    Ok(id)
}

/// Load a shared library by path (legacy — prefer [`resolve_library`]).
pub fn load_library(name: &str) -> Result<Arc<Library>, libloading::Error> {
    let lib = unsafe { Library::new(name) }?;
    Ok(Arc::new(lib))
}

/// Load with search path resolution.
pub fn load_library_resolved(
    name: &str,
    base_dir: Option<&Path>,
    search_paths: &[PathBuf],
) -> Result<Arc<Library>, FfiError> {
    resolve_library(name, base_dir, search_paths)
}

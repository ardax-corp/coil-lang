//! FFI: explicit signatures and libffi dispatch (no runtime guessing).

mod call;
mod closure;
mod error;
mod gate;
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
pub use gate::DloadGate;
pub use libloading::Library;
pub use registry::{HostClosureFn, HostOp, NativeFn, Natives};
pub use resolve::{
    DLOAD_PRODUCTION_STEMS, dload_request_stem, is_libc_alias, is_production_dload_stem,
    library_candidates, platform_shared_lib_filename, resolve_library,
};
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
pub fn load_library(name: &str) -> Result<Arc<Library>, FfiError> {
    resolve_library(name, None, &[], &DloadGate::deny_all())
}

/// Load with search path resolution.
pub fn load_library_resolved(
    name: &str,
    base_dir: Option<&Path>,
    search_paths: &[PathBuf],
    gate: &DloadGate,
) -> Result<Arc<Library>, FfiError> {
    resolve_library(name, base_dir, search_paths, gate)
}

/// Build `examples/sum.c` into the platform `libsum` filename if missing or stale.
/// Used by machine FFI tests so they do not depend on compiler integration tests
/// compiling the fixture as a side effect.
#[cfg(test)]
pub(crate) fn ensure_examples_libsum() -> PathBuf {
    let lib_name = platform_shared_lib_filename("sum");
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("machine crate must have a parent (workspace root)");
    let sum_c = workspace_root.join("examples/sum.c");
    let libsum = workspace_root.join("examples").join(&lib_name);

    let needs_build = match (sum_c.metadata(), libsum.metadata()) {
        (Ok(src_meta), Ok(so_meta)) => src_meta.modified().ok() > so_meta.modified().ok(),
        (Ok(_), Err(_)) => true,
        _ => false,
    };
    if !needs_build && libsum.exists() {
        return libsum;
    }
    if !sum_c.exists() {
        return libsum;
    }

    let tmp = libsum.with_file_name(format!(
        ".{}.{}.tmp",
        lib_name,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));

    let mut cmd = std::process::Command::new("cc");
    #[cfg(target_os = "macos")]
    {
        cmd.arg("-dynamiclib");
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        cmd.arg("-shared").arg("-fPIC");
    }
    #[cfg(target_os = "windows")]
    {
        cmd.arg("-shared");
    }
    let status = cmd.arg("-O2").arg("-o").arg(&tmp).arg(&sum_c).status();
    match status {
        Ok(s) if s.success() => {
            if std::fs::rename(&tmp, &libsum).is_err() {
                if !libsum.exists() {
                    let _ = std::fs::copy(&tmp, &libsum);
                }
                let _ = std::fs::remove_file(&tmp);
            }
        }
        Ok(s) => {
            let _ = std::fs::remove_file(&tmp);
            if std::env::var_os("CI").is_some() {
                panic!(
                    "FFI soft-skip forbidden in CI: cc returned non-zero status {} building {lib_name}",
                    s.code().unwrap_or(-1)
                );
            }
            eprintln!(
                "skipping: cc returned non-zero status {} building {lib_name}",
                s.code().unwrap_or(-1)
            );
        }
        Err(e) => {
            if std::env::var_os("CI").is_some() {
                panic!("FFI soft-skip forbidden in CI: failed to invoke cc: {e}");
            }
            eprintln!("skipping: failed to invoke cc: {e}");
        }
    }
    libsum
}

/// Compile `examples/libsum` if needed; `None` means skip (never in CI).
#[cfg(test)]
pub(crate) fn require_examples_libsum() -> Option<(String, PathBuf)> {
    let lib_name = platform_shared_lib_filename("sum");
    let lib_path = ensure_examples_libsum();
    if lib_path.exists() {
        Some((lib_name, lib_path))
    } else {
        if std::env::var_os("CI").is_some() {
            panic!("FFI soft-skip forbidden in CI: {lib_name} not built");
        }
        eprintln!("skipping: {lib_name} not built");
        None
    }
}

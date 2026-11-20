//! `extern` libraries linked into an AOT executable (`coil package
//! --link-static`): the object `coil_native::aot` made records each linked
//! function's address (`coil_static_syms`), the runtime installs the table
//! at start, and loading one of those libraries resolves its functions
//! from the table instead of opening a shared library.

use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_void};
use std::sync::OnceLock;

use super::resolve::dload_request_stem;

/// One statically linked `extern` function, as `coil_static_syms` lays it
/// out: its library's stem, its C symbol (NUL-terminated) and address.
#[repr(C)]
pub struct StaticSymbol {
    pub library: *const c_char,
    pub symbol: *const c_char,
    pub address: *const c_void,
}

/// The linked functions by library stem, then symbol.
pub type StaticLibrary = HashMap<String, usize>;

static STATIC_LIBRARIES: OnceLock<HashMap<String, StaticLibrary>> = OnceLock::new();

/// Install the `n` records at `table` (once per process; later calls do
/// nothing).
///
/// # Safety
/// `table` points at `n` records whose strings are NUL-terminated and
/// live for the rest of the process.
pub unsafe fn install_static_symbols(table: *const StaticSymbol, n: usize) {
    let mut libs: HashMap<String, StaticLibrary> = HashMap::new();
    for i in 0..n {
        // SAFETY: the caller's contract.
        let rec = unsafe { &*table.add(i) };
        let text = |p: *const c_char| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
        libs.entry(text(rec.library)).or_default().insert(text(rec.symbol), rec.address as usize);
    }
    let _ = STATIC_LIBRARIES.set(libs);
}

/// The linked functions of the library `name` (`extern "NAME"`, by its
/// stem), when the executable links it.
pub fn static_library(name: &str) -> Option<&'static StaticLibrary> {
    STATIC_LIBRARIES.get()?.get(&dload_request_stem(name))
}

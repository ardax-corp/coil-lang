// @expected E0409 — FFI `system` requires `--allow-ffi-exec`.
use ffi::declare;
use ffi::types::Int;
use ffi::types::Ptr;

fn main() {
    let _ = declare(0, "system", (Ptr,), Int);
}

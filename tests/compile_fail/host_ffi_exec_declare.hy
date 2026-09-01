// Expected: compile failure — FFI `system` requires `--allow-ffi-exec`.
use ffi::{declare};
use ffi::types::{Int, Ptr};

fn main() {
    let _ = declare(0, "system", (Ptr,), Int);
}

// Expected: compile failure — non-const `dload` path is a compile error.
use ffi::{dload};

fn main() {
    let name = "plugin";
    let _ = dload(name);
}

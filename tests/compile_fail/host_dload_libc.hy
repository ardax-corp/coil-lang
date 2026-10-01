// Expected: E0410 — `dload("c")` is always denied.
use ffi::{dload};

fn main() {
    let _ = dload("c");
}

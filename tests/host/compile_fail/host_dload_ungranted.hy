// Expected: E0410 — `dload` stem not on `--allow-dload`.
use ffi::{dload};

fn main() {
    let _ = dload("notalist");
}

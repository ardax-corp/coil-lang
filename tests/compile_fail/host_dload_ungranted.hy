// Expected: compile failure — `dload` stem not on `--allow-dload`.
use ffi::{dload};

fn main() {
    let _ = dload("notalist");
}

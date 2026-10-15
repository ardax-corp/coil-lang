// Expected: E0119 — macro arguments are plain expressions.
use fn_macros::{square};

fn main() {
    let x = square!(e: 1);
}

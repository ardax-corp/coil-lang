// Expected: compile failure — macro arguments are plain expressions.
use fn_macros::{square};

fn main() {
    let x = square!(e: 1);
}

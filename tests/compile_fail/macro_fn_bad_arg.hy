// Expected: compile failure — each argument must parse as an expression.
use fn_macros::{square};

fn main() {
    let x = square!(1 +);
}

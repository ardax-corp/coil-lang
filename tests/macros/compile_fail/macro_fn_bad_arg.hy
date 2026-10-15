// Expected: E0001 — each argument must parse as an expression.
use fn_macros::{square};

fn main() {
    let x = square!(1 +);
}

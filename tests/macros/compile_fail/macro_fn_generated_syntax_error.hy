// Expected: E0119 — the output is not an expression.
use fn_macros_bad::{broken};

fn main() {
    let x = broken!(1);
}

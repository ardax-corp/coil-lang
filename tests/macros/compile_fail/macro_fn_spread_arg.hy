// Expected: E0119 — a `...` spread is not a macro argument.
use fn_macros::sum;

fn main() {
    let xs = [1, 2];
    let x = sum!(...xs);
}

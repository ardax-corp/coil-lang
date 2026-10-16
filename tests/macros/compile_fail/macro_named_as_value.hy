// Expected: E0100 — macros have their own namespace, so naming an imported
// function-style macro as a plain value is "cannot find value" (#1203).
use fn_macros::square;

fn main() {
    let f = square;
}

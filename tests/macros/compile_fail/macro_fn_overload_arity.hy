// @expected E0119 — `ensure!` takes 1 or 2 arguments.
use fn_macros::ensure;

fn main() {
    let r = ensure!(true, "a", "b");
}

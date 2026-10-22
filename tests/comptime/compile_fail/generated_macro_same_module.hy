// @expected E0119 — a macro a `comptime` block generates is used from other
// modules, not from the module that generated it.
use comptime_gen::scale_macro;
use macro::{Code, Expr};

comptime {
    return scale_macro("triple", 3);
}

fn main() {
    let x = triple!(2);
}

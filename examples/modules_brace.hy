// examples/modules_brace.hy — brace-group imports from a module file.
//
// `examples/src/calc.hy` exports `add` and `mul`. Brace syntax imports
// both in one statement; the resolver falls back to the module file
// (`calc.hy`) because there is no `calc/add.hy` / `calc/mul.hy`.
//
// @output 1242

use calc::{add, mul};
use io::sync::print;
use string::format;

fn main() {
    let _ = print(format("%i", add(5, 7)));
    let _ = print(format("%i", mul(6, 7)));
}

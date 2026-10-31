// @expected E0121 — two `twice` macros take one argument each.
use macro::{Expr, Code};

macro twice(Expr e) -> Code {
    return quote expr { ${e} + ${e} };
}

macro twice(Expr x) -> Code {
    return quote expr { 2 * ${x} };
}

fn main() {}

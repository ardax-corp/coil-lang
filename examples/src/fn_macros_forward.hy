// Function-style macros whose expression output is a single macro call, used
// by `tests/macros/function_macro_forward.hy` (#1404).
use macro::{Expr, Code, raw};

/// `cube!(e)`: `e * e * e`.
macro cube(Expr e) -> Code {
    return quote expr { ${e} * ${e} * ${e} };
}

/// `to_cube!(e)`: exactly `cube!(e)`, this provider's own macro.
macro to_cube(Expr e) -> Code {
    return quote expr { cube!(${e}) };
}

/// `to_to_cube!(e)`: `to_cube!(e)`, two forwards before any code.
macro to_to_cube(Expr e) -> Code {
    return quote expr { to_cube!(${e}) };
}

/// `to_cube_raw!(e)`: `cube!(e)` as raw text.
macro to_cube_raw(Expr e) -> Code {
    return raw("cube!(" + e.src() + ")");
}

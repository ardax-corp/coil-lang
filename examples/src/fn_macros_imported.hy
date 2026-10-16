// Function-style macros whose output calls macros this provider imports from
// `fn_macros`, used by `tests/macros/function_macro_provider_imports.hy` (#1407).
use macro::{Expr, Code, raw};
use fn_macros::{square, sum as add_all};

/// `square_plus_one!(e)`: `square!(e) + 1`, `square` imported by this provider.
macro square_plus_one(Expr e) -> Code {
    return quote expr { square!(${e}) + 1 };
}

/// `to_square!(e)`: exactly `square!(e)`, forwarded to the imported macro.
macro to_square(Expr e) -> Code {
    return quote expr { square!(${e}) };
}

/// `sum_squares!(a, b)`: `add_all!(square!(a), square!(b))`, an imported
/// macro under its alias.
macro sum_squares(Expr a, Expr b) -> Code {
    return raw("add_all!(square!(" + a.src() + "), square!(" + b.src() + "))");
}

/// `to_missing!(e)`: `missing!(e)`, which neither this provider nor anything
/// it imports declares (`tests/macros/compile_fail/macro_fn_output_unknown.hy`).
macro to_missing(Expr e) -> Code {
    return quote expr { missing!(${e}) };
}

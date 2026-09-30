// Function-style macros that go wrong, for `tests/compile_fail/macro_fn_*.hy`.
use macro::{Expr, Code, raw};

/// Output that is not an expression.
macro broken(Expr e) -> Code {
    return raw(e.src() + " +");
}

/// Output with a type error.
macro mistyped(Expr e) -> Code {
    return quote stmts {
        let n: int = ${e};
    };
}

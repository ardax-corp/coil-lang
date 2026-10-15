// Expected: E0121 — `all(a, b)` overlaps `all(a, ...rest)`.
use macro::{Expr, Code};

macro all(Expr a, Vec<Expr> rest) -> Code {
    return quote expr { ${a} };
}

macro all(Expr a, Expr b) -> Code {
    return quote expr { ${a} && ${b} };
}

fn main() {}

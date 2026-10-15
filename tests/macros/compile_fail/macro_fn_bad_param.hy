// Expected: E0119 — a function-style macro takes `Expr` parameters.
use macro::{Expr, Code};

macro takes_int(int n) -> Code {
    return quote expr { 1 };
}

fn main() {}

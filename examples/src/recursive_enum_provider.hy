// Provider for tests/modules/imported_recursive_enum.hy (#2039): an enum whose
// payloads name the enum itself, walked by a recursive function.
enum Expr {
    Num(int),
    Add(Expr, Expr),
    Neg { inner: Expr },
}

fn eval(Expr e) -> int {
    return match e {
        Expr::Num(n) => n,
        Expr::Add(a, b) => eval(a) + eval(b),
        Expr::Neg { inner } => 0 - eval(inner),
    };
}

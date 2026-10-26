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

// A generic payload naming its own enum, and a payload naming an enum declared
// further down.
enum Chain<T> {
    End,
    Link(T, Chain<T>),
}

fn chain_len(Chain<int> c) -> int {
    return match c {
        Chain::End => 0,
        Chain::Link(_, rest) => 1 + chain_len(rest),
    };
}

enum Outer {
    Leaf,
    Via(Inner),
}

enum Inner {
    Back(Outer),
}

fn depth(Outer o) -> int {
    return match o {
        Outer::Leaf => 0,
        Outer::Via(Inner::Back(next)) => 1 + depth(next),
    };
}

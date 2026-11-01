// @expected E0200 — `INT` names the built-in `int`; a class cannot take the
// name (#1673).
class INT {
    pub n: int,
}

fn get(INT x) -> int {
    return x.n;
}

fn main() {
    let _ = get(new INT(3));
}

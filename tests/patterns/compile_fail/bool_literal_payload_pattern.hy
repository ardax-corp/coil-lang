// @expected E0001 — a bool literal inside a payload pattern is rejected too.
fn f(Option<bool> o) -> int {
    return match o {
        Option::Some(false) => 1,
        default => 0,
    };
}

fn main() {
    let r = f(Option::Some(true));
}

// @expected E0200 — `Option` is the prelude enum; a class cannot take the
// name while the prelude binds it, as an enum cannot (#1654).
class Option {
    pub n: int,
}

fn get(Option o) -> int {
    return o.n;
}

fn main() {
    let _ = get(new Option(3));
}

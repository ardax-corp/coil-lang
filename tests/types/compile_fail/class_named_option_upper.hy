// @expected E0200 — built-in type names match case-insensitively, so
// `OPTION` names the prelude `Option`; a class cannot take it (#1672).
class OPTION {
    pub n: int,
}

fn get(OPTION o) -> int {
    return o.n;
}

fn main() {
    let _ = get(new OPTION(3));
}

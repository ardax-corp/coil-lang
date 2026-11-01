// @expected E0200 — built-in type names match in any letter case, so
// `ERRORKIND` names the built-in error enum `ErrorKind` (#1701).
class ERRORKIND {
    pub n: int,
}

fn get(ERRORKIND k) -> int {
    return k.n;
}

fn main() {
    let _ = get(new ERRORKIND(3));
}

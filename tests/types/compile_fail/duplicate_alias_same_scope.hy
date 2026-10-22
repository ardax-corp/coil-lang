// @expected E0119 — a type alias name may be declared once per scope.
type Id = int;

type Id = string;

fn main() {
    let x: Id = 1;
}

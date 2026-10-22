// @expected E0001 — `case` with only a wildcard arm is not a `match` (COI-74).
fn main() {
    case x { _ => 0 };
}

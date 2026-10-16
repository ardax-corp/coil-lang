// Expected: E0202 — `qpath_provider` is compiled (through `double`), but its
// enum `Shape` was not imported, so the bare `Shape::Empty` is unknown.
use qpath_provider::double;

fn main() {
    let v = Shape::Empty;
}

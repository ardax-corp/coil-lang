// @expected E0119 — `for` over a tuple needs every element to share one type.
fn main() {
    for x in (1, "a") {}
}

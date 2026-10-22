// @expected E0102 — every `yield` in a `gen fn` has the same type.
gen fn mixed() {
    yield 1;
    yield "two";
}

fn main() {
    let h = mixed();
    let _ = resume h;
}

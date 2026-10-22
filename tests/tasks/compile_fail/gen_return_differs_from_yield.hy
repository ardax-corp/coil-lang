// @expected E0102 — a `gen fn`'s `return` value has the type of its yields.
gen fn counter() {
    yield 1;
    return "done";
}

fn main() {
    let h = counter();
    let _ = resume h;
}

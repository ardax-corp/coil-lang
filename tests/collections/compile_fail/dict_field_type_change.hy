// @expected E0102 — a dict field keeps its type; an `int` field takes no `string`.
fn main() {
    let d = { foo: 42 };
    d.foo = "x";
}

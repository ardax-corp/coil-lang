// @expected E0104 — strings are immutable values with no `push`; build with `+`.
fn main() {
    let s = "abc";
    s.push("d");
}

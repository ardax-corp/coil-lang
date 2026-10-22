// @expected E0115 — `??` needs an Option or a Result on the left.
fn main() {
    let r = "a" ?? "b";
}

// @expected E0132 — a closure capturing the local reads it when it is made.
fn main() {
    let s: string;
    let f = fn () use (s) => s;
    s = "late";
}

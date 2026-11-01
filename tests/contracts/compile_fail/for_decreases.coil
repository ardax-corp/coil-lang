// @expected E0001 — `decreases` is for `while` loops only; a `for` loop ends with its items.
fn main() {
    let s = 0;
    for k in 0..3 decreases 3 - s {
        s = s + 1;
    }
}

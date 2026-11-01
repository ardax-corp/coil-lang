// @expected E0146 — `break` in a `defer` body cannot leave the loop around
// the `defer`.
fn main() {
    let i = 0;
    while i < 3 {
        i = i + 1;
        defer {
            break;
        }
    }
}

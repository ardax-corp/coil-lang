// @expected E0801 — `break` must be inside a loop of its own function; a
// lambda body inside a loop is a separate function.
fn main() {
    let i = 0;
    while i < 3 {
        let f = fn () {
            break;
        };
        i = i + 1;
    }
}

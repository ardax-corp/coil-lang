// @expected E0111 — a `while true` with a `break` can end, so the code
// after it still needs a `return`.
fn f() -> int {
    while true {
        break;
    }
}

fn main() {
    let _ = f;
}

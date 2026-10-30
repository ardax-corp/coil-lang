// @expected E0146 — a `defer` body cannot `return`: it runs while its
// function is already leaving.
fn work() -> int {
    defer {
        return 2;
    }
    return 1;
}

fn main() {
    let _ = work();
}

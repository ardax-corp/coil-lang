// @expected E0146 — a `defer` body cannot `raise`: its function has already
// chosen what it returns.
fn work() -> Result<int, string> {
    defer {
        raise "late";
    }
    return 1;
}

fn main() {
    let _ = work();
}

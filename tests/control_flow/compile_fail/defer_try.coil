// @expected E0146 — `?` inside a `defer` body would return the error from a
// function that is already leaving.
fn fails() -> Result<int, string> {
    raise "no";
}

fn work() -> Result<int, string> {
    defer {
        let _ = fails()?;
    }
    return 1;
}

fn main() {
    let _ = work();
}

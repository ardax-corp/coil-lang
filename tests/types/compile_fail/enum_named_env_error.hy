// Expected: E0200 — `EnvError` is a built-in error enum with a built-in
// `Show` instance; an enum cannot take the name (#1701).
enum EnvError {
    Missing,
}

fn main() {
    let _ = show(EnvError::Missing);
}

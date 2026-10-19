// Expected: E0119 — logical `!` takes a `bool` or an `int`; a type parameter
// `T` may be neither, and the help names it `T` (coil-lang#1447).
fn negate<T>(T x) -> bool {
    return !x;
}

fn main() {
    negate(true);
}

// Expected: E0119 — `%` on a type parameter without a `Rem` bound (#817).
fn f<T: Add>(T a, T b) -> T {
    return a % b;
}

fn main() {
    let x = f(7, 3);
}

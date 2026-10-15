// Expected: E0119 — `[T]` is not a type in return position either; use `[T; N]` or `Vec<T>`.
fn f() -> [int] {
    return [1];
}

fn main() {
}

// Expected: E0119
// #890: `[T]` is rejected in parameters too; use `Vec<T>` or `[T; N]`.

fn head([int] xs) -> int {
    return xs[0];
}

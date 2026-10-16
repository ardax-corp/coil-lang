// Expected: E0119 — the generic enum `Box` takes one type argument, not two.
enum Box<T> {
    Box(T),
}

fn f(Box<int, string> x) -> int {
    return 0;
}

fn main() {}

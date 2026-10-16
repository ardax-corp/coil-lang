// Expected: E0119 — the alias `Pair<T>` takes one type argument, not two.
type Pair<T> = (T, T);

fn f(Pair<int, string> p) -> int {
    return 0;
}

fn main() {}

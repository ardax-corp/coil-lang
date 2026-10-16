// Expected: E0102 — a `string -> int` value does not fit a `fn(int x) -> int` alias.
type IntFn = fn(int x) -> int;

fn apply(IntFn f) -> int {
    return f(1);
}

fn size(string s) -> int {
    return 1;
}

fn main() {
    let r = apply(size);
}

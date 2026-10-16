// Expected: E0102 — bare `Red` is a `Light` case, so it is a constructor
// pattern, not a binding, and cannot match an `Option<int>`.
enum Light {
    Red,
    Green,
}

fn f(Option<int> o) -> int {
    return match o {
        Red => 1,
        default => 0,
    };
}

fn main() {
    let r = f(Option::None);
}

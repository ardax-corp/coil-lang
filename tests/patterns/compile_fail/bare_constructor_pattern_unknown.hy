// Expected: E0211 — bare `Nope(v)` names no enum case in scope.
fn f(Option<int> o) -> int {
    return match o {
        Nope(v) => v,
        default => 0,
    };
}

fn main() {
    let r = f(Option::None);
}

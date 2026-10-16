// Expected: E0102 — one wrong-enum arm next to a default still fails.
enum A { X, Y }
enum B { P, Q }

fn f(A a) -> int {
    return match a {
        A::X => 0,
        B::P => 1,
        default => 2,
    };
}

fn main() {
    let r = f(A::X);
}

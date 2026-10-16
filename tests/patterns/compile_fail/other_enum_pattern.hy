// Expected: E0102 — a pattern from enum B against a scrutinee of enum A.
enum A { X, Y }
enum B { P, Q }

fn f(A a) -> int {
    return match a {
        B::P => 1,
        B::Q => 2,
    };
}

fn main() {
    let r = f(A::X);
}

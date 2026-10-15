// Expected: E0208 — duplicate field in match record pattern (parse E0208).
enum P {
    P { x: int, y: int },
}

fn f(P p) -> int {
    return match p {
        P::P { x, x } => x,
    };
}

fn main() {
    f(P::P { x: 1, y: 2 });
}

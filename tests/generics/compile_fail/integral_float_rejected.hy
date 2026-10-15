// Expected: E0119 — `float` is `Num` but not `Integral` (#816).
fn f<T: Integral>(T a) -> T {
    return a;
}

fn main() {
    let x = f(1.5);
}

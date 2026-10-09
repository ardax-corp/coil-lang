// Expected: E0119 — unary `-` on a type parameter without a `Neg` bound (#803).
fn f<T: Sub>(T a) -> T {
    return -a;
}

fn main() {
    let x = f(1);
}

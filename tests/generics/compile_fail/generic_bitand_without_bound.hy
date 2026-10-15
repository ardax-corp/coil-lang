// Expected: E0119 — `&` on a type parameter without a `BitAnd` bound; `Num` does not imply it (#821).
fn f<T: Num>(T a, T b) -> T {
    return a & b;
}

fn main() {
    let x = f(1, 2);
}

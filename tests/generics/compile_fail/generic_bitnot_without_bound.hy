// Expected: E0119 — unary `~` on a type parameter without a `BitNot` bound (#824).
fn f<T: Num>(T a) -> T {
    return ~a;
}

fn main() {
    let x = f(1);
}

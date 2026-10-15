// Expected: E0119 — `...x` spreads only tuples, fixed arrays and `Vec`s.
fn f(int a) -> int {
    return a;
}

fn main() {
    let x = f(...5);
}

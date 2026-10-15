// Expected: E0209 — an `Option` match without a `Some` arm is not exhaustive.
fn main() {
    let x: Option<int> = Option::None();
    let r = match x {
        Option::None => 0,
    };
}

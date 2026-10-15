// Expected: E0119 — reading a field the dict's type does not have.
fn main() {
    let d = { foo: 42 };
    let x = d.bar;
}

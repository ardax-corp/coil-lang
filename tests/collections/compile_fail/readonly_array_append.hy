// Expected: E0001 — `xs[] = x` append is not Coil syntax; append with `Vec::push`.
fn main() {
    let xs = readonly [1, 2, 3];
    xs[] = 4;
}

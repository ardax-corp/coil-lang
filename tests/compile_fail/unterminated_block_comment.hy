// Expected: parse failure — a block comment must be closed with `*/`.
fn main() {
    /* opened /* nested */ but never closed
    return;
}

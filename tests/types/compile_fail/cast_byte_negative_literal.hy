// Expected: E0102
// A negative literal cast to byte is still rejected after #845.

fn main() {
    let b = -1 as byte;
}

// Expected: E0102 — only unary traits are existential value types; a value is not a
// `Convert` (a two-parameter trait).
trait Convert<A, B> {
    fn cast(A x) -> B;
}

impl Convert<int> for int {
    fn cast(int x) -> int {
        return x;
    }
}

fn take(Convert c) -> int {
    return 0;
}

fn main() {
    take(1);
}

// Expected: E0119 — `apply_cast` needs `Convert<int, B>`, and no instance exists.
trait Convert<A, B> {
    fn cast(A x) -> B {}
}

fn apply_cast<A, B>(A x) -> B where Convert<A, B> {
    return cast(x);
}

fn main() {
    let y = apply_cast(42);
}

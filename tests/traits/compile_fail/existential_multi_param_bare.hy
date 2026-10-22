// @expected E0119 — a two-parameter trait cannot be a bare (existential) parameter type.
trait Convert<A, B> {
    fn cast(A x) -> B {}
}

fn take(Convert x) {}

fn main() {}

// A `gen fn` declared in an `impl` returns a coroutine, as a free one (#778).
// HIR only: coil-lang#785 (the AST codegen does not carry this fix).
class Counter {
    n: int,
}

impl Counter {
    pub gen fn count() -> int {
        yield self.n;
        yield self.n + 1;
        return self.n + 2;
    }
}

test("gen fn method yields through its receiver") {
    let c = new Counter(5);
    let g = c.count();
    let a = resume g;
    let b = resume g;
    let d = resume g;
    assert(a == 5 && b == 6 && d == 7)?;
}

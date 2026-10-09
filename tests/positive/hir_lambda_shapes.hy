// Lambdas with block bodies, lambdas inside lambdas, captures relayed
// through an outer lambda, and a captured class local.
class Acc {
    pub n: int,
}

fn ap(int -> int -> int f) -> int {
    let g = f(10);
    return g(2);
}

fn run(int -> int f, int x) -> int {
    return f(x);
}

test("curried lambdas") {
    assert(ap(fn (int a) => fn (int b) use (a) => a * 3 + b) == 32)?;
    let k = 5;
    let r = ap(fn (int a) use (k) => fn (int b) use (a, k) => a + b + k);
    assert(r == 17)?;
}

test("a block body with locals and an early return") {
    let limit = 4;
    let f = fn (int x) use (limit) {
        let y = x * 2;
        if y > limit {
            return limit;
        }
        return y;
    };
    assert(run(f, 1) == 2)?;
    assert(run(f, 9) == 4)?;
}

test("a captured class local") {
    let acc = new Acc(3);
    let f = fn (int x) use (acc) => acc.n + x;
    assert(run(f, 4) == 7)?;
    acc.n = 10;
    assert(run(f, 4) == 14)?;
}

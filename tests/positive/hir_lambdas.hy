// Expression-bodied anonymous functions: no captures, a captured local
// and parameter, string results, and one passed to a higher-order fn.
const STEP = 2;

// A function ahead of a lambda, in a file with top-level statements.
fn step(int n) -> int {
    return n + 40;
}

fn stepped(int n) -> int {
    let f = fn (int x) => x * 3;
    return f(n) + STEP;
}

fn apply(int -> int f, int x) -> int {
    return f(x);
}

fn adder(int base) -> int {
    let g = fn (int x) use (base) => x + base;
    return g(1) + apply(g, 2);
}

fn twice(int n) -> int {
    return apply(fn (int x) => x * 2, n);
}

test("expression lambdas") {
    let k = 5;
    let add_k = fn (int x) use (k) => x + k;
    assert(add_k(1) == 6)?;
    assert(adder(10) == 23)?;
    assert(twice(21) == 42)?;
    let both = fn (int a, int b) use (k) => a * b - k;
    assert(both(3, 4) == 7)?;
    let label = fn (int x) => x.show();
    assert(label(9) == "9")?;
}

test("lambda in a file with a module const") {
    assert(stepped(5) == 17)?;
    assert(step(2) == 42)?;
}

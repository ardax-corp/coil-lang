// Calling a function with a prefix of its arguments makes a function value
// that takes the rest.
fn add3(int a, int b, int c) -> int {
    return a * 100 + b * 10 + c;
}

fn greet(string head, string name) -> string {
    return head + ", " + name;
}

class Box {
    pub n: int,
}

fn weigh(Box b, int k) -> int {
    return b.n * k;
}

fn twice(int x) -> int {
    return x * 2;
}

fn apply(int -> int f, int x) -> int {
    return f(x);
}

test("one argument filled") {
    let f = add3(1);
    assert(f(2, 3) == 123)?;
}

test("two arguments filled") {
    let g = add3(4, 5);
    assert(g(6) == 456)?;
    assert(g(0) == 450)?;
}

test("filled values are captured") {
    let hi = greet("hi");
    assert(hi("ada") == "hi, ada")?;
    let b = new Box(7);
    let w = weigh(b);
    assert(w(3) == 21)?;
}

test("a computed argument and a passed partial") {
    let h = add3(twice(2), 1);
    assert(apply(h, 9) == 419)?;
}

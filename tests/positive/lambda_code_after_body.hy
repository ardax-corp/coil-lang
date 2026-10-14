// A body that builds function values: each lambda's code runs in its own
// frame wherever the body's code is laid out, with heap captures and
// allocations on both sides of a collection, and a lambda inside a lambda.
use tests::assert;
use string::format;

class Cell {
    pub n: int,
}

fn apply(int -> int f, int x) -> int {
    return f(x);
}

fn adder(int k) -> int -> int {
    let cells = [new Cell(k), new Cell(k + 1)];
    let base = cells[0].n + cells[1].n;
    return fn (int x) use (base) => x + base;
}

fn nested(int k) -> int {
    let outer = fn (int x) {
        let twice = fn (int y) => y * 2;
        twice(x) + 1
    };
    let made = new Cell(outer(k));
    return made.n + apply(fn (int z) => z - 1, k);
}

fn churn(int n) -> int {
    let s = format("%v", n);
    let f = fn (int x) use (s) {
        let k = 0;
        if s == "123" {
            k = 3;
        }
        x + k
    };
    let i = 0;
    let acc = 0;
    while i < 20000 {
        let c = new Cell(i);
        acc = acc + f(c.n) - c.n;
        i = i + 1;
    }
    return acc;
}

test("a lambda keeps its capture past the body's allocations") {
    let f = adder(10);
    assert!(f(1) == 22);
    assert!(apply(adder(0), 5) == 6);
}

test("a lambda inside a lambda") {
    assert!(nested(4) == 12);
}

test("a heap capture survives collections in the loop") {
    assert!(churn(123) == 60000);
}

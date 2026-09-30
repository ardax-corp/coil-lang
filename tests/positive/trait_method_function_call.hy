// Function-style trait calls `method(x, …)` at a ground type select the
// instance like `x.method(…)`; a free fn of the same name still wins (#522).

class Node {
    pub i: int,
}

class Config {
    pub port: int,
}

trait One<T> {
    fn one(T x) -> int {}
}

impl One for int {
    pub fn one(int x) -> int {
        return x + 1;
    }
}

impl One for Config {
    pub fn one(Config x) -> int {
        return x.port + 1;
    }
}

trait Two<T> {
    fn two(T x, Node n) -> int {}
}

impl Two for Config {
    pub fn two(Config x, Node n) -> int {
        return x.port + n.i;
    }
}

test("ground ufcs, int, one param") {
    assert(one(41) == 42)?;
}

test("ground ufcs, class, one param") {
    assert(one(new Config(41)) == 42)?;
}

test("ground ufcs, class, two params") {
    assert(two(new Config(1), new Node(4)) == 5)?;
}

// Free fn declared before the trait and impl that reuse its name.
fn early(int n) -> int {
    return n * 2;
}

trait Named<T> {
    fn early(T x) -> int {}
    fn late(T x) -> int {}
}

impl Named for Config {
    pub fn early(Config x) -> int {
        return x.port + 100;
    }

    pub fn late(Config x) -> int {
        return x.port + 200;
    }
}

// Free fn declared after them.
fn late(int n) -> int {
    return n * 3;
}

test("free fn declared before wins over the trait method") {
    assert(early(5) == 10)?;
    assert(new Config(1).early() == 101)?;
}

test("free fn declared after wins over the trait method") {
    assert(late(5) == 15)?;
    assert(new Config(1).late() == 201)?;
}

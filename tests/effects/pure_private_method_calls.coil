// A function that makes an object and calls methods that mutate `self`
// only on that object stays pure: the writes are invisible to its callers
// (design §3.7, #1648). So does passing its own `Vec` to a function that
// pushes onto its parameter, and returning a `readonly` view.
use tests::assert;

class Counter {
    pub n: int,
    pub log: Vec<int>,
}

impl Counter {
    pub fn bump() {
        self.n = self.n + 1;
    }

    pub fn add(int k) {
        self.n = self.n + k;
        self.log.push(k);
    }

    pub fn twice() {
        self.bump();
        self.bump();
    }

    pub fn total() -> int {
        return self.n;
    }
}

fn fill(Vec<int> xs, int n) {
    for i in 0..n {
        xs.push(i);
    }
}

pure fn counted(int k) -> int {
    let c = new Counter(0, Vec::new());
    c.bump();
    c.add(k);
    c.twice();
    return c.total() + c.log.len();
}

pure fn filled(int n) -> int {
    let xs: Vec<int> = Vec::new();
    fill(xs, n);
    return xs.len();
}

pure fn frozen(int k) -> readonly Counter {
    let c = new Counter(k, Vec::new());
    c.add(2);
    return c;
}

// Not declared: inferred pure, so the loop below may split across tasks.
fn score(int i) -> int {
    let c = new Counter(i, Vec::new());
    c.twice();
    return c.total();
}

test("methods that mutate a private object") {
    assert!(counted(5) == 9);
    assert!(filled(4) == 4);
    let c = frozen(1);
    assert!(c.n == 3);
    assert!(c.log.len() == 1);
}

test("a loop over a function that mutates its own object") {
    let acc = 0;
    for i in 0..100000 {
        acc = acc + score(i);
    }
    assert!(acc == 5000150000);
}

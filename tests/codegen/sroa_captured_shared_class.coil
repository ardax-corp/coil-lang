// A `shared class` local whose field an inlined method writes, then
// captured by a lambda: it stays one heap object (no frame-slot box point
// for a captured local), and the lambda sees the writes.
use tests::assert;

shared class Counter {
    pub n: int,
}

impl Counter {
    pub fn add() {
        self.n += 1;
        self.spin();
    }

    pub fn spin() {
        let k = 0;
        while k < self.n {
            k++;
        }
    }
}

test("captured after an inlined field write") {
    let c = new Counter(0);
    c.add();
    let f = fn () use (c) => c.n;
    assert!(f() == 1);
    c.add();
    assert!(f() == 2);
}

// `readonly` seals a value against external writes only: field reads and
// `len` work, the variable can be rebound to another value, and an `impl`
// method may still mutate the object through `self`. Index and for-in
// reads are in readonly_array_reads.hy.
use tests::assert;

class Counter {
    pub n: int,
}

impl Counter {
    pub fn bump() -> int {
        self.n = self.n + 1;
        return self.n;
    }
}

test("field reads and len") {
    let xs = readonly [3, 4, 5];
    assert!(len(xs) == 3);
    let c = readonly new Counter(7);
    assert!(c.n == 7);
}

test("rebinding a readonly variable") {
    let a = readonly [1];
    assert!(len(a) == 1);
    a = readonly [2];
    assert!(len(a) == 1);
    let c = readonly new Counter(1);
    c = readonly new Counter(5);
    assert!(c.n == 5);
}

test("methods mutate through self") {
    let c = readonly new Counter(0);
    assert!(c.bump() == 1);
    assert!(c.bump() == 2);
    assert!(c.n == 2);
}

test("typeof shows the readonly type") {
    let xs = readonly [1, 2];
    assert!((typeof xs) == "readonly [int; 2]");
}

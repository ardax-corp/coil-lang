// `readonly` seals a value: field reads, `len` and methods that only read
// work, and the variable can be rebound to another value. A method that
// mutates `self` cannot be called on it (compile_fail/readonly_method_call.hy).
// Index and for-in reads are in readonly_array_reads.hy.
use tests::assert;

class Counter {
    pub n: int,
}

impl Counter {
    pub fn bump() -> int {
        self.n = self.n + 1;
        return self.n;
    }

    pub fn peek() -> int {
        return self.n;
    }
}

test("field reads and len") {
    let xs = readonly [3, 4, 5];
    assert!(xs.len() == 3);
    let c = readonly new Counter(7);
    assert!(c.n == 7);
}

test("rebinding a readonly variable") {
    let a = readonly [1];
    assert!(a.len() == 1);
    a = readonly [2];
    assert!(a.len() == 1);
    let c = readonly new Counter(1);
    c = readonly new Counter(5);
    assert!(c.n == 5);
}

test("methods that only read") {
    let m = new Counter(0);
    let c = readonly m;
    assert!(m.bump() == 1);
    assert!(c.peek() == 1);
    assert!(c.n == 1);
}

test("typeof shows the readonly type") {
    let xs = readonly [1, 2];
    assert!((typeof xs) == "readonly [int; 2]");
}

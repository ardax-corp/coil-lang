// A generic class's per-type method clone (`Slot::get$inst$string`) reads
// and writes its `T` fields through `self`, which is typed as the bare
// class: the field's word comes from the clone's ground type.
use tests::assert;

class Slot<T> {
    item: T,
    count: int,
}

impl Slot<T> {
    pub static fn new(T item) -> Slot<T> {
        return new Slot(item, 0);
    }

    pub fn get() -> T {
        return self.item;
    }

    pub fn set(T v) {
        self.item = v;
        self.count = self.count + 1;
    }

    pub fn swap(T v) -> T {
        let old = self.item;
        self.item = v;
        return old;
    }

    pub fn pair(int n) -> (T, int) {
        return (self.item, n + self.count);
    }
}

fn join(string a, string b) -> string {
    return a + b;
}

fn churn(int n) -> int {
    let total = 0;
    let s = "x";
    for i in 0..n {
        s = join("x", s);
        total = total + i;
    }
    return total + s.len();
}

test("int slot") {
    let s = Slot::new(3);
    assert!(s.get() == 3);
    s.set(8);
    assert!(s.swap(9) == 8);
    assert!(s.get() == 9);
    let p = s.pair(1);
    assert!(p[0] == 9 && p[1] == 2);
}

test("string slot across allocations") {
    let s = Slot::new(join("a", "1"));
    assert!(churn(200) > 0);
    assert!(s.get() == "a1");
    s.set(join("b", "2"));
    assert!(churn(200) > 0);
    assert!(s.swap("c") == "b2");
    assert!(s.get() == "c");
    assert!(s.pair(4)[0] == "c");
}

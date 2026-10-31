// Class `static` fields are one shared slot, read and written as
// `Class::field`; a `static fn` has no `self` and is called as `Class::f()`.
use tests::assert;

class Counter {
    static count: int = 0,
    static label: string = "c",
    pub value: int,
}

impl Counter {
    pub static fn fresh() -> Counter {
        Counter::count = Counter::count + 1;
        return new Counter(Counter::count * 10);
    }

    pub fn tag() -> string {
        return Counter::label + "#" + "x";
    }
}

test("static fields are shared by every instance") {
    let a = Counter::fresh();
    let b = Counter::fresh();
    assert!(a.value == 10);
    assert!(b.value == 20);
    assert!(Counter::count == 2);
}

test("static fields are writable from outside") {
    Counter::label = "k";
    let c = new Counter(0);
    assert!(c.tag() == "k#x");
    Counter::count = 100;
    assert!(Counter::fresh().value == 1010);
}

// A class field of a mixed-type tuple, held by a plain or generic class,
// is read, indexed and replaced (#1170).
use tests::assert;

class Holder {
    pub value: (int, string),
}

class Cell<T> {
    pub value: T,
}

impl Cell<T> {
    pub fn get() -> T {
        return self.value;
    }
}

test("class with a mixed tuple field") {
    let t = new Holder((1, "x"));
    assert!(t.value[0] == 1);
    assert!(t.value[1] == "x");
}

test("mixed tuple field read into a local") {
    let t = new Holder((7, "seven"));
    let v = t.value;
    assert!(v[0] + 1 == 8);
    assert!(v[1] == "seven");
}

test("mixed tuple field replaced") {
    let t = new Holder((1, "a"));
    t.value = (2, "b");
    assert!(t.value[0] == 2);
    assert!(t.value[1] == "b");
}

test("generic class at a mixed tuple") {
    let c = new Cell((1, "x"));
    let v = c.get();
    assert!(v[0] == 1);
    assert!(v[1] == "x");
    assert!(c.value[1] == "x");
}

test("generic class at a pair tuple") {
    let c = new Cell((3, 4));
    let v = c.get();
    assert!(v[0] + v[1] == 7);
    assert!(c.value[0] == 3);
}

// #1079: an array literal passed to a class constructor coerces to a
// `Vec<T>` field, the same way it does for a function argument.
use tests::assert;

class Names {
    pub names: Vec<string>,
}

class Bag<T> {
    pub items: Vec<T>,
}

test("positional array literal to Vec field") {
    let c = new Names(["a", "b"]);
    assert!(c.names[0] == "a");
    assert!(c.names[1] == "b");
    assert!(c.names.len() == 2);
}

test("coerced field is a growable Vec") {
    let c = new Names(["a"]);
    c.names.push("b");
    assert!(c.names.len() == 2);
    assert!(c.names[1] == "b");
}

test("generic class Vec field") {
    let b = new Bag([1, 2, 3]);
    assert!(b.items.len() == 3);
    assert!(b.items[2] == 3);
}

// A write through a field of a shared object copies the field's value
// first (one in-place op per guarded field), and only when it is shared.
use tests::assert;

class Bag {
    pub items: Vec<int>,
    pub names: Vec<string>,
}

impl Bag {
    pub fn add(int x) {
        self.items.push(x);
        self.names.push(x.show());
    }
}

test("a copy's field writes leave the original") {
    let a = new Bag(Vec::from([1]), Vec::new());
    let b = a;
    b.add(2);
    assert!(a.items.len() == 1 && a.names.len() == 0);
    assert!(b.items.len() == 2 && b.names[0] == "2");
}

test("a field shared with a local is copied on write") {
    let a = new Bag(Vec::from([1]), Vec::new());
    let kept = a.items;
    a.add(5);
    assert!(kept.len() == 1);
    assert!(a.items.len() == 2 && a.items[1] == 5);
}

test("an unshared field is written in place, many times") {
    let a = new Bag(Vec::new(), Vec::new());
    let i = 0;
    while i < 1000 {
        a.add(i);
        i += 1;
    }
    assert!(a.items.len() == 1000 && a.items[999] == 999);
    assert!(a.names[10] == "10");
}

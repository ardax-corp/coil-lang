// An inherent `impl Store<T: Eq>` gives a generic class methods that use
// the bound; the class works at every element type with an instance,
// including a derived one.
use tests::assert;

class Store<T> {
    pub items: Vec<T>,
}

impl Store<T: Eq> {
    pub fn add(T x) -> bool {
        if self.has(x) {
            return false;
        }
        self.items.push(x);
        return true;
    }

    pub fn has(T x) -> bool {
        for v in self.items {
            if v == x {
                return true;
            }
        }
        return false;
    }

    pub fn size() -> int {
        return self.items.len();
    }
}

fn empty_ints() -> Vec<int> {
    let v: Vec<int> = Vec::new();
    return v;
}

fn empty_strings() -> Vec<string> {
    let v: Vec<string> = Vec::new();
    return v;
}

#[derive(Eq)]
class Key {
    pub a: int,
    pub b: string,
}

test("a set of ints") {
    let s = new Store(empty_ints());
    assert!(s.add(1));
    assert!(s.add(2));
    assert!(!s.add(1));
    assert!(s.size() == 2);
    assert!(s.has(2) && !s.has(3));
}

test("a set of strings") {
    let s = new Store(empty_strings());
    s.add("x");
    s.add("x");
    s.add("y");
    assert!(s.size() == 2);
}

test("a set of a derived Eq class compares by value") {
    let ks: Vec<Key> = Vec::new();
    let s = new Store(ks);
    assert!(s.add(new Key(1, "a")));
    assert!(!s.add(new Key(1, "a")));
    assert!(s.add(new Key(1, "b")));
    assert!(s.size() == 2);
}

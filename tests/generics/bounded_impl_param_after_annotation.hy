// A bounded inherent impl's parameter is the same `K` in every method: a
// sibling that annotates a local `Vec<K>` does not split it, so `==` on `K`
// in a later method still goes through the `Eq` dictionary (#1277).
use tests::assert;

class Bag<K, V> {
    pub keys: Vec<K>,
    pub vals: Vec<V>,
}

impl Bag<K, V> {
    pub static fn new() -> Bag<K, V> {
        let keys: Vec<K> = Vec::new();
        let vals: Vec<V> = Vec::new();
        return new Bag(keys, vals);
    }
}

impl Bag<K: Eq, V> {
    fn scratch() -> int {
        let ks: Vec<K> = Vec::new();
        return ks.len();
    }

    pub fn first_is(K k) -> bool {
        return self.keys[0] == k;
    }

    pub fn index_of(K k) -> int {
        let i = 0;
        while i < self.keys.len() {
            if self.keys[i] == k {
                return i;
            }
            i = i + 1;
        }
        return 0 - 1;
    }
}

test("== on the impl parameter after a sibling annotates Vec<K>") {
    let b = Bag::new();
    b.keys.push(1);
    b.keys.push(2);
    assert!(b.first_is(1));
    assert!(!b.first_is(2));
    assert!(b.index_of(2) == 1);
    assert!(b.index_of(3) == -1);
}

test("the same with string keys") {
    let b = Bag::new();
    b.keys.push("a");
    b.vals.push(1);
    assert!(b.first_is("a"));
    assert!(b.index_of("b") == -1);
}

// A two-word `Option` / `Result` built in place (`Some(1)`, `None`,
// `Ok(2)`) passed as a bare type parameter of a bounded impl's method goes
// boxed to its enum object, as a local of the same type does. Direct MIR
// makes the call instead of leaving the body to the stack lowering (#1861).
use tests::assert;

class Slot<K, V> {
    pub k: K,
    pub v: V,
}

impl Slot<K: Eq, V> {
    pub fn put(K k, V x) -> bool {
        if self.k == k {
            self.v = x;
            return true;
        }
        return false;
    }

    pub fn set(V x) {
        self.v = x;
    }

    pub fn get(K k, V fallback) -> V {
        if self.k == k {
            return self.v;
        }
        return fallback;
    }
}

test("an Option<int> constructor as a bounded method's value") {
    let s: Slot<string, Option<int>> = new Slot("a", None);
    assert!(s.put("a", Some(1)));
    assert!(!s.put("b", Some(2)));
    assert!(s.get("a", None) == Some(1));
    assert!(s.get("b", None) == None);
    s.set(None);
    assert!(s.get("a", Some(9)) == None);
}

test("a Result<int, string> constructor as a bounded method's value") {
    let s: Slot<string, Result<int, string>> = new Slot("a", Err("none"));
    s.put("a", Ok(2));
    assert!(s.get("a", Err("no")) == Ok(2));
    assert!(s.get("b", Err("no")) == Err("no"));
}

test("an Option<float> constructor as a bounded method's value") {
    let s: Slot<int, Option<float>> = new Slot(1, None);
    s.set(Some(1.5));
    assert!(s.get(1, None) == Some(1.5));
    assert!(show(s.get(2, None)) == "None");
}

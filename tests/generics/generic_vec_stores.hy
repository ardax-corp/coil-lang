// A generic body stores its type parameter's value into a `Vec<K>` slot
// (`xs[i] = k`) and pushes it: the word moves as it is, so the body
// (`HashMap::insert` among them) lowers to direct MIR.
use tests::assert;
use collections::map::HashMap;
use string::format;

class Slots<T> {
    pub items: Vec<T>,
}

impl Slots<T> {
    pub fn put(int i, T v) {
        if i < self.items.len() {
            self.items[i] = v;
            return;
        }
        self.items.push(v);
    }
}

fn set_at<T>(Vec<T> xs, int i, T v) {
    xs[i] = v;
}

test("generic slot stores") {
    let s: Slots<string> = new Slots(Vec::new());
    s.put(0, "a");
    s.put(0, "b");
    s.put(1, "c");
    assert!(s.items[0] == "b" && s.items[1] == "c");
    let xs: Vec<int> = Vec::new();
    xs.push(1);
    xs.push(2);
    set_at(xs, 1, 9);
    assert!(xs[1] == 9);
}

test("HashMap insert") {
    let m: HashMap<string, int> = HashMap::new();
    let i = 0;
    while i < 40 {
        assert!(m.insert(format("k%v", i), i));
        i = i + 1;
    }
    assert!(!m.insert("k3", 30));
    assert!(m.get_or("k3", 0) == 30 && m.get_or("k39", 0) == 39 && m.len() == 40);
}

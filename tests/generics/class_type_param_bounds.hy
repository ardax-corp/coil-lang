// A bound on a class's type parameter (`class Sorted<T: Ord>`) is checked
// at `new` and shared by every inherent `impl Sorted<T>`, so its methods
// compare `T` through the `Ord` dictionary without restating the bound. A
// static constructor on a bounded impl (`impl Heap<T: Ord>`) is usable
// when the annotation or a later use pins `T` (#2085).
use tests::assert;

class Sorted<T: Ord> {
    pub items: Vec<T>,
}

impl Sorted<T> {
    pub static fn empty() -> Sorted<T> {
        return new Sorted(Vec::new());
    }

    pub fn max() -> T {
        let m = self.items[0];
        for x in self.items {
            if x > m {
                m = x;
            }
        }
        return m;
    }

    pub fn add(T x) {
        self.items.push(x);
    }
}

class Heap<T> {
    items: Vec<T>,
}

impl Heap<T: Ord> {
    pub static fn new() -> Heap<T> {
        return new Heap(Vec::new());
    }

    pub fn push(T x) {
        self.items.push(x);
    }

    pub fn top() -> T {
        let m = self.items[0];
        for x in self.items {
            if x < m {
                m = x;
            }
        }
        return m;
    }

    pub fn size() -> int {
        return self.items.len();
    }
}

test("the class bound is usable in an inherent impl") {
    let s = new Sorted(Vec::from([3, 1, 2]));
    assert!(s.max() == 3);
    let t = new Sorted(Vec::from(["b", "c", "a"]));
    assert!(t.max() == "c");
}

test("a class-bounded static constructor pinned by the annotation") {
    let s: Sorted<int> = Sorted::empty();
    s.add(4);
    s.add(9);
    assert!(s.max() == 9);
}

test("a bounded impl's static constructor pinned by the annotation") {
    let h: Heap<int> = Heap::new();
    h.push(5);
    h.push(2);
    h.push(8);
    assert!(h.size() == 3);
    assert!(h.top() == 2);
}

test("a bounded impl's static constructor pinned by a later use") {
    let h = Heap::new();
    h.push(2.5);
    h.push(1.5);
    assert!(h.top() == 1.5);
}

// A generic class `Cell<T>` with an inherent `impl Cell<T>`: one declaration
// used at several element types, with methods that read and write `T`.
use tests::assert;

class Cell<T> {
    pub value: T,
}

impl Cell<T> {
    pub fn get() -> T {
        return self.value;
    }

    pub fn set(T v) {
        self.value = v;
    }

    pub fn swap(T v) -> T {
        let old = self.value;
        self.value = v;
        return old;
    }
}

test("int cell") {
    let c = new Cell(3);
    c.set(c.get() + 1);
    assert!(c.value == 4);
    assert!(c.swap(9) == 4);
    assert!(c.get() == 9);
}

test("string cell") {
    let s = new Cell("a");
    s.set(s.get() + "b");
    assert!(s.get() == "ab");
}

test("cell of a tuple and a cell of cells") {
    let t = new Cell((1, 2));
    assert!(t.get()[1] == 2);
    t.set((3, 4));
    assert!(t.get() == (3, 4));
    let outer = new Cell(new Cell(5));
    outer.get().set(6);
    assert!(outer.value.value == 6);
}

test("annotated cell type") {
    let c: Cell<float> = new Cell(1.5);
    c.set(c.get() * 2.0);
    assert!(c.get() == 3.0);
}

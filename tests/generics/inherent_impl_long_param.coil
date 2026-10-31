// An inherent impl's type parameters may have any name, as on the class:
// `impl Cell<Elem>` is the generic impl, and a class named like a trait
// still takes an inherent impl.
use tests::assert;

class Cell<Elem> {
    pub v: Elem,
}

impl Cell<Elem> {
    pub fn get() -> Elem {
        return self.v;
    }
}

class Show {
    pub n: int,
}

impl Show {
    pub fn twice() -> int {
        return self.n * 2;
    }
}

test("multi-letter inherent impl parameter") {
    assert!(new Cell(3).get() == 3);
    assert!(new Cell("s").get() == "s");
}

test("class named like a trait") {
    assert!(new Show(4).twice() == 8);
}

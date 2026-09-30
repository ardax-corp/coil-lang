// Attribute macros from `examples/src/derive_macros.hy` on a class and a method.
use derive_macros::{with_describe, twice};

#[with_describe(prefix = "shape")]
class Point {
    pub x: int,
    pub y: int,
}

class Counter {
    pub n: int,
}

impl Counter {
    #[twice]
    pub fn get() -> int {
        return self.n;
    }
}

test("attribute macro on a class keeps it and adds methods") {
    let p = new Point(1, 2);
    assert(p.x + p.y == 3)?;
    assert(p.describe() == "shape:Point")?;
}

test("attribute macro on a method") {
    let c = new Counter(21);
    assert(c.get() == 21)?;
    assert(c.get_twice() == 42)?;
}

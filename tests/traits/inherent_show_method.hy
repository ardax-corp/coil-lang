// An inherent method named like a prelude trait method (`show`) is called
// as itself; the class needs no `Show` instance for it.
use tests::assert;

class Point {
    pub x: int,
}

impl Point {
    pub fn show() -> string {
        return "point";
    }
}

test("inherent show wins") {
    let p = new Point(1);
    let s = p.show();
    assert!(s == "point");
}

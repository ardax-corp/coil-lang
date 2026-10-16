// A class local read and written in a loop, through fields and methods,
// keeps its values (the compiler may keep such fields in slots).
use tests::assert;

class Point {
    pub x: int,
    pub y: int,
}

impl Point {
    pub fn sum() -> int {
        return self.x + self.y;
    }

    pub fn twice_x() -> int {
        return self.x + self.x;
    }
}

fn bump(int n) -> int {
    let p = new Point(0, 1);
    let i = 0;
    while i < n {
        p.x = p.x + p.y;
        p.y = p.y + 1;
        i = i + 1;
    }
    return p.x + p.y;
}

fn hot() -> int {
    let p = new Point(3, 4);
    let acc = 0;
    let i = 0;
    while i < 200000 {
        acc = acc + p.sum();
        acc = acc + p.twice_x();
        acc = acc + p.x + p.y;
        i = i + 1;
    }
    return acc;
}

test("field stores in a loop") {
    // x = 21, y = 7
    assert!(bump(6) == 28);
}

test("repeated field and method reads") {
    assert!(hot() == 4000000);
}

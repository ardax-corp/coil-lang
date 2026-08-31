// Classes: construction, fields, methods, mutation.
class Counter {
    pub value: int,
}

impl Counter {
    pub fn inc() {
        self.value = self.value + 1;
    }

    pub fn add(int n) {
        self.value = self.value + n;
    }

    pub fn get() -> int {
        return self.value;
    }
}

class Point {
    pub x: int,
    pub y: int,
}

impl Point {
    pub fn manhattan() -> int {
        return self.x + self.y;
    }
}

test("construct and read field") {
    let c = new Counter(10);
    assert(c.value == 10)?;
}

test("method mutates self") {
    let c = new Counter(0);
    c.inc();
    c.inc();
    assert(c.get() == 2)?;
}

test("method with args") {
    let c = new Counter(5);
    c.add(7);
    assert(c.get() == 12)?;
}

test("multi-field class") {
    let p = new Point(3, 4);
    assert(p.x == 3)?;
    assert(p.y == 4)?;
    assert(p.manhattan() == 7)?;
}

test("field assignment on instance") {
    let p = new Point(1, 2);
    p.x = 10;
    p.y += 5;
    assert(p.x == 10)?;
    assert(p.y == 7)?;
}

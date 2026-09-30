// Module-qualified paths resolve without a `use` of the item, so generated
// code (derive expansions) can name `module::Item` whatever the user imports.
// Only `marker` is imported: it pulls `qpath_provider` (and, through it,
// `qpath_provider::inner`) into the compile. Everything else is qualified.
use qpath_provider::marker;

class Holder {
    pub point: qpath_provider::Point,
    pub boxed: qpath_provider::Box<int>,
}

fn sum_of(qpath_provider::Point p) -> int {
    return p.sum();
}

fn make_point(int x, int y) -> qpath_provider::Point {
    return new qpath_provider::Point(x, y);
}

fn shape_side(qpath_provider::Shape s) -> int {
    return match s {
        qpath_provider::Shape::Circle(r) => r,
        qpath_provider::Shape::Square(side) => side,
        qpath_provider::Shape::Empty => 0,
    };
}

trait Weight<T> {
    fn weight(T x) -> int {}
}

impl Weight for qpath_provider::Point {
    pub fn weight(qpath_provider::Point p) -> int {
        return p.x * 10 + p.y;
    }
}

test("imported item still resolves") {
    assert(marker() == 7)?;
}

test("qualified free function calls") {
    assert(qpath_provider::double(21) == 42)?;
    assert(qpath_provider::nine() == 9)?;
    assert(qpath_provider::pick(1, 2) == 2)?;
    assert(qpath_provider::pick("a", "b") == "b")?;
}

test("qualified class construction and static method") {
    let p = new qpath_provider::Point(3, 4);
    assert(p.sum() == 7)?;
    let o = qpath_provider::Point::origin();
    assert(o.sum() == 0)?;
}

test("qualified types in annotations, params, returns and fields") {
    let p: qpath_provider::Point = make_point(1, 2);
    assert(sum_of(p) == 3)?;
    let b: qpath_provider::Box<int> = new qpath_provider::Box(5);
    assert(b.get() == 5)?;
    let h = new Holder(p, b);
    assert(h.point.sum() == 3)?;
    assert(h.boxed.get() == 5)?;
    let m: qpath_provider::Meters = 12;
    assert(m == 12)?;
}

test("qualified types as generic arguments") {
    let o: Option<qpath_provider::Point> = Option::Some(new qpath_provider::Point(2, 2));
    let n = match o {
        Option::Some(p) => p.x + p.y,
        Option::None => -1,
    };
    assert(n == 4)?;
}

test("qualified enum constructors and patterns") {
    let s: qpath_provider::Shape = qpath_provider::Shape::Square(3);
    assert(s.area() == 9)?;
    assert(shape_side(s) == 3)?;
    assert(shape_side(qpath_provider::Shape::Circle(2)) == 2)?;
    let e = qpath_provider::Shape::Empty;
    assert(e.area() == 0)?;
}

test("trait impl for a qualified class") {
    let p = new qpath_provider::Point(4, 2);
    assert(p.weight() == 42)?;
}

test("three-segment module paths") {
    assert(qpath_provider::inner::triple(2) == 6)?;
    let t: qpath_provider::inner::Tag = qpath_provider::inner::Tag::named("a");
    assert(t.label == "a")?;
    let u = new qpath_provider::inner::Tag("b");
    assert(u.label == "b")?;
    let level = qpath_provider::inner::Level::High(4);
    let n = match level {
        qpath_provider::inner::Level::High(v) => v,
        qpath_provider::inner::Level::Low => 0,
    };
    assert(n == 4)?;
}

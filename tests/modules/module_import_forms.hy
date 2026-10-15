// `use` forms: a module-file item (`math::add`), a brace group with `as`,
// classes, generic classes, enums and generic functions brought in by name or
// alias, and a nested module's items through a brace group.
use tests::assert;
use math::{add, mul as product, add as plus};
use qpath_provider::Point;
use qpath_provider::Box as Crate;
use qpath_provider::Shape;
use qpath_provider::pick;
use qpath_provider::double as twice;
use qpath_provider::inner::triple;
use qpath_provider::inner::Tag;
use qpath_provider::inner::Level;

fn area_of(Shape s) -> int {
    return s.area();
}

test("a module-file item and its aliases") {
    assert!(add(2, 3) == 5);
    assert!(plus(2, 3) == 5);
    assert!(product(4, 5) == 20);
    assert!(twice(21) == 42);
}

test("an imported class: constructor, static and instance methods") {
    let p = new Point(2, 3);
    assert!(p.sum() == 5);
    assert!(Point::origin().sum() == 0);
    p.x = 10;
    assert!(p.sum() == 13);
}

test("a generic class under an alias") {
    let b: Crate<string> = new Crate("in");
    assert!(b.get() == "in");
    let n = new Crate(7);
    assert!(n.get() + 1 == 8);
}

test("an imported enum: constructors, methods and patterns") {
    assert!(area_of(Shape::Square(3)) == 9);
    assert!(area_of(Shape::Circle(1)) == 3);
    let s = Shape::Empty;
    let name = match s {
        Shape::Circle(_) => "circle",
        Shape::Square(_) => "square",
        Shape::Empty => "empty",
    };
    assert!(name == "empty");
}

test("an imported generic function") {
    assert!(pick(1, 2) == 2);
    assert!(pick("a", "b") == "b");
}

test("items of a nested module") {
    assert!(triple(4) == 12);
    assert!(Tag::named("t").label == "t");
    let l = Level::High(3);
    let v = match l {
        Level::Low => 0,
        Level::High(n) => n,
    };
    assert!(v == 3);
}

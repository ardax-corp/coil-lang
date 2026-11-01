// `%v` of a value whose type another module declares goes through that
// type's derived `Show`, not a raw address (#1223).
use tests::assert;
use string::format;
use imported_show_types::{fast, circle, pair, Speed, Shape};

test("an imported scalar enum shows its case") {
    assert!(format("%v", fast()) == "Speed::Fast");
    assert!(format("%v", Speed::Slow) == "Speed::Slow");
}

test("an imported payload enum shows its payload") {
    assert!(format("%v", circle(5)) == "Shape::Circle(5)");
    assert!(format("%v", Shape::Rect { w: 1, h: 2 }) == "Shape::Rect { w: 1, h: 2 }");
}

test("an imported generic class shows its fields") {
    assert!(format("%v", pair(4)) == "Pair { a: 4 }");
}

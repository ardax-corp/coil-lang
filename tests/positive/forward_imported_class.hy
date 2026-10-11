// A function declared after its caller may take and return an imported
// class or enum: its forward signature resolves `Rec` / `Shape` through the
// `use` (#888).

use forward_rec::{Rec, make, Shape, square};

fn twice(int n) -> int {
    let r = make(n);
    return later(r).n;
}

fn later(Rec r) -> Rec {
    return make(r.n * 2);
}

fn side(int n) -> int {
    return area(square(n));
}

fn area(Shape s) -> int {
    match s {
        Shape::Dot => {
            return 0;
        },
        Shape::Sq(n) => {
            return n * n;
        },
    }
}

test("later fn takes an imported enum") {
    assert(side(4) == 16)?;
    assert(area(Shape::Dot) == 0)?;
}

test("later fn takes and returns an imported class") {
    assert(twice(3) == 6)?;
    assert(later(make(5)).n == 10)?;
}

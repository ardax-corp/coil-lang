// A derive that implements a trait declared in the provider module.
use derive_macros::Summary;

#[derive(Summary)]
class Point {
    pub x: int,
    pub y: int,
}

test("generated impl of a package trait resolves as a method") {
    let p = new Point(1, 2);
    assert(p.summary() == "Point x=1 y=2")?;
}

test("generated impl of a package trait resolves function-style") {
    let p = new Point(3, 4);
    assert(summary(p) == "Point x=3 y=4")?;
}

// A derive that implements a trait declared in the provider module.
use derive_macros::Summary;

#[derive(Summary)]
class Point {
    pub x: int,
    pub y: int,
}

// On a generic type the derive writes `impl Summary for Pair<A: Show, B: Show>`.
#[derive(Summary)]
class Pair<A, B> {
    pub a: A,
    pub b: B,
}

test("generated impl of a package trait resolves as a method") {
    let p = new Point(1, 2);
    assert(p.summary() == "Point x=1 y=2")?;
}

test("generated impl of a package trait resolves function-style") {
    let p = new Point(3, 4);
    assert(summary(p) == "Point x=3 y=4")?;
}

test("user derive on a generic type (bounded instance)") {
    let s1 = new Pair(1, "z").summary();
    assert(s1 == "Pair a=1 b=z", s1)?;
    // `Point` has the default `Show` (its type name).
    let s2 = new Pair(new Point(1, 2), 3).summary();
    assert(s2 == "Pair a=Point b=3", s2)?;
}

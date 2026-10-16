// Helper module for tests/modules/imported_types_show.hy (#1223): derived
// `Show` on a scalar enum, a payload enum and a generic class.

#[derive(Show, Eq)]
enum Speed {
    Fast,
    Slow,
}

#[derive(Show)]
enum Shape {
    Circle(int),
    Rect { w: int, h: int },
}

#[derive(Show)]
class Pair<T> {
    pub a: T,
}

fn fast() -> Speed {
    return Speed::Fast;
}

fn circle(int r) -> Shape {
    return Shape::Circle(r);
}

fn pair(int n) -> Pair<int> {
    return new Pair(n);
}

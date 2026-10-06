// A `new` object as a later call argument: every argument stages through
// a temp, so the earlier ones survive the constructor's own temp.
class Point {
    pub x: int,
}

class Val {
    pub v: int,
}

fn pick(int a, Point p, Val v) -> int {
    return a + p.x * 10 + v.v * 100;
}

fn sum_xs(Point a, Point b) -> int {
    return a.x + b.x;
}

fn nested() -> int {
    return sum_xs(new Point(1), new Point(sum_xs(new Point(2), new Point(3))));
}

test("new objects as call arguments") {
    assert(pick(1, new Point(2), new Val(3)) == 321)?;
    let n = 4;
    assert(pick(n, new Point(n + 1), new Val(n * 2)) == 854)?;
    assert(nested() == 6)?;
    assert(sum_xs(new Point(7), new Point(8)) + 1 == 16)?;
}

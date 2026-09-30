// Trait methods with more than one parameter called through a generic
// bound: the monomorphized direct CALL boxes only the parameters the trait
// types as `T` (what the concrete entry unboxes), not `int k` / `Val v` (#521).

class Val {
    pub i: int,
}

class Point {
    pub x: int,
}

trait Probe<T> {
    fn two(T proto, int k) -> int {}
    fn two_obj(T proto, Val v) -> int {}
    fn mk(T proto, int k) -> T {}
}

impl Probe for Point {
    pub fn two(Point proto, int k) -> int {
        return k;
    }

    pub fn two_obj(Point proto, Val v) -> int {
        return v.i;
    }

    pub fn mk(Point proto, int k) -> Point {
        return new Point(proto.x + k);
    }
}

fn g_two<T: Probe>(T p, int k) -> int {
    return p.two(k);
}

fn g_two_obj<T: Probe>(T p, Val v) -> int {
    return p.two_obj(v);
}

fn g_mk<T: Probe>(T p, int k) -> T {
    return p.mk(k);
}

test("int arg") {
    assert(g_two(new Point(4), 9) == 9)?;
}

test("object arg") {
    assert(g_two_obj(new Point(4), new Val(7)) == 7)?;
}

test("returns T") {
    assert(g_mk(new Point(4), 3).x == 7)?;
}

test("direct") {
    assert(new Point(4).two(9) == 9)?;
}

trait Combine<T> {
    fn combine(T a, int k, T b) -> int {}
    fn twice(T a, int k) -> int {
        return a.combine(k, a) * 2;
    }
    fn sum_all(T a, [T] xs) -> int {}
}

impl Combine for Point {
    pub fn combine(Point a, int k, Point b) -> int {
        return a.x + k + b.x;
    }

    pub fn sum_all(Point a, [Point] xs) -> int {
        let s = a.x;
        for p in xs {
            s = s + p.x;
        }
        return s;
    }
}

impl Combine for int {
    pub fn combine(int a, int k, int b) -> int {
        return a * 100 + k * 10 + b;
    }

    pub fn sum_all(int a, [int] xs) -> int {
        let s = a;
        for x in xs {
            s = s + x;
        }
        return s;
    }
}

fn g_combine<T: Combine>(T a, int k, T b) -> int {
    return a.combine(k, b);
}

fn g_twice<T: Combine>(T a, int k) -> int {
    return a.twice(k);
}

fn g_sum<T: Combine>(T a, [T] xs) -> int {
    return a.sum_all(xs);
}

test("T in first and third position") {
    assert(g_combine(new Point(1), 2, new Point(3)) == 6)?;
}

test("primitive instance") {
    assert(g_combine(1, 2, 3) == 123)?;
}

test("default method through the bound") {
    assert(g_twice(new Point(1), 5) == 14)?;
    assert(g_twice(4, 5) == 908)?;
}

test("array of T parameter") {
    assert(g_sum(new Point(1), [new Point(2), new Point(3)]) == 6)?;
    assert(g_sum(1, [2, 3]) == 6)?;
}

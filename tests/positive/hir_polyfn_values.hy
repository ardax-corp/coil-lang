// Generic functions as `PolyFn` values: a let-bound or `forall` local
// boxes its arguments, passes call-site dictionaries for a constrained
// source, and unboxes a type-parameter result.

trait Tagged<T> {
    fn tag_of(T x) -> int {}
}

impl Tagged for int {
    pub fn tag_of(int x) -> int {
        return x * 10;
    }
}

fn id<T>(T x) -> T {
    return x;
}

fn sum<T: Num>(T a, T b) -> T {
    return a + b;
}

fn tagged<T: Tagged>(T x) -> int {
    return tag_of(x);
}

fn twice(forall T. T -> T f, int x) -> int {
    return f(f(x));
}

fn capture<T: Tagged>(T _w) {
    return tagged;
}

test("let-bound generic at two types") {
    let f = id;
    assert(f(7) == 7)?;
    assert(f(2.5) == 2.5)?;
}

test("two-argument constrained generic") {
    let add = sum;
    assert(add(20, 22) == 42)?;
}

test("constrained generic with call-site evidence") {
    let t = tagged;
    assert(t(4) == 40)?;
}

test("forall parameter") {
    assert(twice(id, 5) == 5)?;
}

test("returned PolyFn with captured evidence") {
    let c = capture(0);
    assert(c(3) == 30)?;
}

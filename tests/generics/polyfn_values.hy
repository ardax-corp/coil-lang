// Generic functions as `PolyFn` values: a let-bound or `forall` local
// boxes its arguments, passes call-site dictionaries for a constrained
// source, and unboxes a type-parameter result.
use tests::assert;

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
    assert!(f(7) == 7);
    assert!(f(2.5) == 2.5);
}

test("two-argument constrained generic") {
    let add = sum;
    assert!(add(20, 22) == 42);
}

test("constrained generic with call-site evidence") {
    let t = tagged;
    assert!(t(4) == 40);
}

test("forall parameter") {
    assert!(twice(id, 5) == 5);
}

test("returned PolyFn with captured evidence") {
    let c = capture(0);
    assert!(c(3) == 30);
}

trait Convert<A, B> {
    fn cast(A x) -> B {}
}

impl Convert<int> for int {
    pub fn cast(int x) -> int {
        return x;
    }
}

fn convert_fn<A, B>(A x) -> B where Convert<A, B> {
    return cast(x);
}

fn capture_convert<A, B>(A _wa, B _wb) where Convert<A, B> {
    return convert_fn;
}

fn fib(int n) -> int {
    if n <= 2 {
        return 1;
    }
    return fib(n - 1) + fib(n - 2);
}

test("returned multi-parameter PolyFn keeps its evidence") {
    let f = capture_convert(0, 0);
    assert!(f(42) == 42);
}

test("PolyFn value applied to a recursive call") {
    let f = id;
    assert!(f(fib(6)) == 8);
}

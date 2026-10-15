// A `forall T. T -> T` parameter takes a polymorphic function, which the
// body may call at several types; a generic function identifier is
// accepted for it.
use tests::assert;

fn id<T>(T x) -> T {
    return x;
}

fn app(forall T. T -> T f, int x) -> int {
    return f(x);
}

fn both(forall T. T -> T f, int n, string s) -> string {
    return show(f(n)) + f(s);
}

test("a generic function for a forall parameter") {
    assert!(app(id, 5) == 5);
}

test("the parameter is used at two types") {
    assert!(both(id, 3, "x") == "3x");
}

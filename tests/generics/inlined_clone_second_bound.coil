// A small generic function's monomorphic clone inlined into its caller:
// a trait call on its second type parameter takes that parameter's
// instance, not the first's, whether called as `show(b)` or `b.show()`
// (#1479).
use tests::assert;

fn second<A: Show, B: Show>(A a, B b) -> string {
    return show(b);
}

fn second_method<A: Show, B: Show>(A a, B b) -> string {
    return b.show();
}

test("a trait call on the second type parameter of an inlined clone") {
    assert!(second(true, 2.5) == "2.5", second(true, 2.5));
    assert!(second(1, "two") == "two", second(1, "two"));
    assert!(second_method(true, 7) == "7", second_method(true, 7));
    assert!(second_method("a", false) == "false", second_method("a", false));
}

// A `const` or `static` holding a function value, named by a module path
// or as a class static field, is called directly (#1705).
use tests::assert;
use qualified_fn_value_provider;

fn sq(int x) -> int {
    return x * x;
}

class C {
    pub static f: fn(int) -> int = sq,
}

test("a module const function value called by its path") {
    assert!(qualified_fn_value_provider::TWICE(4) == 8);
    assert!(qualified_fn_value_provider::ADD(2, 5) == 7);
    assert!(qualified_fn_value_provider::TWICE(qualified_fn_value_provider::ADD(1, 2)) == 6);
}

test("a module static function value called by its path") {
    assert!(qualified_fn_value_provider::HOOK(6) == 12);
}

test("a class static function value called directly") {
    assert!(C::f(3) == 9);
    assert!(qualified_fn_value_provider::Ops::triple(3) == 9);
}

test("a call through the path matches one through a local") {
    let g = C::f;
    assert!(g(4) == C::f(4));
    C::f = fn (int x) => x - 1;
    assert!(C::f(4) == 3);
}

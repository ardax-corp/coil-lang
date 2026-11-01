// An imported `const` or `static` holding a function value is called
// directly by its name (#1685).
use tests::assert;
use const_fn_provider::{TWICE, HOOK};

test("an imported const function value called directly") {
    assert!(TWICE(4) == 8);
}

test("an imported static function value called directly") {
    assert!(HOOK(6) == 12);
}
